//! The local music folders this app pushes from.
//!
//! A "source" is a folder of audio files. It is walked recursively once, and after
//! that incrementally: a file whose size and mtime are unchanged keeps the tags read
//! the last time, so a 3000-file library rescans in a moment. The index lives beside
//! the device cache, which is what lets a sync plan be computed with nothing
//! plugged in.
//!
//! Nothing here touches USB or the UI: `sync.rs` turns an index plus the device's
//! track list into a plan, and the worker executes it.

use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::cache;
use crate::core::library::AUDIO_EXTS;
use crate::core::tags;

/// Bump when the on-disk shape changes in a way an older file cannot satisfy.
pub const INDEX_VERSION: u32 = 1;

/// How deep a walk goes. Deeper than any sane album layout, shallow enough that a
/// directory loop cannot run away.
const MAX_DEPTH: usize = 8;

// ── locations ───────────────────────────────────────────────────────────────

pub fn config_path() -> PathBuf {
    cache::data_dir().join("sync-config.json")
}

pub fn index_path() -> PathBuf {
    cache::data_dir().join("sync-index.json")
}

// ── the data ────────────────────────────────────────────────────────────────

/// The firmware's own name normalisation (lowercase, dots dropped) plus whitespace
/// folding.
///
/// Both halves are needed. The firmware drops dots (`01. Song` → `01 Song`), and it
/// also stores titles with padding intact — `10  吻别.mp3` really has two spaces,
/// because the tag's title starts with one. Folding runs of whitespace makes the two
/// sides comparable either way.
pub fn name_key(name: &str) -> String {
    let folded: String = name.split_whitespace().collect::<Vec<_>>().join(" ");
    tags::device_key(&folded)
}

/// One audio file on the Mac, with the tags as last read.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LocalEntry {
    pub path: PathBuf,
    pub size: u64,
    /// Unix seconds of the file's mtime; the cheap half of "has this changed".
    pub mtime: i64,
    pub title: String,
    #[serde(default)]
    pub artist: Option<String>,
    #[serde(default)]
    pub album: Option<String>,
    #[serde(default)]
    pub track: Option<u16>,
}

impl LocalEntry {
    pub fn artist(&self) -> &str {
        self.artist.as_deref().unwrap_or("")
    }

    pub fn album(&self) -> &str {
        self.album.as_deref().unwrap_or("")
    }

    /// The name the firmware would store this file under, normalised.
    pub fn stem_key(&self) -> String {
        let stem = self
            .path
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        name_key(&format!("{stem}.mp3"))
    }

    pub fn label(&self) -> String {
        match (self.title.trim(), self.artist().trim()) {
            ("", artist) => format!("{}", if artist.is_empty() { "?" } else { artist }),
            (title, "") => title.to_owned(),
            (title, artist) => format!("{title} · {artist}"),
        }
    }
}

/// Everything the sources hold, as of `scanned_at`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocalIndex {
    pub version: u32,
    pub scanned_at: i64,
    #[serde(default)]
    pub entries: Vec<LocalEntry>,
}

impl Default for LocalIndex {
    fn default() -> Self {
        Self { version: INDEX_VERSION, scanned_at: 0, entries: Vec::new() }
    }
}

/// The folders the user asked to sync from.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncConfig {
    pub version: u32,
    #[serde(default)]
    pub sources: Vec<PathBuf>,
}

impl Default for SyncConfig {
    fn default() -> Self {
        Self { version: INDEX_VERSION, sources: Vec::new() }
    }
}

/// What one scan did, for the log and the UI.
#[derive(Debug, Clone, Default)]
pub struct ScanStats {
    pub files: usize,
    /// Entries whose size and mtime matched the previous index: tags not re-read.
    pub unchanged: usize,
    pub read: usize,
    pub skipped_dirs: usize,
}

// ── persistence ─────────────────────────────────────────────────────────────

