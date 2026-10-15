//! Music library on the device: pushing tracks with metadata, playlists,
//! cover art and the "abstract album" objects the Zune needs before it will show
//! artist/album/song metadata during playback.
//!
//! Device model (what the firmware actually does):
//!   * audio lives as MP3 objects **at the device root**, named `<stem>.mp3`;
//!   * `.pla` playlists live in the `Playlists` folder and link tracks through
//!     ObjectReferences (0xDC41);
//!   * `CoverArt` (0x3802) objects live in a `CoverArt` folder, children of root;
//!   * an **Abstract Album** (0xBA03) is an empty object at the root whose Name is
//!     the album title and whose ObjectReferences are the member tracks (+ cover).
//!     Without it the Zune shows no metadata at all.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

use crate::device::{Session, ROOT};
use crate::mtp::{fmt, prop};
use crate::tags::{self, TrackTags};

pub const AUDIO_EXTS: [&str; 12] = [
    "mp3", "wav", "m4a", "aac", "flac", "ogg", "opus", "wma", "aif", "aiff", "ape", "mka",
];
const JPEG_EXTS: [&str; 2] = ["jpg", "jpeg"];

#[derive(Debug, Default, Clone)]
pub struct Props {
    pub size: Option<u32>,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub album_artist: Option<String>,
    pub track: Option<u16>,
}

#[derive(Debug, Default, Clone)]
pub struct LibrarySummary {
    pub tracks: usize,
    pub albums: usize,
    pub playlists: usize,
    /// Only filled when asked for: it needs the property sweep.
    pub bytes: Option<u64>,
}

/// A track as it exists on the device.
#[derive(Debug, Clone)]
pub struct DeviceTrack {
    pub handle: u32,
    pub filename: String,
    pub size: u32,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub album_artist: Option<String>,
    pub track: Option<u16>,
}

impl DeviceTrack {
    fn from_props(handle: u32, props: Option<&Props>) -> Self {
        let p = props.cloned().unwrap_or_default();
        DeviceTrack {
            handle,
            filename: p.title.clone().unwrap_or_default(),
            size: p.size.unwrap_or(0),
            title: p.title,
            artist: p.artist,
            album: p.album,
            album_artist: p.album_artist,
            track: p.track,
        }
    }

    pub fn display(&self) -> String {
        match (&self.title, &self.artist) {
            (Some(t), Some(a)) => format!("{t} — {a}"),
            (Some(t), None) => t.clone(),
            _ => self.filename.clone(),
        }
    }
}

/// A grouping object: `.pla` playlist or abstract album.
#[derive(Debug, Clone)]
pub struct DeviceGroup {
    pub handle: u32,
    pub kind: &'static str,
    pub filename: String,
    pub name: Option<String>,
    pub tracks: usize,
}

pub struct Library<'a> {
    pub session: &'a mut Session,
    pub storage: u32,
    /// Property reads that failed, so a wrong opcode or an unsupported property is
    /// reported instead of quietly rendering as an empty column.
    pub prop_failures: usize,
    pub last_prop_error: Option<String>,
}

impl<'a> Library<'a> {
    /// Bind a library to an authenticated session.
    pub fn new(session: &'a mut Session) -> Result<Self> {
        let storage = *session
            .storage_ids()?
            .first()
            .context("device reported no storage")?;
        session.session_storage = storage;
        Ok(Library { session, storage, prop_failures: 0, last_prop_error: None })
    }

    fn note_failure(&mut self, e: anyhow::Error) {
        self.prop_failures += 1;
        self.last_prop_error = Some(format!("{e:#}"));
    }

    /// One property as a string, recording (rather than hiding) a read failure.
    fn prop_str(&mut self, handle: u32, prop: u16) -> Option<String> {
        match self.session.get_prop_str(handle, prop) {
            Ok(v) if !v.is_empty() => Some(v),
            Ok(_) => None,
            Err(e) => {
                self.note_failure(e);
                None
            }
        }
    }

    // ── folders ─────────────────────────────────────────────────────────────

