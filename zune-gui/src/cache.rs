//! On-disk cache for a device's library.
//!
//! Why this exists: reading the real library is a USB round trip per object — a
//! 2425-track Zune takes **2–3 minutes**. That is fine once, and intolerable on
//! every launch while the UI is being built. So the first read is written down,
//! and every later launch reads the file instead of the device.
//!
//! The design rules, in order of importance:
//!
//! 1. **No start-up validation.** Asking the device whether the cache is still
//!    good costs exactly the 2–3 minutes it is meant to save. The cache is used
//!    as-is; `Refresh` is the only thing that re-reads the device.
//! 2. **Keyed by device serial.** `devices/<SERIAL>/library.json`, so two Zunes
//!    never share a library and plugging in the other one switches caches by
//!    itself.
//! 3. **Diffable.** Every object records its handle, the attributes the UI shows,
//!    and when it was read. That is enough to compare two libraries object by
//!    object, which is the primitive a future Mac ↔ Zune sync is built from
//!    (see [`LibraryCache::diff`] and the reserved [`LibraryCache::last_full_sync`]).
//!
//! Layout:
//!
//! ```text
//! <data-dir>/last-device                      # serial of the most recent read
//! <data-dir>/devices/<SERIAL>/library.json    # the whole library, pretty-printed
//! <data-dir>/devices/<SERIAL>/covers/<HANDLE>.jpg
//! ```
//!
//! `<data-dir>` is `~/Library/Application Support/zune-gui` on macOS,
//! `$XDG_DATA_HOME/zune-gui` (or `~/.local/share/zune-gui`) elsewhere, and can be
//! overridden with `ZUNE_CACHE_DIR` (used by the tests).

// The module deliberately exposes a small, tested API for pieces this version does
// not call yet: `diff`/`CacheDiff` (the Mac ↔ Zune sync primitive), `clear`,
// `has_cover`. They are part of the cache's contract, not dead weight.
#![allow(dead_code)]

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::core::library::DeviceTrack;
use crate::model::{Album, CoverSource, DeviceSnapshot, Track};

/// Bump when the on-disk shape changes in a way an older file cannot satisfy.
/// A file with a different version is ignored (and superseded by the next read).
pub const CACHE_VERSION: u32 = 1;

/// After this, the UI says the data may be stale. It is a *hint* only — nothing
/// is ever re-read automatically, because that would defeat the cache.
pub const STALE_AFTER_SECS: i64 = 24 * 60 * 60;

// ── locations ───────────────────────────────────────────────────────────────

/// Root of everything cached. Overridable so tests never touch real user data.
pub fn data_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("ZUNE_CACHE_DIR") {
        if !dir.is_empty() {
            return PathBuf::from(dir);
        }
    }
    platform_data_dir().unwrap_or_else(|| PathBuf::from(".zune-gui"))
}

#[cfg(target_os = "macos")]
fn platform_data_dir() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(PathBuf::from(home).join("Library/Application Support/zune-gui"))
}

