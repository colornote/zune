//! Plain data shared between the background device thread and the UI thread.
//!
//! Nothing here touches USB: the worker owns the session, the UI owns the
//! snapshots it is handed.

use std::path::PathBuf;

/// Connection state as the UI sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Link {
    Disconnected,
    Connecting,
    Connected,
}

impl Link {
    pub fn label(self) -> &'static str {
        match self {
            Link::Disconnected => "no device",
            Link::Connecting => "connecting…",
            Link::Connected => "connected",
        }
    }
}

/// Everything the device bar shows. Filled in progressively: identity first,
/// storage and library counts once the MTPZ session is trusted.
#[derive(Debug, Clone, Default)]
pub struct DeviceSnapshot {
    /// "Zune 30" / "Zune HD", from the USB product id.
    pub name: String,
    pub manufacturer: String,
    pub serial: String,
    pub firmware: String,
    pub mtp_version: String,
    pub topology: String,
    pub storage_desc: String,
    pub capacity: Option<u64>,
    pub free: Option<u64>,
    pub tracks: usize,
    pub albums: usize,
    pub playlists: usize,
    pub library_bytes: u64,
    /// "132 mp3 · 15 wma" — what the device actually stores.
    pub format_summary: String,
    pub cover_bytes: u64,
    /// Path of the MTPZ key file that was actually loaded.
    pub key_path: Option<String>,
    /// Set when the handshake could not run because no key file was found.
    pub key_error: Option<String>,
    /// Set when the link is wedged past what a USB reset can fix: the device has to be
    /// physically unplugged. Shown as a banner so nobody waits for a read that cannot
    /// succeed.
    pub needs_replug: bool,
}

impl DeviceSnapshot {
    pub fn used(&self) -> Option<u64> {
        Some(self.capacity?.saturating_sub(self.free?))
    }
}

/// Where the library currently on screen came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Source {
    /// Read from the device just now.
    #[default]
    Live,
    /// Read from the on-disk cache; the device was deliberately not re-read.
    Cached,
}

/// Provenance of the library snapshot, so the UI can say "cached · 3 min ago"
/// instead of presenting a 3-minute-old read as if it were live. Sent as its own
/// [`Event`] right after the library so the two can never disagree.
#[derive(Debug, Clone, Default)]
pub struct DataOrigin {
    pub source: Source,
    /// Unix seconds when the library was actually read from the device.
    pub read_at: Option<i64>,
    /// True while an MTP session is open (i.e. the device is connected).
    pub device_connected: bool,
    pub serial: String,
    /// One short sentence for the UI (e.g. "device not connected").
    pub note: Option<String>,
}

/// One audio object on the device.
#[derive(Debug, Clone)]
pub struct Track {
    pub handle: u32,
    pub filename: String,
    pub size: u32,
    /// MTP object format (MP3 / WMA / AAC) — the device's own library is mostly WMA.
    pub format: u16,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub track_no: Option<u16>,
    pub duration: Option<u32>,
}

impl Track {
    pub fn duration_label(&self) -> String {
        match self.duration {
            Some(seconds) if seconds > 0 => format_duration(seconds),
            _ => "—".to_string(),
        }
    }

    /// What the Zune itself would list the track as.
    pub fn display_title(&self) -> &str {
        if self.title.is_empty() {
            &self.filename
        } else {
            &self.title
        }
    }
}

/// Where an album's cover art was found.
///
/// This exists because the device's albums were written over years by Microsoft's
/// own software, so "the last reference is the cover" is *our* convention, not the
/// device's. Recording provenance is what turns "the cover is missing" into an
/// explainable fact.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoverSource {
    /// A reference whose object format is CoverArt (0x3802).
    RefCoverArt,
    /// A reference whose object format is JPEG (0x3801).
    RefJpeg,
    /// A reference that is not a track, with an unreadable/unknown format.
    RefOther,
    /// An image object whose own references point at the album (the link stored on the
    /// image side — the shape a Zune 30 actually uses).
    ReverseRef,
    /// Matched by name against the device's image objects.
    FolderMatch,
    /// The embedded picture of a local audio file, or a cover image beside one.
    LocalFile,
    /// Downloaded (iTunes Search) and cached on disk.
    Online,
    /// Nothing on the device looked like a cover for this album.
    None,
}