    /// Handle of a top-level folder (Music / Video / Pictures / Playlists …).
    pub fn find_folder(&mut self, name: &str) -> Result<Option<u32>> {
        for h in self.session.children(fmt::ASSOCIATION, ROOT)? {
            let Ok(info) = self.session.object_info(h) else { continue };
            if info.filename == name && info.parent == ROOT {
                return Ok(Some(h));
            }
        }
        Ok(None)
    }

    /// Handle of a top-level folder, creating it when the device has none.
    pub fn ensure_folder(&mut self, name: &str) -> Result<u32> {
        if let Some(h) = self.find_folder(name)? {
            return Ok(h);
        }
        let h = self.session.create_object(ROOT, fmt::ASSOCIATION, name, 0, 0, 0, 0)?;
        self.session.push_object_data(&[])?;
        self.session.set_prop_str(h, prop::NAME, name)?;
        Ok(h)
    }

    // ── tracks ──────────────────────────────────────────────────────────────

    /// Every MP3 at the device root.
    ///
    /// Tries the bulk property list first: one round trip per property covers the whole
    /// library, where per-track reads cost ~6 round trips each (minutes for thousands
    /// of tracks).
    pub fn tracks(&mut self) -> Result<Vec<DeviceTrack>> {
        let handles = self.session.children(fmt::MP3, ROOT)?;
        if handles.is_empty() {
            return Ok(Vec::new());
        }
        match self.collect_props(&handles) {
            Ok(props) => Ok(handles
                .into_iter()
                .map(|h| DeviceTrack::from_props(h, props.get(&h)))
                .collect()),
            Err(e) => {
                eprintln!("  · bulk property read unavailable ({e:#})");
                eprintln!("  · falling back to one call per track — slow for a big library");
                let mut out = Vec::with_capacity(handles.len());
                for (i, h) in handles.iter().enumerate() {
                    if i > 0 && i % 250 == 0 {
                        eprintln!("    … {i}/{} tracks", handles.len());
                    }
                    let info = self.session.object_info(*h)?;
                    out.push(self.track_detail(*h, info.filename, info.size));
                }
                Ok(out)
            }
        }
    }

    /// Read every property we care about for all of `handles`, in as few round trips
    /// as the device allows.
    fn collect_props(&mut self, handles: &[u32]) -> Result<HashMap<u32, Props>> {
        // One call for every property of every object…
        let entries = match self
            .session
            .object_prop_list(self.storage, u32::MAX, 0, u16::MAX)
        {
            Ok(e) if !e.is_empty() => e,
            Ok(_) => bail!("device returned an empty property list"),
            Err(e) => {
                // …or, failing that, one call per property.
                log::debug!("all-properties request failed: {e:#}");
                let mut acc = Vec::new();
                let mut errors = Vec::new();
                for p in [prop::SIZE, prop::NAME, prop::ARTIST, prop::ALBUM, prop::ALBUM_ARTIST, prop::TRACK] {
                    match self.session.object_prop_list(self.storage, u32::MAX, 0, p) {
                        Ok(mut e) => acc.append(&mut e),
                        Err(e) => errors.push(format!("{p:#06x}: {e:#}")),
                    }
                }
                if acc.is_empty() {
                    bail!("no property could be listed ({})", errors.join("; "));
                }
                acc
            }
        };

        let known: std::collections::HashSet<u32> = handles.iter().copied().collect();
        let mut map: HashMap<u32, Props> = HashMap::new();
        for e in entries {
            if !known.contains(&e.handle) {
                continue;
            }
            let p = map.entry(e.handle).or_default();
            match (e.property, e.text, e.number) {
                (prop::NAME, Some(t), _) => p.title = Some(t),
                (prop::ARTIST, Some(t), _) => p.artist = Some(t),
                (prop::ALBUM, Some(t), _) => p.album = Some(t),
                (prop::ALBUM_ARTIST, Some(t), _) => p.album_artist = Some(t),
                (prop::TRACK, _, Some(n)) if n != 0 => p.track = Some(n as u16),
                (prop::SIZE, _, Some(n)) => p.size = Some(n),
                _ => {}
            }
        }
        Ok(map)
    }