#[cfg(target_os = "windows")]
fn platform_data_dir() -> Option<PathBuf> {
    let appdata = std::env::var_os("APPDATA")?;
    Some(PathBuf::from(appdata).join("zune-gui"))
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn platform_data_dir() -> Option<PathBuf> {
    if let Some(xdg) = std::env::var_os("XDG_DATA_HOME") {
        let base = PathBuf::from(xdg);
        if !base.as_os_str().is_empty() {
            return Some(base.join("zune-gui"));
        }
    }
    let home = std::env::var_os("HOME")?;
    Some(PathBuf::from(home).join(".local/share/zune-gui"))
}

/// A serial that can be a directory name. Unknown serials land in `unknown/`
/// rather than failing — a device that reports no serial still gets a cache.
pub fn serial_dir_name(serial: &str) -> String {
    let trimmed = serial.trim();
    if trimmed.is_empty() || trimmed == "-" {
        return "unknown".to_owned();
    }
    let name: String = trimmed
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect();
    if name.is_empty() {
        "unknown".to_owned()
    } else {
        name
    }
}

/// Whether a serial is specific enough to key a per-device cache on.
pub fn is_usable_serial(serial: &str) -> bool {
    let trimmed = serial.trim();
    !trimmed.is_empty() && trimmed != "-"
}

pub fn devices_dir() -> PathBuf {
    data_dir().join("devices")
}

pub fn device_dir(serial: &str) -> PathBuf {
    devices_dir().join(serial_dir_name(serial))
}

pub fn library_path(serial: &str) -> PathBuf {
    device_dir(serial).join("library.json")
}

pub fn covers_dir(serial: &str) -> PathBuf {
    device_dir(serial).join("covers")
}

/// Cover bytes are keyed by object handle, the only stable identity on the device.
pub fn cover_path(serial: &str, handle: u32) -> PathBuf {
    covers_dir(serial).join(format!("{handle:08x}.jpg"))
}

fn last_device_path() -> PathBuf {
    data_dir().join("last-device")
}

// ── time ────────────────────────────────────────────────────────────────────

pub fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// True once the cache is older than [`STALE_AFTER_SECS`]. Purely advisory.
pub fn is_stale(read_at: i64) -> bool {
    now_unix().saturating_sub(read_at) >= STALE_AFTER_SECS
}

/// `"3 min ago"` / `"2 h ago"` / `"just now"` — the human half of the UI label.
pub fn relative_age(read_at: i64, now: i64) -> String {
    let secs = now.saturating_sub(read_at).max(0);
    if secs < 60 {
        "just now".to_owned()
    } else if secs < 3600 {
        format!("{} min ago", secs / 60)
    } else if secs < 86_400 {
        format!("{} h ago", secs / 3600)
    } else {
        format!("{} d ago", secs / 86_400)
    }
}

// ── the cache itself ────────────────────────────────────────────────────────

/// A whole device library, as read once and written down.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LibraryCache {
    pub version: u32,
    pub serial: String,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub manufacturer: String,
    #[serde(default)]
    pub firmware: String,
    #[serde(default)]
    pub mtp_version: String,
    #[serde(default)]
    pub storage_desc: String,
    #[serde(default)]
    pub capacity: Option<u64>,
    #[serde(default)]
    pub free: Option<u64>,
    /// Unix seconds when the device library was read.
    pub read_at: i64,
    /// Reserved for the future Mac ↔ Zune sync: the last time a full sync ran.
    /// Always written, never read by this version — the field exists so the sync
    /// layer does not need a format change to start using it.
    #[serde(default)]
    pub last_full_sync: Option<i64>,
    #[serde(default)]
    pub tracks: Vec<CachedTrack>,
    #[serde(default)]
    pub albums: Vec<CachedAlbum>,
    #[serde(default)]
    pub playlists: Vec<CachedPlaylist>,
}

impl LibraryCache {
    /// Capture a freshly read library. `read_at` is stamped on every object so a
    /// later diff can tell "removed" from "not read yet".
    pub fn from_library(
        snapshot: &DeviceSnapshot,
        read_at: i64,
        tracks: &[Track],
        albums: &[Album],
        playlists: &[(u32, String, Vec<u32>)],
    ) -> Self {
        // `Track` has already lost the device's raw Duration/album-artist, so those
        // are filled in afterwards by `enrich_from_device_tracks` when available.
        Self {
            version: CACHE_VERSION,
            serial: snapshot.serial.clone(),
            model: snapshot.name.clone(),
            manufacturer: snapshot.manufacturer.clone(),
            firmware: snapshot.firmware.clone(),
            mtp_version: snapshot.mtp_version.clone(),
            storage_desc: snapshot.storage_desc.clone(),
            capacity: snapshot.capacity,
            free: snapshot.free,
            read_at,
            last_full_sync: None,
            tracks: tracks.iter().map(|t| CachedTrack::from(t, read_at)).collect(),
            albums: albums.iter().map(|a| CachedAlbum::from(a, read_at)).collect(),
            playlists: playlists
                .iter()
                .map(|(handle, name, refs)| CachedPlaylist {
                    handle: *handle,
                    name: name.clone(),
                    track_handles: refs.clone(),
                    read_at,
                })
                .collect(),
        }
    }

    /// Add the raw values that only the device read knows: the MTP `Duration` in
    /// milliseconds and the album artist. The UI value (`duration_s`) stays as the
    /// build that read it interpreted it; the raw number is what a future sync
    /// compares on and what can be re-interpreted when the units change.
    pub fn enrich_from_device_tracks(&mut self, device: &[DeviceTrack]) {
        let mut by_handle: HashMap<u32, &DeviceTrack> =
            device.iter().map(|t| (t.handle, t)).collect();
        for track in &mut self.tracks {
            let Some(raw) = by_handle.remove(&track.handle) else { continue };
            track.duration_ms = raw.duration_ms;
            if let Some(artist) = &raw.album_artist {
                track.album_artist = artist.clone();
            }
            if track.title.is_empty() {
                track.title = raw.title.clone().unwrap_or_default();
            }
        }
    }

