//! Snapshot of `zune-rs/src/library.rs` (read-only copy — do NOT edit zune-rs from here).
//!
//! Provenance: re-synced 2026-10-15T13:16:15Z from ~/zune/zune-rs/src/library.rs
//! sha256: 8f642fa1ddf1f528e8117d737316e7b9dd288c60d8a6e2df1e0ed1d61264830d
//! Paths rewritten: crate::{mtp,mtpz,usb,device,library,media,tags} -> crate::core::*.
//! Re-synced to pick up the upstream MTPZ CMAC fix (the previous hand-rolled
//! doubling step produced wrong MACs) and the library property-failure reporting.
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
use std::time::Duration;

use anyhow::{bail, Context, Result};

use crate::core::device::{Session, ROOT};
use crate::core::mtp::{fmt, prop};
use crate::core::tags::{self, TrackTags};

pub const AUDIO_EXTS: [&str; 12] = [
    "mp3", "wav", "m4a", "aac", "flac", "ogg", "opus", "wma", "aif", "aiff", "ape", "mka",
];
const JPEG_EXTS: [&str; 2] = ["jpg", "jpeg"];

/// The properties a track needs, as delivered by a bulk sweep.
#[derive(Debug, Default, Clone)]
pub struct Props {
    pub size: Option<u32>,
    pub name: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub album_artist: Option<String>,
    pub track: Option<u16>,
    /// Milliseconds, exactly as the device reports it.
    pub duration_ms: Option<u32>,
}

/// How an object's references were read — logged, because "no cover art" and "the
/// reference read silently failed" look identical from the UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefsPath {
    /// Nothing read yet.
    Unknown,
    /// GetObjectReferences (0x9810).
    Opcode,
    /// The ObjectReferences property (0xDC41).
    Property,
    /// Both routes came back empty.
    Failed,
}

impl RefsPath {
    pub fn label(self) -> &'static str {
        match self {
            RefsPath::Unknown => "not tried yet",
            RefsPath::Opcode => "GetObjectReferences (0x9810)",
            RefsPath::Property => "the ObjectReferences property (0xDC41)",
            RefsPath::Failed => "nothing — both routes came back empty",
        }
    }
}

/// Which strategy produced the library's properties — logged, so a slow read is never
/// a mystery.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sweep {
    /// One round trip for everything.
    AllAtOnce,
    /// One round trip per property, covering every object.
    PerProperty,
    /// One round trip per object, carrying all of its properties.
    PerTrackBulk,
    /// The classic path: several calls per track.
    PerTrack,
}

impl Sweep {
    pub fn label(self) -> &'static str {
        match self {
            Sweep::AllAtOnce => "one bulk property read",
            Sweep::PerProperty => "one bulk read per property",
            Sweep::PerTrackBulk => "one property list per track",
            Sweep::PerTrack => "per-track reads (the device refused the bulk form)",
        }
    }
}

/// Fold property-list entries into one `Props` per object handle.
pub fn collect_props(entries: Vec<crate::core::mtp::PropEntry>) -> HashMap<u32, Props> {
    let mut map: HashMap<u32, Props> = HashMap::new();
    for entry in entries {
        let props = map.entry(entry.handle).or_default();
        match entry.property {
            prop::SIZE => props.size = entry.number,
            prop::NAME => props.name = entry.text,
            prop::ARTIST => props.artist = entry.text,
            prop::ALBUM => props.album = entry.text,
            prop::ALBUM_ARTIST => props.album_artist = entry.text,
            prop::TRACK => props.track = entry.number.map(|n| n.min(u16::MAX as u32) as u16),
            prop::DURATION => props.duration_ms = entry.number,
            _ => {}
        }
    }
    map
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
    /// ZUNE-GUI addition: the object's format, so WMA/AAC tracks are not mistaken for
    /// anything else (the device's own library is mostly WMA).
    pub format: u16,
    /// ZUNE-GUI addition: Duration (0xDC89) **in milliseconds**, as the device reports it.
    pub duration_ms: Option<u32>,
}