    /// Counts only: no per-object calls, so this stays fast on a big library.
    pub fn summary(&mut self, with_size: bool) -> Result<LibrarySummary> {
        let track_handles = self.session.children(fmt::MP3, ROOT)?;
        let albums = self.session.children(fmt::ABSTRACT_ALBUM, ROOT)?.len();
        let playlists = match self.find_folder("Playlists")? {
            Some(dir) => self.session.children(fmt::PLAYLIST, dir)?.len(),
            None => 0,
        };
        let bytes = if with_size {
            self.collect_props(&track_handles)
                .ok()
                .map(|m| m.values().filter_map(|p| p.size).map(u64::from).sum())
        } else {
            None
        };
        Ok(LibrarySummary { tracks: track_handles.len(), albums, playlists, bytes })
    }

    fn track_detail(&mut self, handle: u32, filename: String, size: u32) -> DeviceTrack {
        // Properties are read one call at a time; a failure is counted so the caller can
        // tell "the device has no tags" from "we asked for the wrong thing".
        let title = self.prop_str(handle, prop::NAME);
        let artist = self.prop_str(handle, prop::ARTIST);
        let album = self.prop_str(handle, prop::ALBUM);
        let album_artist = self.prop_str(handle, prop::ALBUM_ARTIST);
        let track = match self.session.get_prop_u16(handle, prop::TRACK) {
            Ok(0) => None,
            Ok(n) => Some(n),
            Err(e) => {
                self.note_failure(e);
                None
            }
        };
        DeviceTrack { handle, filename, size, title, artist, album, album_artist, track }
    }

    /// Find a root track by the name the firmware would have stored for `stem`.
    fn find_track(&mut self, stem: &str) -> Result<Option<u32>> {
        let want = tags::device_key(&format!("{stem}.mp3"));
        for h in self.session.children(fmt::MP3, ROOT)? {
            let Ok(info) = self.session.object_info(h) else { continue };
            if tags::device_key(&info.filename) == want {
                return Ok(Some(h));
            }
        }
        Ok(None)
    }

    /// Push one audio file: create the object, stream it, then set the metadata
    /// properties the Zune displays. Existing objects with the same name are
    /// replaced. Returns the new handle.
    ///
    /// Note on metadata: the Zune indexes title/artist/album/track from the **file's
    /// own tags** as it ingests the object (it renames the object to "NN Title.mp3"
    /// using them), and answers `SetObjectPropValue` with 0x200F (AccessDenied). The
    /// property writes are therefore best-effort — one summary line, never fatal.
    pub fn push_track(&mut self, path: &Path, data: &[u8], t: &TrackTags) -> Result<u32> {
        let stem = path
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        if let Some(old) = self.find_track(&stem)? {
            self.session.delete_object(old)?;
        }

        let filename = tags::stored_name(path);
        let handle = self
            .session
            .create_object(ROOT, fmt::MP3, &filename, data.len() as u32, 0, 0, 0)
            .with_context(|| format!("creating object for {filename}"))?;
        self.session.push_object_data(data)?;

        let mut attempts = 0usize;
        let mut failures: Vec<String> = Vec::new();
        let mut note = |what: &str, r: Result<()>, attempts: &mut usize, failures: &mut Vec<String>| {
            *attempts += 1;
            if let Err(e) = r {
                failures.push(format!("{what}: {e:#}"));
            }
        };
        note(
            "Name",
            self.session.set_prop_str(handle, prop::NAME, &t.title),
            &mut attempts,
            &mut failures,
        );
        for (label, p, v) in [
            ("Artist", prop::ARTIST, t.artist.as_deref()),
            ("Album", prop::ALBUM, t.album.as_deref()),
            ("AlbumArtist", prop::ALBUM_ARTIST, t.album_artist.as_deref()),
        ] {
            if let Some(v) = v {
                note(label, self.session.set_prop_str(handle, p, v), &mut attempts, &mut failures);
            }
        }
        if let Some(n) = t.track {
            note(
                "Track",
                self.session.set_prop_u16(handle, prop::TRACK, n),
                &mut attempts,
                &mut failures,
            );
        }
        if !failures.is_empty() {
            let denied = failures.iter().any(|f| f.contains("0x200f"));
            eprintln!("    · {}/{} properties refused by the device", failures.len(), attempts);
            if denied {
                eprintln!(
                    "      The Zune indexes metadata from the file's own tags and answers\n      \
                     SetObjectPropValue with 0x200F (AccessDenied). The tags we sent\n      \
                     (ID3v2.3) already carry title/artist/album/track, so this is expected —\n      \
                     run `zune props {}` after a re-index to confirm.",
                    hex(handle)
                );
            } else {
                eprintln!("      {}", failures[0]);
            }
        }
        Ok(handle)
    }