impl CoverSource {
    pub fn label(self) -> &'static str {
        match self {
            CoverSource::RefCoverArt => "album reference (0x3802)",
            CoverSource::RefJpeg => "album reference (JPEG)",
            CoverSource::RefOther => "album reference (unclassified)",
            CoverSource::ReverseRef => "reverse reference from the image object",
            CoverSource::FolderMatch => "image name match",
            CoverSource::LocalFile => "local file",
            CoverSource::Online => "online (iTunes)",
            CoverSource::None => "none found",
        }
    }

    pub fn is_found(self) -> bool {
        self != CoverSource::None
    }
}

/// An album tile: an abstract album when the device has one, otherwise the grouping
/// the tracks' own tags describe.
#[derive(Debug, Clone)]
pub struct Album {
    pub name: String,
    /// Handle of the abstract album object (0xBA03), when it exists.
    pub handle: Option<u32>,
    /// Album artist: the album object's own property, else the dominant track artist.
    pub artist: String,
    /// Release year, when the device stores a date for the album.
    pub year: Option<u32>,
    pub track_handles: Vec<u32>,
    pub tracks: usize,
    /// Sum of the member tracks' sizes.
    pub bytes: u64,
    /// Sum of the member tracks' durations, in seconds.
    pub duration: u32,
    /// Cover-art object handle on the device, when one was found.
    pub cover_handle: Option<u32>,
    /// Key for the UI's cover map: the device handle, or a synthetic key derived from the
    /// artist + album when the device has no art (see `coverfetch::synthetic_key`).
    pub cover_key: u32,
    /// Where the cover came from — on the device, from a local file, or online.
    pub cover_source: CoverSource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Info,
    Warn,
    Error,
    Ok,
}

#[derive(Debug, Clone)]
pub struct LogLine {
    pub level: Level,
    pub text: String,
}

impl LogLine {
    pub fn info(text: impl Into<String>) -> Self {
        Self { level: Level::Info, text: text.into() }
    }
    pub fn ok(text: impl Into<String>) -> Self {
        Self { level: Level::Ok, text: text.into() }
    }
    pub fn warn(text: impl Into<String>) -> Self {
        Self { level: Level::Warn, text: text.into() }
    }
    pub fn error(text: impl Into<String>) -> Self {
        Self { level: Level::Error, text: text.into() }
    }
}

/// Progress of the running task. `items_*` counts files, `bytes_*` counts the
/// payload currently on the wire, so a single big file still moves the bar.
#[derive(Debug, Clone, Default)]
pub struct Progress {
    pub task: String,
    pub step: String,
    pub items_done: usize,
    pub items_total: usize,
    pub bytes_done: u64,
    pub bytes_total: u64,
}

impl Progress {
    /// 0.0..=1.0, weighted so that the per-file bar dominates and the current
    /// file's bytes only fill the remainder of its own slice.
    pub fn fraction(&self) -> f32 {
        let items = self.items_total.max(1) as f32;
        let done = self.items_done as f32;
        let within = if self.bytes_total > 0 {
            (self.bytes_done as f32 / self.bytes_total as f32).clamp(0.0, 1.0)
        } else {
            0.0
        };
        ((done + within) / items).clamp(0.0, 1.0)
    }
}