    // ── restore ─────────────────────────────────────────────────────────────

    pub fn to_tracks(&self) -> Vec<Track> {
        self.tracks.iter().map(CachedTrack::to_track).collect()
    }

    pub fn to_albums(&self) -> Vec<Album> {
        self.albums.iter().map(CachedAlbum::to_album).collect()
    }

    /// Library-derived counts, the byte total and the format summary. Identity and
    /// storage are restored separately so a live device's fresh identity is never
    /// overwritten by a stale cached one.
    pub fn apply_library(&self, snapshot: &mut DeviceSnapshot) {
        snapshot.tracks = self.tracks.len();
        snapshot.albums = self.albums.len();
        snapshot.playlists = self.playlists.len();
        snapshot.library_bytes = self.tracks.iter().map(|t| t.size as u64).sum();
        let mut by_format: BTreeMap<u16, usize> = Default::default();
        for track in &self.tracks {
            *by_format.entry(track.format).or_default() += 1;
        }
        snapshot.format_summary = by_format
            .iter()
            .map(|(format, count)| {
                format!("{count} {}", crate::core::device::format_name::of(*format))
            })
            .collect::<Vec<_>>()
            .join(" · ");
    }

    pub fn apply_storage(&self, snapshot: &mut DeviceSnapshot) {
        if self.capacity.is_some() {
            snapshot.capacity = self.capacity;
        }
        if self.free.is_some() {
            snapshot.free = self.free;
        }
        if !self.storage_desc.is_empty() {
            snapshot.storage_desc = self.storage_desc.clone();
        }
    }

    /// `name`, `serial`, `firmware` … — used when the device is *not* connected and
    /// the cache is the only description of it.
    pub fn apply_identity(&self, snapshot: &mut DeviceSnapshot) {
        if !self.model.is_empty() {
            snapshot.name = self.model.clone();
        }
        if !self.manufacturer.is_empty() {
            snapshot.manufacturer = self.manufacturer.clone();
        }
        if is_usable_serial(&self.serial) {
            snapshot.serial = self.serial.clone();
        }
        if !self.firmware.is_empty() {
            snapshot.firmware = self.firmware.clone();
        }
        if !self.mtp_version.is_empty() {
            snapshot.mtp_version = self.mtp_version.clone();
        }
    }

    // ── persistence ─────────────────────────────────────────────────────────

    /// Write the cache (pretty JSON, so it can be read and diffed by hand) and
    /// remember this device as the most recent one.
    pub fn save(&self) -> Result<PathBuf> {
        let path = library_path(&self.serial);
        let json = serde_json::to_vec_pretty(self).context("serializing the library cache")?;
        write_atomic(&path, &json)?;
        let _ = remember_last(&self.serial);
        Ok(path)
    }