    pub fn remove(&mut self, handle: u32) -> Result<()> {
        self.session.delete_object(handle)
    }

    /// Push one photo: fitted to the screen, stored as `<stem>.jpg` in `Pictures`.
    pub fn push_photo(&mut self, photo: &crate::media::Photo) -> Result<u32> {
        let dir = self.ensure_folder("Pictures")?;
        // Deduplicate like the audio path does: same name = same object.
        for h in self.session.children(fmt::JPEG, dir)? {
            let Ok(info) = self.session.object_info(h) else { continue };
            if tags::device_key(&info.filename) == tags::device_key(&photo.name) {
                self.session.delete_object(h)?;
            }
        }
        let handle = self.session.create_object(
            dir,
            fmt::JPEG,
            &photo.name,
            photo.data.len() as u32,
            photo.width,
            photo.height,
            24,
        )?;
        self.session.push_object_data(&photo.data)?;
        Ok(handle)
    }

    /// Push one video into the `Video` folder as `<stem>.wmv`.
    pub fn push_video(&mut self, name: &str, data: &[u8], title: &str) -> Result<u32> {
        let dir = self.ensure_folder("Video")?;
        for h in self.session.children(fmt::WMV, dir)? {
            let Ok(info) = self.session.object_info(h) else { continue };
            if tags::device_key(&info.filename) == tags::device_key(name) {
                self.session.delete_object(h)?;
            }
        }
        let handle = self.session.create_object(dir, fmt::WMV, name, data.len() as u32, 320, 240, 0)?;
        self.session.push_object_data(data)?;
        // Title shown under Videos; the video is already on the device either way.
        if let Err(e) = self.session.set_prop_str(handle, prop::NAME, title) {
            eprintln!("    ! video title not set: {e}");
        }
        Ok(handle)
    }

    /// Remove a root track by file stem.
    pub fn remove_track(&mut self, stem: &str) -> Result<bool> {
        match self.find_track(stem)? {
            Some(h) => {
                self.session.delete_object(h)?;
                Ok(true)
            }
            None => Ok(false),
        }
    }

    // ── playlists & albums ──────────────────────────────────────────────────

    /// Create (or replace) a `.pla` playlist referencing `refs`.
    pub fn create_playlist(&mut self, name: &str, refs: &[u32]) -> Result<u32> {
        let dir = self.ensure_folder("Playlists")?;
        let filename = format!("{name}.pla");

        for h in self.session.children(fmt::PLAYLIST, dir)? {
            let Ok(info) = self.session.object_info(h) else { continue };
            if info.filename.eq_ignore_ascii_case(&filename) {
                self.session.delete_object(h)?;
            }
        }

        let handle = self.session.create_object(dir, fmt::PLAYLIST, &filename, 0, 0, 0, 0)?;
        self.session.push_object_data(&[])?;
        self.session.set_prop_str(handle, prop::NAME, name)?;
        if !refs.is_empty() {
            self.session.set_object_refs(handle, refs)?;
        }
        Ok(handle)
    }

