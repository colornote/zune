//! Snapshot of `zune-rs/src/tags.rs` (read-only copy — do NOT edit zune-rs from here).
//!
//! Provenance: re-synced 2026-10-15T13:16:15Z from /home/qyd/zune/zune-rs/src/tags.rs
//! sha256: 007d2c1d0279ea5371dfe30e3326d8a1a28f64b3f5cb5f4e6e2df1f024acb74f
//! Paths rewritten: crate::{mtp,mtpz,usb,device,library,media,tags} -> crate::core::*.
//! Re-synced to pick up the upstream MTPZ CMAC fix (the previous hand-rolled
//! doubling step produced wrong MACs) and the library property-failure reporting.
//! Track tags, read straight from the file (mp3 ID3, m4a/iTunes, FLAC, Ogg, WAV…).
//!
//! The Zune has no notion of tags: it displays what we set as object properties, so
//! this is the source of truth for Name/Artist/Album/AlbumArtist/Track.

use std::path::Path;

use lofty::file::TaggedFileExt;
use lofty::prelude::{Accessor, ItemKey};
use lofty::probe::Probe;

#[derive(Debug, Default, Clone)]
pub struct TrackTags {
    pub title: String,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub album_artist: Option<String>,
    pub track: Option<u16>,
}

/// Read tags; the title falls back to the file stem when the file has none.
pub fn read(path: &Path) -> TrackTags {
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    let mut out = TrackTags { title: stem, ..Default::default() };

    let Ok(tagged) = Probe::open(path).and_then(|p| p.read()) else {
        return out;
    };
    let tag = tagged.primary_tag().or_else(|| tagged.first_tag());
    let Some(tag) = tag else { return out };

    if let Some(t) = tag.title().map(|v| v.trim().to_string()).filter(|v| !v.is_empty()) {
        out.title = t;
    }
    let clean = |v: Option<std::borrow::Cow<'_, str>>| {
        v.map(|v| v.trim().to_string()).filter(|v| !v.is_empty())
    };
    out.artist = clean(tag.artist());
    out.album = clean(tag.album());
    out.album_artist = clean(tag.get_string(ItemKey::AlbumArtist).map(|v| v.to_string().into()));
    out.track = tag.track().map(|n| n.min(u16::MAX as u32) as u16);
    out
}

/// The firmware strips dots from object names ("01. Song" → "01 Song"), so both
/// sides of a dedup comparison are normalised the same way.
pub fn device_key(name: &str) -> String {
    name.to_lowercase().replace('.', "")
}

/// What the device will show for a converted track: the file stem plus `.mp3`.
pub fn stored_name(path: &Path) -> String {
    format!(
        "{}.mp3",
        path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dots_are_stripped() {
        assert_eq!(device_key("01. Song.MP3"), "01 songmp3");
        assert_eq!(device_key("01 Song.mp3"), "01 songmp3");
    }
}