    /// Load the cache for one serial, or `None` when there is none (or it is stale
    /// in shape). A broken file is reported and treated as absent.
    pub fn load(serial: &str) -> Option<LibraryCache> {
        let path = library_path(serial);
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
            Err(e) => {
                log::warn!("could not read the library cache at {}: {e}", path.display());
                return None;
            }
        };
        match serde_json::from_slice::<LibraryCache>(&bytes) {
            Ok(cache) if cache.version == CACHE_VERSION => Some(cache),
            Ok(cache) => {
                log::warn!(
                    "library cache {} has version {} (this build understands {CACHE_VERSION}) — ignoring it",
                    path.display(),
                    cache.version
                );
                None
            }
            Err(e) => {
                log::warn!("library cache {} is not readable JSON: {e}", path.display());
                None
            }
        }
    }

    /// The most recently read library across every cached device. This is what lets
    /// a start with no device plugged in still show the last known library.
    pub fn load_newest() -> Option<LibraryCache> {
        if let Some(serial) = last_device() {
            if let Some(cache) = LibraryCache::load(&serial) {
                return Some(cache);
            }
        }
        // Fall back to scanning: the pointer may be missing (an old cache, or a
        // cleared file) while a perfectly good library.json is still there.
        let dir = devices_dir();
        let mut best: Option<LibraryCache> = None;
        for entry in std::fs::read_dir(&dir).ok()? {
            let Ok(entry) = entry else { continue };
            if !entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                continue;
            }
            let path = entry.path().join("library.json");
            let Ok(bytes) = std::fs::read(&path) else { continue };
            let Ok(cache) = serde_json::from_slice::<LibraryCache>(&bytes) else { continue };
            if cache.version != CACHE_VERSION {
                continue;
            }
            if best.as_ref().map(|b| cache.read_at > b.read_at).unwrap_or(true) {
                best = Some(cache);
            }
        }
        best
    }

    /// Record this cache as the most recently read one.
    pub fn remember(&self) -> Result<()> {
        remember_last(&self.serial)
    }

    // ── the diff primitive (foundation for Mac ↔ Zune sync) ─────────────────

    /// Compare two libraries object by object.
    ///
    /// Identity is the object handle (albums fall back to their name when the
    /// device gave them none); the payload is the attributes the UI shows.
    /// `read_at` is provenance, not content, so it is never a difference.
    pub fn diff(&self, other: &LibraryCache) -> CacheDiff {
        let mut diff = CacheDiff::default();

        let old: HashMap<u32, &CachedTrack> =
            self.tracks.iter().map(|t| (t.handle, t)).collect();
        let new: HashMap<u32, &CachedTrack> =
            other.tracks.iter().map(|t| (t.handle, t)).collect();
        for (handle, track) in &new {
            match old.get(handle) {
                None => diff.tracks_added.push(*handle),
                Some(previous) if !previous.same_payload(track) => diff.tracks_changed.push(*handle),
                Some(_) => {}
            }
        }
        for handle in old.keys() {
            if !new.contains_key(handle) {
                diff.tracks_removed.push(*handle);
            }
        }

        let old_albums: HashMap<String, &CachedAlbum> =
            self.albums.iter().map(|a| (a.identity(), a)).collect();
        let new_albums: HashMap<String, &CachedAlbum> =
            other.albums.iter().map(|a| (a.identity(), a)).collect();
        for (key, album) in &new_albums {
            match old_albums.get(key) {
                None => diff.albums_added.push(key.clone()),
                Some(previous) if !previous.same_payload(album) => {
                    diff.albums_changed.push(key.clone())
                }
                Some(_) => {}
            }
        }
        for key in old_albums.keys() {
            if !new_albums.contains_key(key) {
                diff.albums_removed.push(key.clone());
            }
        }

        let old_playlists: HashMap<u32, &CachedPlaylist> =
            self.playlists.iter().map(|p| (p.handle, p)).collect();
        let new_playlists: HashMap<u32, &CachedPlaylist> =
            other.playlists.iter().map(|p| (p.handle, p)).collect();
        for (handle, playlist) in &new_playlists {
            match old_playlists.get(handle) {
                None => diff.playlists_added.push(*handle),
                Some(previous) if !previous.same_payload(playlist) => {
                    diff.playlists_changed.push(*handle)
                }
                Some(_) => {}
            }
        }
        for handle in old_playlists.keys() {
            if !new_playlists.contains_key(handle) {
                diff.playlists_removed.push(*handle);
            }
        }

        diff.sort();
        diff
    }
}

// ── per-object records ──────────────────────────────────────────────────────

/// One track, with its handle, the attributes the UI shows and when it was read.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedTrack {
    pub handle: u32,
    pub filename: String,
    pub size: u32,
    pub format: u16,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub artist: String,
    #[serde(default)]
    pub album: String,
    #[serde(default)]
    pub album_artist: String,
    #[serde(default)]
    pub track_no: Option<u16>,
    /// Raw MTP `Duration` (0xDC89) in milliseconds, exactly as the device sent it.
    /// Absent for synthetic libraries (demo mode) and hand-written caches.
    #[serde(default)]
    pub duration_ms: Option<u32>,
    /// The duration in seconds as the build that read the device interpreted the
    /// raw value. This is what the UI shows, so a cache never changes the number
    /// under the user just because the interpretation was reworked.
    #[serde(default)]
    pub duration_s: Option<u32>,
    /// Unix seconds when this object was read from the device.
    #[serde(default)]
    pub read_at: i64,
}