    /// Push a JPEG as CoverArt (0x3802) into the root `CoverArt` folder.
    pub fn push_cover_art(&mut self, jpeg: &Path) -> Result<u32> {
        let data = std::fs::read(jpeg).with_context(|| format!("reading {}", jpeg.display()))?;
        let dir = self.ensure_folder("CoverArt")?;
        let filename = jpeg
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "cover.jpg".into());
        let handle = self
            .session
            .create_object(dir, fmt::COVER_ART, &filename, data.len() as u32, 0, 0, 0)?;
        self.session.push_object_data(&data)?;
        Ok(handle)
    }

    /// Read the cover art out of the device's own copy of this album's audio files.
    ///
    /// A Zune 30 keeps art in two places and only one is readable over MTP: library
    /// thumbnails sit behind a `ZMediaLib` API whose read side `MtpSvc` never imports,
    /// while most files also carry a full JPEG in an ID3v2 `APIC` frame — and those read
    /// fine. Two partial reads: 64 KB to locate the frame, then exactly the picture,
    /// because the picture is ~272 KB and the file is ~8 MB.
    ///
    /// `None` means this album is not on the device, or its files carry no art.
    pub fn cover_from_device(&mut self, album: &str) -> Result<Option<Vec<u8>>> {
        const HEAD: u32 = 64 * 1024;
        let wanted = album.trim().to_lowercase();
        let Some(track) = self.tracks()?.into_iter().find(|t| {
            t.album
                .as_deref()
                .map(|a| a.trim().to_lowercase() == wanted)
                .unwrap_or(false)
        }) else {
            return Ok(None);
        };
        let head = match self.session.get_partial_object(track.handle, 0, HEAD) {
            Ok(head) => head,
            // A handle that no longer exists makes the firmware answer nothing, so this is
            // a "no art found" rather than a failure to report upwards.
            Err(e) => {
                eprintln!("    (device read for cover art did not answer: {e:#})");
                return Ok(None);
            }
        };
        let Some(at) = crate::id3::locate_picture(&head) else {
            return Ok(None);
        };
        // A frame id matched inside compressed audio can declare a nonsense size.
        if at.payload_size == 0 || at.payload_size > 8 * 1024 * 1024 {
            return Ok(None);
        }
        let picture = self.session.get_partial_object(
            track.handle,
            at.body_offset as u32,
            at.payload_size as u32,
        )?;
        Ok(Some(picture))
    }

    /// Create the abstract album object the Zune uses to group tracks.
    pub fn create_abstract_album(&mut self, name: &str, refs: &[u32]) -> Result<u32> {
        let handle = self.session.create_object(ROOT, fmt::ABSTRACT_ALBUM, name, 0, 0, 0, 0)?;
        self.session.push_object_data(&[])?;
        self.session.set_prop_str(handle, prop::NAME, name)?;
        if !refs.is_empty() {
            self.session.set_object_refs(handle, refs)?;
        }
        Ok(handle)
    }

    /// Playlists and abstract albums currently on the device.
    pub fn groups(&mut self) -> Result<Vec<DeviceGroup>> {
        let mut out = Vec::new();

        let pl_dir = self.find_folder("Playlists")?;
        if let Some(dir) = pl_dir {
            for h in self.session.children(fmt::PLAYLIST, dir)? {
                let Ok(info) = self.session.object_info(h) else { continue };
                let name = self.prop_str(h, prop::NAME);
                // A playlist's references are its tracks, in order.
                let tracks = self.refs(h).len();
                out.push(DeviceGroup { handle: h, kind: "playlist", filename: info.filename, name, tracks });
            }
        }
        for h in self.session.children(fmt::ABSTRACT_ALBUM, ROOT)? {
            let Ok(info) = self.session.object_info(h) else { continue };
            let name = self.prop_str(h, prop::NAME);
            let refs = self.refs(h);
            // An album references its tracks and, when it has one, the cover art.
            // Checking just the last reference costs one call per album and tells the
            // two apart without walking the whole list.
            let mut tracks = refs.len();
            if let Some(last) = refs.last() {
                if let Ok(info) = self.session.object_info(*last) {
                    if matches!(info.format, fmt::COVER_ART | fmt::JPEG) {
                        tracks = tracks.saturating_sub(1);
                    }
                }
            }
            out.push(DeviceGroup { handle: h, kind: "album", filename: info.filename, name, tracks });
        }
        Ok(out)
    }

    fn refs(&mut self, handle: u32) -> Vec<u32> {
        match self.session.object_refs(handle) {
            Ok(v) => v,
            Err(e) => {
                self.note_failure(e);
                Vec::new()
            }
        }
    }

    // ── the full album workflow ─────────────────────────────────────────────

    /// Push a directory as one album: tracks → playlist → cover art → abstract album.
    pub fn sync_album(
        &mut self,
        dir: &Path,
        name: Option<&str>,
        cover: Option<&Path>,
        make_playlist: bool,
        dry_run: bool,
        mut progress: impl FnMut(&str),
    ) -> Result<AlbumReport> {
        if !dir.is_dir() {
            bail!("{} is not a directory", dir.display());
        }
        let files = audio_files(dir)?;
        if files.is_empty() {
            bail!("no audio files in {}", dir.display());
        }

        // Album title: explicit, else the folder name without its "NN - " prefix,
        // else the first track's album tag (which is what the Zune matches on).
        let album_name = match name {
            Some(n) => n.to_string(),
            None => {
                let from_tag = tags::read(&files[0]).album;
                from_tag.unwrap_or_else(|| derive_album_name(dir))
            }
        };
        progress(&format!("album: {album_name} ({} tracks)", files.len()));

        let mut track_tags: Vec<(PathBuf, TrackTags)> = files
            .iter()
            .map(|p| (p.clone(), tags::read(p)))
            .collect();
        track_tags.sort_by_key(|(p, t)| (t.track.unwrap_or(0), p.clone()));

        if dry_run {
            // Say where the cover would come from, without reading or writing anything: a
            // dry run that cannot tell you "the device already has this one" is only half
            // a dry run.
            let local = pick_cover(dir, cover);
            match local {
                Some(c) => progress(&format!("  cover: {}", c.display())),
                None => match self.cover_from_device(&album_name) {
                    Ok(Some(bytes)) => progress(&format!(
                        "  cover: none in the folder — the device has one for this album \
                         ({:.0} KB in the audio file's ID3 tag), it would be reused",
                        bytes.len() as f64 / 1024.0
                    )),
                    Ok(None) => progress("  cover: none in the folder, and none on the device"),
                    Err(e) => progress(&format!("  cover: none in the folder (device lookup failed: {e:#})")),
                },
            }
            for (p, t) in &track_tags {
                progress(&format!(
                    "  would push {:<40} {}{}{}",
                    p.file_name().unwrap_or_default().to_string_lossy(),
                    t.title,
                    t.artist.as_deref().map(|a| format!(" · {a}")).unwrap_or_default(),
                    t.track.map(|n| format!(" [{n:02}]")).unwrap_or_default()
                ));
            }
            let cover_pick = pick_cover(dir, cover);
            progress(&format!(
                "  cover: {}",
                cover_pick.as_ref().map(|c| c.display().to_string()).unwrap_or_else(|| "(none)".into())
            ));
            return Ok(AlbumReport { album: album_name, pushed: 0, total: track_tags.len(), playlist: None, cover: None, album_handle: None });
        }

        // 1. tracks (converted to MP3 when the file is not already device-ready)
        let mut refs = Vec::new();
        for (path, t) in &track_tags {
            let converted = crate::media::to_mp3(path)
                .with_context(|| format!("converting {}", path.display()))?;
            let handle = self.push_track(path, &converted.data, t)?;
            progress(&format!(
                "  ✓ {:<40} {}{}",
                path.file_name().unwrap_or_default().to_string_lossy(),
                t.title,
                t.artist.as_deref().map(|a| format!(" · {a}")).unwrap_or_default()
            ));
            refs.push(handle);
        }

        // 2. playlist
        let playlist = if make_playlist {
            let h = self.create_playlist(&album_name, &refs)?;
            progress(&format!("  ✓ playlist {album_name}.pla ({} tracks)", refs.len()));
            Some(h)
        } else {
            None
        };

        // 3. cover art (largest JPEG in the folder, unless one was named)
        let cover_handle = match pick_cover(dir, cover) {
            Some(c) => {
                let size = std::fs::metadata(&c).map(|m| m.len()).unwrap_or(0);
                let h = self.push_cover_art(&c)?;
                progress(&format!(
                    "  ✓ cover art {} ({:.0} KB)",
                    c.file_name().unwrap_or_default().to_string_lossy(),
                    size as f64 / 1024.0
                ));
                Some(h)
            }
            None => {
                // Nothing in the folder. The device may already have art for this album,
                // because a Zune stores it inside the audio files rather than as a separate
                // object — so the copy being pushed can borrow it. It is written beside the
                // folder's audio rather than over anything, and named for where it came from.
                let extracted = dir.join("cover-from-device.jpg");
                match self.cover_from_device(&album_name) {
                    Ok(Some(bytes)) => {
                        match std::fs::write(&extracted, &bytes) {
                            Ok(()) => {
                                progress(&format!(
                                    "  ✓ cover art recovered from the device ({:.0} KB) → {}",
                                    bytes.len() as f64 / 1024.0,
                                    extracted.display()
                                ));
                                let h = self.push_cover_art(&extracted)?;
                                Some(h)
                            }
                            Err(e) => {
                                progress(&format!("  · no cover art found (writing {} failed: {e})", extracted.display()));
                                None
                            }
                        }
                    }
                    Ok(None) => {
                        progress("  · no cover art found, and the device has none for this album either");
                        None
                    }
                    Err(e) => {
                        progress(&format!("  · no cover art found (device lookup failed: {e:#})"));
                        None
                    }
                }
            }
        };

        // 4. abstract album — without it the device shows no metadata
        let mut album_refs = refs.clone();
        if let Some(c) = cover_handle {
            album_refs.push(c);
        }
        let album_handle = self.create_abstract_album(&album_name, &album_refs)?;
        progress(&format!("  ✓ abstract album {album_name} ({})", hex(album_handle)));

        Ok(AlbumReport {
            album: album_name,
            pushed: refs.len(),
            total: track_tags.len(),
            playlist,
            cover: cover_handle,
            album_handle: Some(album_handle),
        })
    }
}