pub fn load_config() -> SyncConfig {
    let path = config_path();
    let Ok(text) = std::fs::read_to_string(&path) else {
        return SyncConfig::default();
    };
    match serde_json::from_str::<SyncConfig>(&text) {
        Ok(config) if config.version == INDEX_VERSION => config,
        Ok(_) => {
            log::debug!("sync-config.json is from another version — starting empty");
            SyncConfig::default()
        }
        Err(e) => {
            log::warn!("sync-config.json did not parse ({e}) — starting empty");
            SyncConfig::default()
        }
    }
}

pub fn save_config(config: &SyncConfig) -> Result<()> {
    write_json(&config_path(), config).context("writing the sync configuration")
}

pub fn load_index() -> Option<LocalIndex> {
    let text = std::fs::read_to_string(index_path()).ok()?;
    match serde_json::from_str::<LocalIndex>(&text) {
        Ok(index) if index.version == INDEX_VERSION => Some(index),
        Ok(_) => {
            log::debug!("sync-index.json is from another version — a full scan will replace it");
            None
        }
        Err(e) => {
            log::warn!("sync-index.json did not parse ({e}) — a full scan will replace it");
            None
        }
    }
}

pub fn save_index(index: &LocalIndex) -> Result<()> {
    write_json(&index_path(), index).context("writing the local index")
}

fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    let text = serde_json::to_string_pretty(value)?;
    let tmp = path.with_extension("json.part");
    std::fs::write(&tmp, text).with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("moving {} into place", tmp.display()))?;
    Ok(())
}

// ── scanning ────────────────────────────────────────────────────────────────

/// Every audio file under `dir`, recursively, in sorted order.
///
/// Hidden entries are skipped (`.Trash`, `.DS_Store` neighbours) and directories are
/// visited only once, so a symlink that points at an ancestor cannot loop. The list
/// is sorted so a plan reads in a stable order whatever the filesystem says.
pub fn walk_audio(dir: &Path, out: &mut Vec<PathBuf>, stats: &mut ScanStats) -> Result<()> {
    fn recurse(
        dir: &Path,
        depth: usize,
        out: &mut Vec<PathBuf>,
        stats: &mut ScanStats,
    ) -> Result<()> {
        if depth > MAX_DEPTH {
            stats.skipped_dirs += 1;
            return Ok(());
        }
        let Ok(entries) = std::fs::read_dir(dir) else {
            return Ok(()); // an unreadable folder is not a reason to fail the scan
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with('.') {
                continue;
            }
            match entry.file_type() {
                Ok(kind) if kind.is_dir() => recurse(&path, depth + 1, out, stats)?,
                Ok(kind) if kind.is_file() => {
                    if is_audio(&path) {
                        out.push(path);
                    }
                }
                // Symlinks are followed only when they resolve to a file, so a linked
                // album still counts but a linked directory cannot cycle.
                _ => {
                    if path.is_file() && is_audio(&path) {
                        out.push(path);
                    }
                }
            }
        }
        Ok(())
    }
    recurse(dir, 0, out, stats)?;
    out.sort();
    Ok(())
}

fn is_audio(path: &Path) -> bool {
    path.extension()
        .map(|e| AUDIO_EXTS.contains(&e.to_string_lossy().to_lowercase().as_str()))
        .unwrap_or(false)
}

/// Walk every source and build a fresh index, reusing tags for files that have not
/// changed since `previous` was written.
pub fn scan(sources: &[PathBuf], previous: Option<&LocalIndex>) -> Result<(LocalIndex, ScanStats)> {
    let mut stats = ScanStats::default();
    let mut files: Vec<PathBuf> = Vec::new();
    for source in sources {
        if !source.is_dir() {
            log::debug!("sync source {} is not a directory — skipped", source.display());
            continue;
        }
        walk_audio(source, &mut files, &mut stats)?;
    }
    files.sort();
    files.dedup();

    let cached: std::collections::HashMap<&Path, &LocalEntry> = previous
        .map(|index| index.entries.iter().map(|e| (e.path.as_path(), e)).collect())
        .unwrap_or_default();

    let mut entries = Vec::with_capacity(files.len());
    for path in files {
        let Ok(meta) = std::fs::metadata(&path) else { continue };
        let size = meta.len();
        let mtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        if let Some(old) = cached.get(path.as_path()) {
            if old.size == size && old.mtime == mtime {
                entries.push((*old).clone());
                stats.unchanged += 1;
                continue;
            }
        }
        entries.push(read_entry(&path, size, mtime));
        stats.read += 1;
    }
    stats.files = entries.len();
    entries.sort_by(|a, b| a.path.cmp(&b.path));
    Ok((
        LocalIndex {
            version: INDEX_VERSION,
            scanned_at: cache::now_unix(),
            entries,
        },
        stats,
    ))
}