impl CachedTrack {
    fn from(track: &Track, read_at: i64) -> Self {
        Self {
            handle: track.handle,
            filename: track.filename.clone(),
            size: track.size,
            format: track.format,
            title: track.title.clone(),
            artist: track.artist.clone(),
            album: track.album.clone(),
            album_artist: String::new(),
            track_no: track.track_no,
            duration_ms: None,
            duration_s: track.duration,
            read_at,
        }
    }

    fn to_track(&self) -> Track {
        Track {
            handle: self.handle,
            filename: self.filename.clone(),
            size: self.size,
            format: self.format,
            title: self.title.clone(),
            artist: self.artist.clone(),
            album: self.album.clone(),
            track_no: self.track_no,
            duration: self.duration_s,
        }
    }

    fn same_payload(&self, other: &CachedTrack) -> bool {
        self.filename == other.filename
            && self.size == other.size
            && self.format == other.format
            && self.title == other.title
            && self.artist == other.artist
            && self.album == other.album
            && self.album_artist == other.album_artist
            && self.track_no == other.track_no
            && self.duration_ms == other.duration_ms
            && self.duration_s == other.duration_s
    }
}

/// One album tile: identity, members, the cover link and the summed statistics.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedAlbum {
    pub name: String,
    #[serde(default)]
    pub handle: Option<u32>,
    #[serde(default)]
    pub artist: String,
    #[serde(default)]
    pub year: Option<u32>,
    #[serde(default)]
    pub track_handles: Vec<u32>,
    /// Cover-art object handle, whatever resolved it — this is the field the album
    /// page needs, and the reason the cache must carry handles, not just names.
    #[serde(default)]
    pub cover_handle: Option<u32>,
    /// `cover_source_code` value, not the display label (labels are UI text).
    #[serde(default)]
    pub cover_source: u8,
    #[serde(default)]
    pub bytes: u64,
    #[serde(default)]
    pub duration_s: u32,
    #[serde(default)]
    pub read_at: i64,
}

impl CachedAlbum {
    fn from(album: &Album, read_at: i64) -> Self {
        Self {
            name: album.name.clone(),
            handle: album.handle,
            artist: album.artist.clone(),
            year: album.year,
            track_handles: album.track_handles.clone(),
            cover_handle: album.cover_handle,
            cover_source: cover_source_code(album.cover_source),
            bytes: album.bytes,
            duration_s: album.duration,
            read_at,
        }
    }

    fn to_album(&self) -> Album {
        Album {
            name: self.name.clone(),
            handle: self.handle,
            artist: self.artist.clone(),
            year: self.year,
            tracks: self.track_handles.len(),
            bytes: self.bytes,
            duration: self.duration_s,
            track_handles: self.track_handles.clone(),
            cover_handle: self.cover_handle,
            cover_source: cover_source_from_code(self.cover_source),
        }
    }

    /// Device handle when there is one, else the lowercased name.
    fn identity(&self) -> String {
        match self.handle {
            Some(handle) => format!("{handle:#010x}"),
            None => format!("name:{}", self.name.to_lowercase()),
        }
    }