impl Session {
    /// Raw property value (kept for callers that need the bytes).
    pub fn get_prop_raw_public(&mut self, handle: u32, prop: u16) -> Result<Vec<u8>> {
        self.send_cmd(crate::mtp::OP_GET_OBJECT_PROP, &[handle, prop as u32])?;
        let d = self.recv_data()?.data;
        self.recv_resp()?;
        Ok(d)
    }
}

#[derive(Debug, Default)]
pub struct AlbumReport {
    pub album: String,
    pub pushed: usize,
    pub total: usize,
    pub playlist: Option<u32>,
    pub cover: Option<u32>,
    pub album_handle: Option<u32>,
}

pub fn hex(h: u32) -> String {
    format!("{h:#010x}")
}

/// Audio files directly inside `dir`, in name order.
pub fn audio_files(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
        let path = entry?.path();
        if !path.is_file() {
            continue;
        }
        let ext = path
            .extension()
            .map(|e| e.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        if AUDIO_EXTS.contains(&ext.as_str()) {
            out.push(path);
        }
    }
    out.sort();
    Ok(out)
}

/// "01 - Album Name" → "Album Name"; a plain folder name is used as-is.
fn derive_album_name(dir: &Path) -> String {
    let raw = dir.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    let trimmed = raw.trim_start_matches(|c: char| c.is_ascii_digit() || c.is_whitespace());
    let trimmed = trimmed.trim_start_matches(|c: char| c == '-' || c == '–' || c == '—');
    let trimmed = trimmed.trim();
    if trimmed.is_empty() {
        raw
    } else {
        trimmed.to_string()
    }
}

/// Largest JPEG in the folder, or the explicitly named one.
fn pick_cover(dir: &Path, explicit: Option<&Path>) -> Option<PathBuf> {
    if let Some(p) = explicit {
        if p.is_file() {
            return Some(p.to_path_buf());
        }
    }
    let mut best: Option<(u64, PathBuf)> = None;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for entry in rd.flatten() {
            let p = entry.path();
            if p.is_dir() {
                stack.push(p);
                continue;
            }
            let is_jpeg = p
                .extension()
                .map(|e| JPEG_EXTS.contains(&e.to_string_lossy().to_lowercase().as_str()))
                .unwrap_or(false);
            if !is_jpeg {
                continue;
            }
            let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
            if best.as_ref().map(|(s, _)| size > *s).unwrap_or(true) {
                best = Some((size, p));
            }
        }
    }
    best.map(|(_, p)| p)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn album_names_lose_their_numbering() {
        assert_eq!(derive_album_name(Path::new("/m/01 - Kid A")), "Kid A");
        assert_eq!(derive_album_name(Path::new("/m/07 Album")), "Album");
        assert_eq!(derive_album_name(Path::new("/m/Loveless")), "Loveless");
    }
}