impl DeviceTrack {
    /// Build a track from a bulk-swept property set.
    ///
    /// A sweep returns the object's *name property*, not its file name, so `filename`
    /// carries that instead when nothing better is known — which is what the Zune shows
    /// for these objects anyway.
    pub fn from_props(handle: u32, format: u16, props: Option<&Props>) -> Self {
        let p = props.cloned().unwrap_or_default();
        DeviceTrack {
            handle,
            filename: p.name.clone().unwrap_or_default(),
            size: p.size.unwrap_or(0),
            title: p.name,
            artist: p.artist,
            album: p.album,
            album_artist: p.album_artist,
            track: p.track,
            format,
            duration_ms: p.duration_ms,
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
    /// ZUNE-GUI addition: which property strategy the last library read used.
    pub last_sweep: Sweep,
    /// ZUNE-GUI addition: set once the device has refused the bulk property list, so
    /// later reads skip straight to the slow path instead of paying the timeout again.
    pub bulk_props_unavailable: bool,
    /// ZUNE-GUI addition: the same, for the *per-object* form of the request. This device
    /// refuses both (0xa801 Invalid_ObjectPropertyCode), and probing costs three seconds
    /// plus a drain, so the answer is remembered.
    pub per_object_bulk_unavailable: bool,
    /// ZUNE-GUI addition: which route served `ObjectReferences`, and why it failed.
    pub last_refs_path: RefsPath,
    pub last_refs_error: Option<String>,
}

impl<'a> Library<'a> {
    /// Bind a library to an authenticated session.
    pub fn new(session: &'a mut Session) -> Result<Self> {
        let storage = *session
            .storage_ids()?
            .first()
            .context("device reported no storage")?;
        session.session_storage = storage;
        Ok(Library {
            session,
            storage,
            prop_failures: 0,
            last_prop_error: None,
            last_sweep: Sweep::PerTrack,
            bulk_props_unavailable: false,
            per_object_bulk_unavailable: false,
            last_refs_path: RefsPath::Unknown,
            last_refs_error: None,
        })
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

    /// Every MP3 at the device root, with its properties.
    pub fn tracks(&mut self) -> Result<Vec<DeviceTrack>> {
        self.tracks_reported(|_, _| {})
    }

    /// ZUNE-GUI addition: same as [`Self::tracks`], reporting (done, total) after each
    /// object so a slow USB link can still show a moving refresh bar.
    ///
    /// Reads the library the fast way first: one GetObjectPropList round trip for every
    /// track's properties, instead of ~6 round trips *per track* — on a 2425-track
    /// device that is the difference between minutes and seconds. If the device refuses
    /// the bulk form, it falls back to per-property sweeps and finally to per-track
    /// reads; [`Self::last_sweep`] says which path actually ran.
    pub fn tracks_reported(
        &mut self,
        mut progress: impl FnMut(usize, usize),
    ) -> Result<Vec<DeviceTrack>> {
        // ZUNE-GUI: the device's own library is largely **WMA** (written by Microsoft's
        // software) alongside MP3 and AAC — everything this client pushes is MP3.
        // Enumerating only MP3 silently truncated the track list, and because album
        // references point at those WMA objects, it also made them look like cover art.
        const AUDIO_FORMATS: [u16; 3] = [fmt::MP3, fmt::WMA, fmt::AAC];
        let mut found: Vec<(u32, u16)> = Vec::new();
        for format in AUDIO_FORMATS {
            for handle in self.session.children(format, ROOT)? {
                found.push((handle, format));
            }
        }
        let total = found.len();
        let handles: Vec<u32> = found.iter().map(|(h, _)| *h).collect();

        // ── the fast path ──
        match self.props_sweep(&handles) {
            Ok((props, kind)) => {
                self.last_sweep = kind;
                let mut out = Vec::with_capacity(total);
                for (index, (handle, format)) in found.iter().enumerate() {
                    out.push(DeviceTrack::from_props(*handle, *format, props.get(handle)));
                    progress(index + 1, total);
                }
                return Ok(out);
            }
            Err(e) => {
                // props_sweep already drained; say so once, at a level the Log shows.
                log::warn!("bulk property read unavailable ({e:#}); reading per track");
                self.last_sweep = Sweep::PerTrack;
            }
        }

        // ── the middle path: one property-list call per track ──
        //
        // The all-objects sweep above is refused, but a single-object request is a
        // different call and may well be answered. One probe decides, because the payoff
        // is large: it turns six round trips per track into one, which is the difference
        // between a two-and-a-half-minute read and a half-minute one.
        if let Some((first, _)) = found.first().copied() {
            if self.per_object_bulk_unavailable {
                log::debug!("skipping the per-object property list: the device refused it before");
            } else {
            const PROBE_TIMEOUT: Duration = Duration::from_secs(3);
            let probe = self.session.with_read_timeout(PROBE_TIMEOUT, |session| {
                session.object_prop_list(first, 0, u16::MAX, 0)
            });
            match probe {
                Ok(entries) if !entries.is_empty() => {
                    log::info!(
                        "per-object property list works — reading {total} tracks with one call each"
                    );
                    self.last_sweep = Sweep::PerTrackBulk;
                    let mut out = Vec::with_capacity(total);
                    for (index, (h, format)) in found.into_iter().enumerate() {
                        let props = match self.session.object_prop_list(h, 0, u16::MAX, 0) {
                            Ok(entries) if !entries.is_empty() => {
                                collect_props(entries).remove(&h).unwrap_or_default()
                            }
                            Ok(_) => Props::default(),
                            Err(e) => {
                                self.note_failure(e);
                                Props::default()
                            }
                        };
                        out.push(DeviceTrack::from_props(h, format, Some(&props)));
                        progress(index + 1, total);
                    }
                    return Ok(out);
                }
                Ok(_) => log::debug!("the per-object property list returned no entries"),
                Err(e) => log::debug!("the per-object property list did not answer: {e:#}"),
            }
            // Whatever the device sent for a probe it never finished has to go, or the
            // next command fails too — the same trap the all-objects sweep falls into.
            match self.session.drain_pending_input(Duration::from_millis(350)) {
                Ok(0) => {}
                Ok(bytes) => {
                    log::info!("drained {bytes} bytes left by the failed per-object probe")
                }
                Err(e) => log::debug!("drain after the per-object probe reported: {e:#}"),
            }
            self.per_object_bulk_unavailable = true;
            }
        }

        // ── the slow path: one set of calls per track ──
        let mut out = Vec::with_capacity(total);
        for (index, (h, format)) in found.into_iter().enumerate() {
            let info = self.session.object_info(h)?;
            out.push(self.track_detail(h, info.filename, info.size, format));
            progress(index + 1, total);
        }
        Ok(out)
    }

    /// Every property we care about, for every handle, in as few round trips as the
    /// device allows. Returns the map and which strategy worked.
    ///
    /// **The failure mode here is worse than slowness.** A Zune that does not implement
    /// the bulk property list does not answer — it lets the read time out — and the
    /// unanswered response then sits in its buffer, where it makes every *later* command
    /// fail with a bulk-OUT timeout. So a failed sweep is (a) probed with a short
    /// timeout, (b) followed by a drain, and (c) remembered, so it is never attempted
    /// twice in a session.
    pub fn props_sweep(&mut self, handles: &[u32]) -> Result<(HashMap<u32, Props>, Sweep)> {
        if handles.is_empty() {
            return Ok((HashMap::new(), Sweep::AllAtOnce));
        }
        if self.bulk_props_unavailable {
            bail!("the bulk property list is disabled for this session");
        }
        // An escape hatch for A/B comparison on a device that answers inconsistently.
        if std::env::var_os("ZUNE_NO_BULK_PROPS").is_some() {
            self.bulk_props_unavailable = true;
            bail!("bulk property reads disabled by ZUNE_NO_BULK_PROPS");
        }

        // Probe with a short timeout first: enough for a device that implements it,
        // cheap for one that does not.
        const PROBE_TIMEOUT: Duration = Duration::from_secs(3);
        let probe = self.session.with_read_timeout(PROBE_TIMEOUT, |session| {
            session.object_prop_list(u32::MAX, 0, u16::MAX, u32::MAX)
        });

        match probe {
            Ok(entries) if !entries.is_empty() => {
                return Ok((collect_props(entries), Sweep::AllAtOnce));
            }
            Ok(_) => {
                self.note_failure(anyhow::anyhow!("the bulk property list returned no entries"));
            }
            Err(e) => {
                self.note_failure(anyhow::anyhow!("bulk property list probe: {e:#}"));
            }
        }

        // The device did not answer in time; whatever it *did* send has to go, or the
        // next command fails too.
        match self.session.drain_pending_input(Duration::from_millis(350)) {
            Ok(0) => log::debug!("nothing to drain after the failed property sweep"),
            Ok(bytes) => log::info!("drained {bytes} bytes left by the failed property sweep"),
            Err(e) => log::debug!("drain after the failed sweep reported: {e:#}"),
        }
        self.bulk_props_unavailable = true;
        bail!("the device does not answer the bulk property list (0x9805)")
    }

    fn track_detail(&mut self, handle: u32, filename: String, size: u32, format: u16) -> DeviceTrack {
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
        // ZUNE-GUI: duration has its own column in the track list (milliseconds).
        let duration_ms = match self.session.get_prop_u32(handle, prop::DURATION) {
            Ok(0) => None,
            Ok(ms) => Some(ms),
            Err(e) => {
                self.note_failure(e);
                None
            }
        };
        DeviceTrack {
            handle,
            filename,
            size,
            title,
            artist,
            album,
            album_artist,
            track,
            format,
            duration_ms,
        }
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
    pub fn push_track(&mut self, path: &Path, data: &[u8], t: &TrackTags) -> Result<u32> {
        self.push_track_reported(path, data, t, true, &mut |_, _| {})
    }

    /// ZUNE-GUI addition: `push_track` with byte-level upload progress, so a single
    /// large file still moves the progress bar. Identical wire behaviour.
    ///
    /// `replace` keeps the historical rule — an object the device already holds under
    /// the same name is deleted first. Callers that already know the name is free (a
    /// sync plan, checked against the live library) pass `false`, which skips the
    /// lookup in [`Self::find_track`]: that walk is one `GetObjectInfo` per object on
    /// the device, measured at ~2 minutes for a 2425-track library, and it is paid
    /// once per pushed file.
    pub fn push_track_reported(
        &mut self,
        path: &Path,
        data: &[u8],
        t: &TrackTags,
        replace: bool,
        progress: &mut dyn FnMut(usize, usize),
    ) -> Result<u32> {
        let stem = path
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        if replace {
            if let Some(old) = self.find_track(&stem)? {
                self.session.delete_object(old)?;
            }
        }

        let filename = tags::stored_name(path);
        let handle = self
            .session
            .create_object(ROOT, fmt::MP3, &filename, data.len() as u32, 0, 0, 0)
            .with_context(|| format!("creating object for {filename}"))?;
        self.session
            .push_object_data_with_progress(data, |sent, total| progress(sent, total))?;

        // Metadata the Zune shows; failures here are not fatal (the track is
        // already on the device, it would just show ungrouped).
        let mut warn = |e: anyhow::Error| eprintln!("    ! metadata: {e}");
        if let Err(e) = self.session.set_prop_str(handle, prop::NAME, &t.title) {
            warn(e);
        }
        for (p, v) in [
            (prop::ARTIST, t.artist.as_deref()),
            (prop::ALBUM, t.album.as_deref()),
            (prop::ALBUM_ARTIST, t.album_artist.as_deref()),
        ] {
            if let Some(v) = v {
                if let Err(e) = self.session.set_prop_str(handle, p, v) {
                    warn(e);
                }
            }
        }
        if let Some(n) = t.track {
            if let Err(e) = self.session.set_prop_u16(handle, prop::TRACK, n) {
                warn(e);
            }
        }
        Ok(handle)
    }

    pub fn remove(&mut self, handle: u32) -> Result<()> {
        self.session.delete_object(handle)
    }

    /// Push one photo: fitted to the screen, stored as `<stem>.jpg` in `Pictures`.
    pub fn push_photo(&mut self, photo: &crate::core::media::Photo) -> Result<u32> {
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

    // ZUNE-GUI: made `pub` for cover resolution, and made **dual-path**.
    //
    // `ObjectReferences` can be read two ways: the MTP operation GetObjectReferences
    // (0x9810) and the ObjectReferences *property* (0xDC41, via GetObjectPropValue).
    // Firmware may implement one and not the other — and if 0x9810 is refused, every
    // album silently looks like it references nothing, which is indistinguishable from
    // "this device has no cover art". Try both, and remember which one worked.
    pub fn refs(&mut self, handle: u32) -> Vec<u32> {
        if self.last_refs_path != RefsPath::Property {
            match self.session.object_refs(handle) {
                Ok(v) if !v.is_empty() => {
                    self.last_refs_path = RefsPath::Opcode;
                    return v;
                }
                Ok(_) => {
                    self.last_refs_error.get_or_insert_with(|| {
                        "GetObjectReferences (0x9810) returned an empty list".to_owned()
                    });
                }
                Err(e) => {
                    self.last_refs_error = Some(format!("GetObjectReferences (0x9810): {e:#}"));
                    self.note_failure(e);
                }
            }
        }
        match self.session.get_prop_raw_public(handle, prop::OBJECT_REFS) {
            Ok(raw) => match crate::core::mtp::parse_u32_array(&raw) {
                Ok(v) if !v.is_empty() => {
                    self.last_refs_path = RefsPath::Property;
                    v
                }
                _ => {
                    self.last_refs_path = RefsPath::Failed;
                    Vec::new()
                }
            },
            Err(e) => {
                if self.last_refs_error.is_none() {
                    self.last_refs_error =
                        Some(format!("ObjectReferences property (0xDC41): {e:#}"));
                }
                self.last_refs_path = RefsPath::Failed;
                Vec::new()
            }
        }
    }

    /// ZUNE-GUI addition: every abstract album with its references, in device order
    /// (tracks first, cover art last — the order `sync_album` writes them in).
    pub fn albums_with_refs(&mut self) -> Result<Vec<(String, u32, Vec<u32>)>> {
        let albums: Vec<(String, u32)> = self
            .groups()?
            .into_iter()
            .filter(|g| g.kind == "album")
            .map(|g| (g.name.unwrap_or(g.filename), g.handle))
            .collect();
        let mut out = Vec::with_capacity(albums.len());
        for (name, handle) in albums {
            let refs = self.refs(handle);
            out.push((name, handle, refs));
        }
        Ok(out)
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
            let converted = crate::core::media::to_mp3(path)
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
                progress("  · no cover art found");
                None
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
        self.send_cmd(crate::core::mtp::OP_GET_OBJECT_PROP, &[handle, prop as u32])?;
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
pub fn derive_album_name(dir: &Path) -> String {
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
pub fn pick_cover(dir: &Path, explicit: Option<&Path>) -> Option<PathBuf> {
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