/// `4:18`, or `1:02:05` once it runs past an hour. Shared by track rows and album
/// totals so the two can never disagree.
pub fn format_duration(seconds: u32) -> String {
    let (h, m, s) = (seconds / 3600, (seconds % 3600) / 60, seconds % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

/// A decoded cover-art image, ready to become a texture on the UI thread.
#[derive(Clone)]
pub struct CoverImage {
    pub width: usize,
    pub height: usize,
    pub rgba: Vec<u8>,
}

/// Worker → UI.
pub enum Event {
    Link(Link),
    Snapshot(Box<DeviceSnapshot>),
    Library { tracks: Vec<Track>, albums: Vec<Album> },
    /// Where the `Library` event that preceded this one came from. Always sent
    /// after a library so the badge never describes a library that is not shown.
    Origin(Box<DataOrigin>),
    Log(LogLine),
    Progress(Box<Progress>),
    TaskStarted { task: String, items: usize },
    TaskFinished { ok: bool, summary: String },
    Busy(bool),
    Cover {
        handle: u32,
        image: CoverImage,
        /// "device", "cached", "local file · x.mp3", "online · iTunes" …
        origin: String,
    },
    CoverFailed { handle: u32, reason: String },
    /// The background cover prefetch finished: covers for these albums are on disk now,
    /// so the Albums page can load them without another network round trip.
    CoversPrefetched { albums: usize },
    /// A WiFi scan came back: the parsed networks, plus the raw hex for the log.
    WifiScan { networks: Vec<crate::core::wifi::WifiNetwork>, hex: String },
}

/// UI → worker.
#[derive(Debug)]
pub enum Command {
    /// Try to find, open and authenticate the device once.
    Connect,
    /// Close the MTP session but keep looking for the device.
    Disconnect,
    /// Re-read storage info, tracks and albums.
    /// Re-read the device. `force` skips the cheap "has anything changed" check and
    /// always pays for the full property sweep.
    Refresh { force: bool },
    /// Delete these object handles.
    Delete(Vec<u32>),
    /// Push loose audio files (a directory means "every audio file inside it").
    PushTracks(Vec<PathBuf>),
    /// Push a directory as one album: tracks → playlist → cover → abstract album.
    PushAlbum {
        dir: PathBuf,
        name: Option<String>,
        cover: Option<PathBuf>,
        playlist: bool,
    },
    /// Download a cover-art object so it can be shown.
    FetchCover(u32),
    /// The device has no art object for this album. `track_handle` is its first track,
    /// whose own file may carry an embedded ID3 `APIC` picture — on a Zune 30 that is
    /// where the art actually is. Failing that, the local files and then the online
    /// ladder are tried, and whatever comes back is cached under `key`.
    FetchFallbackCover {
        key: u32,
        artist: String,
        album: String,
        track_handle: Option<u32>,
    },
    /// Dump the real reference/property structure of the first N albums on the
    /// device, so cover parsing can be written against facts instead of guesses.
    DiagnoseAlbums(usize),
    /// Fast cover diagnosis: reads only the image objects and a few albums, never the
    /// track library. Answers "where do the covers live and what links them to albums".
    DiagnoseCovers { images: usize, albums: usize },
    WifiScan,
    WifiTest,
    WifiProfile(Vec<u8>),
    /// Reset the USB link (the macOS recovery for a stalled bulk pipe).
    UsbReset,
    Eject,
    /// Stop after the file currently in flight.
    Cancel,
    Shutdown,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations_read_like_a_player() {
        assert_eq!(format_duration(0), "0:00");
        assert_eq!(format_duration(9), "0:09");
        assert_eq!(format_duration(59), "0:59");
        assert_eq!(format_duration(60), "1:00");
        assert_eq!(format_duration(258), "4:18");
        assert_eq!(format_duration(3725), "1:02:05");
        // The live bug: a millisecond value formatted as seconds.
        assert_eq!(format_duration(2_837_659), "788:14:19");
        // The same library once the unit is right: eleven ~4-minute tracks.
        assert_eq!(format_duration(2_838), "47:18");
    }

    #[test]
    fn a_track_without_a_duration_says_so() {
        let mut track = Track {
            handle: 1,
            filename: "x.mp3".into(),
            size: 0,
            format: 0x3009,
            title: "x".into(),
            artist: String::new(),
            album: String::new(),
            track_no: None,
            duration: None,
        };
        assert_eq!(track.duration_label(), "—");
        track.duration = Some(0);
        assert_eq!(track.duration_label(), "—");
        track.duration = Some(4 * 60 + 18);
        assert_eq!(track.duration_label(), "4:18");
    }
}