fn read_entry(path: &Path, size: u64, mtime: i64) -> LocalEntry {
    let tags = tags::read(path);
    LocalEntry {
        path: path.to_path_buf(),
        size,
        mtime,
        title: tags.title,
        artist: tags.artist,
        album: tags.album,
        track: tags.track,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("zune-gui-sync-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_walk_finds_audio_recursively_and_skips_everything_else() {
        let root = temp_dir("walk");
        fs::create_dir_all(root.join("Artist/Album")).unwrap();
        fs::create_dir_all(root.join(".Trash")).unwrap();
        fs::write(root.join("Artist/Album/01 song.mp3"), b"x").unwrap();
        fs::write(root.join("Artist/Album/cover.jpg"), b"x").unwrap();
        fs::write(root.join("top.flac"), b"x").unwrap();
        fs::write(root.join(".Trash/old.mp3"), b"x").unwrap();
        fs::write(root.join(".DS_Store"), b"x").unwrap();

        let mut files = Vec::new();
        let mut stats = ScanStats::default();
        walk_audio(&root, &mut files, &mut stats).unwrap();
        let names: Vec<String> = files
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().to_string())
            .collect();
        assert_eq!(names, vec!["01 song.mp3", "top.flac"]);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_rescan_reuses_unchanged_entries() {
        let root = temp_dir("rescan");
        fs::write(root.join("one.mp3"), b"first").unwrap();
        let (first, stats) = scan(std::slice::from_ref(&root), None).unwrap();
        assert_eq!(stats.read, 1);
        assert_eq!(first.entries.len(), 1);

        let (second, stats) = scan(std::slice::from_ref(&root), Some(&first)).unwrap();
        assert_eq!(stats.read, 0, "unchanged file must not be re-read");
        assert_eq!(stats.unchanged, 1);
        assert_eq!(second.entries, first.entries);

        // Touching the file's size changes it, so the entry is rebuilt.
        std::thread::sleep(std::time::Duration::from_millis(10));
        fs::write(root.join("one.mp3"), b"second version").unwrap();
        let (third, stats) = scan(std::slice::from_ref(&root), Some(&second)).unwrap();
        assert_eq!(stats.read, 1);
        assert_eq!(third.entries[0].size, 14);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_missing_source_is_skipped_not_fatal() {
        let missing = PathBuf::from("/definitely/not/here");
        let (index, stats) = scan(&[missing], None).unwrap();
        assert!(index.entries.is_empty());
        assert_eq!(stats.files, 0);
    }

    #[test]
    fn config_and_index_round_trip() {
        let dir = temp_dir("json");
        let config = SyncConfig {
            version: INDEX_VERSION,
            sources: vec![dir.join("Music")],
        };
        let text = serde_json::to_string(&config).unwrap();
        let back: SyncConfig = serde_json::from_str(&text).unwrap();
        assert_eq!(back.sources, config.sources);

        let index = LocalIndex {
            version: INDEX_VERSION,
            scanned_at: 7,
            entries: vec![LocalEntry {
                path: dir.join("a.mp3"),
                size: 1,
                mtime: 2,
                title: "曲".to_owned(),
                artist: Some("歌手".to_owned()),
                album: None,
                track: Some(3),
            }],
        };
        let back: LocalIndex =
            serde_json::from_str(&serde_json::to_string(&index).unwrap()).unwrap();
        assert_eq!(back.entries.len(), 1);
        assert_eq!(back.entries[0].title, "曲");
        assert_eq!(back.entries[0].stem_key(), name_key("a.mp3"));
        let _ = fs::remove_dir_all(&dir);
    }
}