    fn same_payload(&self, other: &CachedAlbum) -> bool {
        self.name == other.name
            && self.handle == other.handle
            && self.artist == other.artist
            && self.year == other.year
            && self.track_handles == other.track_handles
            && self.cover_handle == other.cover_handle
            && self.cover_source == other.cover_source
            && self.bytes == other.bytes
            && self.duration_s == other.duration_s
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedPlaylist {
    pub handle: u32,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub track_handles: Vec<u32>,
    #[serde(default)]
    pub read_at: i64,
}

impl CachedPlaylist {
    fn same_payload(&self, other: &CachedPlaylist) -> bool {
        self.name == other.name && self.track_handles == other.track_handles
    }
}

/// Stable on-disk encoding of [`CoverSource`] (the display label is UI text and
/// may be reworded without invalidating caches).
pub fn cover_source_code(source: CoverSource) -> u8 {
    match source {
        CoverSource::None => 0,
        CoverSource::RefCoverArt => 1,
        CoverSource::RefJpeg => 2,
        CoverSource::RefOther => 3,
        CoverSource::ReverseRef => 4,
        CoverSource::FolderMatch => 5,
    }
}

pub fn cover_source_from_code(code: u8) -> CoverSource {
    match code {
        1 => CoverSource::RefCoverArt,
        2 => CoverSource::RefJpeg,
        3 => CoverSource::RefOther,
        4 => CoverSource::ReverseRef,
        5 => CoverSource::FolderMatch,
        _ => CoverSource::None,
    }
}

// ── the diff result ─────────────────────────────────────────────────────────

/// What changed between two libraries. Handles for tracks/playlists, handle-or-name
/// keys for albums. Empty means "identical".
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct CacheDiff {
    pub tracks_added: Vec<u32>,
    pub tracks_removed: Vec<u32>,
    pub tracks_changed: Vec<u32>,
    pub albums_added: Vec<String>,
    pub albums_removed: Vec<String>,
    pub albums_changed: Vec<String>,
    pub playlists_added: Vec<u32>,
    pub playlists_removed: Vec<u32>,
    pub playlists_changed: Vec<u32>,
}

impl CacheDiff {
    pub fn is_empty(&self) -> bool {
        self.tracks_added.is_empty()
            && self.tracks_removed.is_empty()
            && self.tracks_changed.is_empty()
            && self.albums_added.is_empty()
            && self.albums_removed.is_empty()
            && self.albums_changed.is_empty()
            && self.playlists_added.is_empty()
            && self.playlists_removed.is_empty()
            && self.playlists_changed.is_empty()
    }

    fn sort(&mut self) {
        self.tracks_added.sort_unstable();
        self.tracks_removed.sort_unstable();
        self.tracks_changed.sort_unstable();
        self.albums_added.sort();
        self.albums_removed.sort();
        self.albums_changed.sort();
        self.playlists_added.sort_unstable();
        self.playlists_removed.sort_unstable();
        self.playlists_changed.sort_unstable();
    }

    pub fn summary(&self) -> String {
        format!(
            "tracks +{} -{} ~{} · albums +{} -{} ~{} · playlists +{} -{} ~{}",
            self.tracks_added.len(),
            self.tracks_removed.len(),
            self.tracks_changed.len(),
            self.albums_added.len(),
            self.albums_removed.len(),
            self.albums_changed.len(),
            self.playlists_added.len(),
            self.playlists_removed.len(),
            self.playlists_changed.len(),
        )
    }
}

// ── cover bytes ─────────────────────────────────────────────────────────────

/// Read a previously downloaded cover payload. Works with no device attached.
pub fn read_cover(serial: &str, handle: u32) -> Option<Vec<u8>> {
    if !is_usable_serial(serial) {
        return None;
    }
    let path = cover_path(serial, handle);
    match std::fs::read(&path) {
        Ok(bytes) if !bytes.is_empty() => Some(bytes),
        _ => None,
    }
}

/// Store raw cover bytes so the next lookup is a file read, not a USB round trip.
pub fn write_cover(serial: &str, handle: u32, bytes: &[u8]) -> Result<()> {
    if !is_usable_serial(serial) {
        anyhow::bail!("no device serial to key the cover cache on");
    }
    if bytes.is_empty() {
        anyhow::bail!("refusing to cache an empty cover");
    }
    write_atomic(&cover_path(serial, handle), bytes)
}

/// Whether a cover is already cached, without reading it.
pub fn has_cover(serial: &str, handle: u32) -> bool {
    is_usable_serial(serial) && cover_path(serial, handle).is_file()
}

// ── clearing ────────────────────────────────────────────────────────────────

/// Delete one device's cache (library and covers).
pub fn clear(serial: &str) -> Result<()> {
    let dir = device_dir(serial);
    if dir.exists() {
        std::fs::remove_dir_all(&dir)
            .with_context(|| format!("removing {}", dir.display()))?;
    }
    Ok(())
}

/// Delete every cached device.
pub fn clear_all() -> Result<()> {
    let dir = devices_dir();
    if dir.exists() {
        std::fs::remove_dir_all(&dir)
            .with_context(|| format!("removing {}", dir.display()))?;
    }
    let _ = std::fs::remove_file(last_device_path());
    Ok(())
}

// ── small filesystem helpers ────────────────────────────────────────────────

/// Serial of the device whose library was read most recently.
pub fn last_device() -> Option<String> {
    let text = std::fs::read_to_string(last_device_path()).ok()?;
    let serial = text.trim().to_owned();
    (!serial.is_empty()).then_some(serial)
}

pub fn remember_last(serial: &str) -> Result<()> {
    write_atomic(&last_device_path(), serial.as_bytes())
}

/// Write via a temp file + rename so a crash never leaves a half-written cache
/// that the next launch would refuse to parse (or worse, trust).
fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, bytes).with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, path)
        .with_context(|| format!("renaming {} into place", tmp.display()))?;
    Ok(())
}

// ── tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "zune-cache-test-{}-{}",
            name,
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn track(handle: u32, title: &str, size: u32) -> Track {
        Track {
            handle,
            filename: format!("{title}.mp3"),
            size,
            format: 0x3009,
            title: title.to_owned(),
            artist: "Artist".to_owned(),
            album: "Album".to_owned(),
            track_no: Some(1),
            duration: Some(258),
        }
    }

    fn album(name: &str, handle: u32, cover: Option<u32>) -> Album {
        Album {
            name: name.to_owned(),
            handle: Some(handle),
            artist: "Artist".to_owned(),
            year: Some(1997),
            tracks: 1,
            bytes: 1000,
            duration: 258,
            track_handles: vec![1],
            cover_handle: cover,
            cover_source: if cover.is_some() {
                CoverSource::RefCoverArt
            } else {
                CoverSource::None
            },
        }
    }

    fn snapshot() -> DeviceSnapshot {
        DeviceSnapshot {
            name: "Zune 30".to_owned(),
            serial: "ABC123".to_owned(),
            firmware: "1.4".to_owned(),
            capacity: Some(30_000_000_000),
            free: Some(10_000_000_000),
            storage_desc: "Zune".to_owned(),
            ..Default::default()
        }
    }

    #[test]
    fn cache_round_trips_through_json() {
        let snapshot = snapshot();
        let tracks = vec![track(1, "Track", 1000)];
        let albums = vec![album("Album", 0x100, Some(0x200))];
        let playlists = vec![(0x300u32, "Mix".to_owned(), vec![1u32])];
        let cache =
            LibraryCache::from_library(&snapshot, 1_700_000_000, &tracks, &albums, &playlists);

        let json = serde_json::to_vec(&cache).unwrap();
        let back: LibraryCache = serde_json::from_slice(&json).unwrap();

        assert_eq!(back.version, CACHE_VERSION);
        assert_eq!(back.serial, "ABC123");
        // The album page reads `cover_handle`; the round trip must carry it.
        assert_eq!(back.albums[0].cover_handle, Some(0x200));
        assert_eq!(back.albums[0].cover_source, cover_source_code(CoverSource::RefCoverArt));
        assert_eq!(back.playlists[0].track_handles, vec![1]);

        let restored = back.to_tracks();
        assert_eq!(restored.len(), 1);
        assert_eq!(restored[0].title, "Track");
        assert_eq!(restored[0].duration, Some(258));

        let restored_albums = back.to_albums();
        assert_eq!(restored_albums[0].cover_handle, Some(0x200));
        assert_eq!(restored_albums[0].tracks, 1);
        assert_eq!(restored_albums[0].duration, 258);
    }

    #[test]
    fn apply_library_recomputes_the_snapshot() {
        let snapshot = snapshot();
        let tracks = vec![track(1, "A", 1000), track(2, "B", 2000)];
        let albums = vec![album("Album", 0x100, None)];
        let cache = LibraryCache::from_library(&snapshot, 0, &tracks, &albums, &[]);

        let mut applied = DeviceSnapshot::default();
        cache.apply_library(&mut applied);
        assert_eq!(applied.tracks, 2);
        assert_eq!(applied.albums, 1);
        assert_eq!(applied.library_bytes, 3000);
        assert_eq!(applied.format_summary, "2 mp3");
        // Identity is only restored on request, so a live device's stays fresh.
        assert_eq!(applied.serial, "");
        cache.apply_identity(&mut applied);
        assert_eq!(applied.serial, "ABC123");
    }

    #[test]
    fn diff_finds_added_removed_and_changed_objects() {
        let snapshot = snapshot();
        let before = LibraryCache::from_library(
            &snapshot,
            0,
            &[track(1, "Same", 1000), track(2, "Removed", 1000), track(3, "Old title", 1000)],
            &[album("Gone", 0x100, None)],
            &[],
        );
        let after = LibraryCache::from_library(
            &snapshot,
            100,
            &[track(1, "Same", 1000), track(3, "New title", 1000), track(4, "Added", 1000)],
            &[album("New", 0x101, None)],
            &[],
        );

        let diff = before.diff(&after);
        assert_eq!(diff.tracks_added, vec![4]);
        assert_eq!(diff.tracks_removed, vec![2]);
        assert_eq!(diff.tracks_changed, vec![3]);
        assert_eq!(diff.albums_added, vec!["0x00000101".to_owned()]);
        assert_eq!(diff.albums_removed, vec!["0x00000100".to_owned()]);
        assert!(!diff.is_empty());

        // A cache never differs from itself.
        assert!(after.diff(&after).is_empty());
    }

    #[test]
    fn enrichment_keeps_the_devices_raw_duration() {
        let snapshot = snapshot();
        let tracks = vec![track(1, "A", 1000)];
        let mut cache = LibraryCache::from_library(&snapshot, 0, &tracks, &[], &[]);
        // `Track` only carries the derived seconds; the raw ms is not known yet.
        assert_eq!(cache.tracks[0].duration_ms, None);
        assert_eq!(cache.tracks[0].duration_s, Some(258));

        let device = vec![DeviceTrack {
            handle: 1,
            filename: "a.mp3".to_owned(),
            size: 1000,
            title: Some("A".to_owned()),
            artist: Some("Artist".to_owned()),
            album: Some("Album".to_owned()),
            album_artist: Some("Album Artist".to_owned()),
            track: Some(1),
            format: 0x3009,
            duration_ms: Some(258_000),
        }];
        cache.enrich_from_device_tracks(&device);
        assert_eq!(cache.tracks[0].duration_ms, Some(258_000));
        // The number the user already saw must not shift.
        assert_eq!(cache.tracks[0].duration_s, Some(258));
        assert_eq!(cache.tracks[0].album_artist, "Album Artist");
    }

    #[test]
    fn stale_is_a_hint_and_relative_age_reads_like_english() {
        let now = 10_000_000;
        assert_eq!(relative_age(now - 5, now), "just now");
        assert_eq!(relative_age(now - 180, now), "3 min ago");
        assert_eq!(relative_age(now - 7200, now), "2 h ago");
        assert_eq!(relative_age(now - 3 * 86_400, now), "3 d ago");
        // Relative to the past, never negative.
        assert_eq!(relative_age(now + 500, now), "just now");

        assert!(!is_stale(now_unix()));
        assert!(is_stale(now_unix() - STALE_AFTER_SECS - 1));
        assert!(!is_stale(now_unix() - STALE_AFTER_SECS + 60));
    }

    #[test]
    fn serials_become_safe_directory_names() {
        assert_eq!(serial_dir_name("0123456789ABCDEF"), "0123456789ABCDEF");
        assert_eq!(serial_dir_name("a/b:c"), "a_b_c");
        assert_eq!(serial_dir_name("  "), "unknown");
        assert_eq!(serial_dir_name("-"), "unknown");
        assert!(!is_usable_serial("-"));
        assert!(is_usable_serial("ABC"));
    }

    #[test]
    fn cache_files_land_under_the_device_serial() {
        let dir = temp_dir("layout");
        std::env::set_var("ZUNE_CACHE_DIR", &dir);

        let cache =
            LibraryCache::from_library(&snapshot(), 1234, &[track(1, "A", 1)], &[], &[]);
        let written = cache.save().unwrap();
        assert!(written.ends_with("devices/ABC123/library.json"));
        assert!(written.is_file());

        let loaded = LibraryCache::load("ABC123").unwrap();
        assert_eq!(loaded.read_at, 1234);
        assert_eq!(loaded.tracks[0].title, "A");
        // The newest-cache lookup must find it without being told the serial.
        assert_eq!(LibraryCache::load_newest().unwrap().serial, "ABC123");

        write_cover("ABC123", 0x1f, b"jpeg-bytes").unwrap();
        assert_eq!(read_cover("ABC123", 0x1f).unwrap(), b"jpeg-bytes");
        assert!(has_cover("ABC123", 0x1f));
        assert!(read_cover("ABC123", 0x20).is_none());
        // An unusable serial must never create a cache directory.
        assert!(!is_usable_serial("-"));
        assert!(read_cover("-", 0x1f).is_none());

        clear("ABC123").unwrap();
        assert!(LibraryCache::load("ABC123").is_none());

        // A file written by another cache format must be ignored, not misread.
        let dev = device_dir("OLD");
        std::fs::create_dir_all(&dev).unwrap();
        std::fs::write(dev.join("library.json"), r#"{"version":0,"serial":"OLD","read_at":1}"#)
            .unwrap();
        assert!(LibraryCache::load("OLD").is_none());

        std::env::remove_var("ZUNE_CACHE_DIR");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
