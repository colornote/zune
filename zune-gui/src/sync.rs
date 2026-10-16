//! One-way planning: what is on the Mac but not yet on the Zune.
//!
//! Matching is the whole problem here. A Zune renames what it ingests to
//! `"{track:02} {title}.mp3"` (measured on a real library: 99.9 % of 2425 objects),
//! so a local `01. 旅行的意义.mp3` and the device's `01 01.旅行的意义.mp3` share
//! neither a file name nor a title string. Three keys are derived on both sides
//! instead, and a local file counts as present when any one of them matches:
//!
//!   1. the file name, normalised the way the firmware does (`tags::device_key`);
//!   2. the name the firmware itself would have derived, `{track:02} {title}.mp3`;
//!   3. a tag signature — title, artist, album, with a leading track number removed
//!      from the title, lowercased and stripped of punctuation.
//!
//! Nothing here touches USB or the UI, so the whole thing is testable and a plan can
//! be computed while the device is unplugged.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use crate::core::tags;
use crate::locallib::{name_key, LocalEntry, LocalIndex};
use crate::model::Track;

/// The device's library, reduced to the set of keys a local file can match by.
pub struct DeviceIndex {
    keys: HashSet<String>,
}

impl DeviceIndex {
    pub fn contains(&self, key: &str) -> bool {
        !key.is_empty() && self.keys.contains(key)
    }

    pub fn len(&self) -> usize {
        self.keys.len()
    }

    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }
}

/// Every key the device answers to: its own file name, the name its tags imply, and
/// the tag signature.
pub fn device_index(tracks: &[Track]) -> DeviceIndex {
    let mut keys = HashSet::new();
    for track in tracks {
        if !track.filename.is_empty() {
            keys.insert(name_key(&track.filename));
            let stem = std::path::Path::new(&track.filename)
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default();
            if !stem.is_empty() {
                keys.insert(name_key(&format!("{stem}.mp3")));
            }
        }
        if !track.title.trim().is_empty() {
            keys.insert(name_key(&predicted_name(track.track_no, &track.title)));
            let signature = signature(&track.title, &track.artist, &track.album);
            if !signature.is_empty() {
                keys.insert(signature);
            }
        }
    }
    DeviceIndex { keys }
}

/// Whether the device already holds this file.
pub fn is_present(device: &DeviceIndex, entry: &LocalEntry) -> bool {
    if device.contains(&entry.stem_key()) {
        return true;
    }
    if !entry.title.trim().is_empty() {
        if device.contains(&name_key(&predicted_name(entry.track, &entry.title))) {
            return true;
        }
        if device.contains(&signature(&entry.title, entry.artist(), entry.album())) {
            return true;
        }
    }
    false
}

/// One file the plan would push.
#[derive(Debug, Clone)]
pub struct PlanItem {
    pub path: PathBuf,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub track: Option<u16>,
    pub size: u64,
}

impl PlanItem {
    pub fn label(&self) -> String {
        match (self.title.trim(), self.artist.trim()) {
            ("", "") => self
                .path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default(),
            ("", artist) => artist.to_owned(),
            (title, "") => title.to_owned(),
            (title, artist) => format!("{title} · {artist}"),
        }
    }
}

/// What a sync would do, computed without touching the device.
#[derive(Debug, Clone, Default)]
pub struct SyncPlan {
    pub to_push: Vec<PlanItem>,
    /// Sum of the files' sizes on disk. A rough estimate for the device: MP3 is
    /// pushed as-is, anything else is transcoded and usually comes out smaller.
    pub bytes: u64,
    /// Local files the device already has (by any of the three keys).
    pub present: usize,
    /// Local files that map to a device name another local file already claimed.
    /// Only the first is pushed.
    pub duplicates: Vec<PathBuf>,
    pub local_files: usize,
    pub sources: Vec<PathBuf>,
    pub scanned_at: i64,
}

impl SyncPlan {
    pub fn is_empty(&self) -> bool {
        self.to_push.is_empty()
    }

    /// The album a push should be grouped under, for the plan list's headers.
    pub fn album_of(item: &PlanItem) -> String {
        if item.album.trim().is_empty() {
            "Unknown Album".to_owned()
        } else {
            item.album.clone()
        }
    }
}

/// Build the plan: every indexed file the device does not already have.
pub fn plan(index: &LocalIndex, device: &DeviceIndex, sources: &[PathBuf]) -> SyncPlan {
    let mut plan = SyncPlan {
        local_files: index.entries.len(),
        sources: sources.to_vec(),
        scanned_at: index.scanned_at,
        ..Default::default()
    };
    // Two local files can want the same device name (the same song in two folders, or
    // a remaster beside the original). Pushing both would make the second replace the
    // first, so the first one wins and the rest are reported.
    let mut claimed: HashMap<String, PathBuf> = HashMap::new();
    for entry in &index.entries {
        let stem_key = entry.stem_key();
        if let Some(first) = claimed.get(&stem_key) {
            log::debug!(
                "{} and {} map to one device name — the first wins",
                first.display(),
                entry.path.display()
            );
            plan.duplicates.push(entry.path.clone());
            continue;
        }
        claimed.insert(stem_key, entry.path.clone());
        if is_present(device, entry) {
            plan.present += 1;
            continue;
        }
        plan.bytes += entry.size;
        plan.to_push.push(PlanItem {
            path: entry.path.clone(),
            title: entry.title.clone(),
            artist: entry.artist().to_owned(),
            album: entry.album().to_owned(),
            track: entry.track,
            size: entry.size,
        });
    }
    plan.to_push.sort_by(|a, b| {
        a.artist
            .to_lowercase()
            .cmp(&b.artist.to_lowercase())
            .then(a.album.to_lowercase().cmp(&b.album.to_lowercase()))
            .then(a.track.unwrap_or(0).cmp(&b.track.unwrap_or(0)))
            .then(a.title.to_lowercase().cmp(&b.title.to_lowercase()))
    });
    plan
}

// ── the keys ────────────────────────────────────────────────────────────────

/// The name the firmware derives from a file's tags: `{track:02} {title}.mp3`, or
/// `{title}.mp3` when the file has no track number. Measured against 2425 device
/// objects: this reproduces 99.9 % of the names the device actually stored.
///
/// The title is *not* trimmed: the device stores `10  吻别.mp3` for a title that
/// starts with a space, and every comparison goes through [`name_key`] anyway.
pub fn predicted_name(track: Option<u16>, title: &str) -> String {
    match track {
        Some(number) if number > 0 => format!("{number:02} {title}.mp3"),
        _ => format!("{title}.mp3"),
    }
}

/// A tag identity that survives the firmware's renaming: title, artist and album,
/// lowercased, stripped of punctuation, with a leading track number removed from the
/// title. Empty when there is no title to key on.
pub fn signature(title: &str, artist: &str, album: &str) -> String {
    let title = normalised_title(title);
    if title.is_empty() {
        return String::new();
    }
    format!("{}|{}|{}", title, normalise(artist), normalise(album))
}

fn normalise(text: &str) -> String {
    let folded: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    crate::coverfetch::normalise(&folded)
}

/// `"01. 旅行的意义"` → `"旅行的意义"`, `"07 - Adore"` → `"Adore"`. The numbering has
/// to go: the device stores it in the file name *and* often keeps it in the title,
/// while the file on the Mac may carry it in neither place.
fn normalised_title(title: &str) -> String {
    normalise(strip_leading_number(title.trim()))
}

fn strip_leading_number(text: &str) -> &str {
    let text = text.trim();
    let digits = text.bytes().take_while(|b| b.is_ascii_digit()).count();
    if digits == 0 || digits > 3 {
        return text;
    }
    let rest = text[digits..].trim_start_matches(|c: char| {
        c == '.' || c == '-' || c == '–' || c == '—' || c == '_' || c.is_whitespace()
    });
    if rest.is_empty() {
        text
    } else {
        rest
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::locallib::LocalEntry;
    use crate::model::Track;
    use std::path::PathBuf;

    fn device_track(filename: &str, title: &str, artist: &str, album: &str, track: Option<u16>) -> Track {
        Track {
            handle: 1,
            filename: filename.to_owned(),
            size: 0,
            format: 0x3009,
            title: title.to_owned(),
            artist: artist.to_owned(),
            album: album.to_owned(),
            track_no: track,
            duration: None,
        }
    }

    fn local(title: &str, artist: &str, album: &str, track: Option<u16>) -> LocalEntry {
        LocalEntry {
            path: PathBuf::from(format!("/m/{title}.mp3")),
            size: 1000,
            mtime: 0,
            title: title.to_owned(),
            artist: Some(artist.to_owned()),
            album: Some(album.to_owned()),
            track,
        }
    }

    #[test]
    fn a_renamed_device_file_is_still_recognised() {
        // The shape on the device: the firmware prefixed the track number to a title
        // that already carried one.
        let device = device_index(&[device_track(
            "01 01.旅行的意义.mp3",
            "01.旅行的意义",
            "陈绮贞",
            "04.华丽的冒险(2005.09)",
            Some(1),
        )]);
        // The local file, as its tags read on the Mac: no numbering in the title.
        assert!(is_present(&device, &local("旅行的意义", "陈绮贞", "04.华丽的冒险(2005.09)", Some(1))));
        // Another song on the same album is not.
        assert!(!is_present(&device, &local("腐朽", "陈绮贞", "04.华丽的冒险(2005.09)", Some(2))));
    }

    #[test]
    fn an_unnumbered_file_matches_by_its_name() {
        let device = device_index(&[device_track("Adore.mp3", "Adore", "Smashing Pumpkins", "B-Sides", None)]);
        let mut entry = local("Adore", "Smashing Pumpkins", "B-Sides", None);
        entry.path = PathBuf::from("/m/Adore.mp3");
        assert!(is_present(&device, &entry));
    }

    #[test]
    fn a_missing_track_number_does_not_hide_a_match() {
        let device = device_index(&[device_track("07 02-why.mp3", "02-why", "Avril Lavigne", "Let Go", Some(7))]);
        assert!(is_present(&device, &local("02-why", "Avril Lavigne", "Let Go", None)));
    }

    #[test]
    fn the_predicted_name_is_what_the_firmware_writes() {
        assert_eq!(predicted_name(Some(1), "旅行的意义"), "01 旅行的意义.mp3");
        assert_eq!(predicted_name(Some(10), " 吻别"), "10  吻别.mp3");
        assert_eq!(predicted_name(None, "Adore"), "Adore.mp3");
        assert_eq!(predicted_name(Some(0), "Adore"), "Adore.mp3");
    }

    #[test]
    fn a_leading_number_is_stripped_only_from_the_title() {
        assert_eq!(strip_leading_number("01. 旅行的意义"), "旅行的意义");
        assert_eq!(strip_leading_number("07 - Adore"), "Adore");
        assert_eq!(strip_leading_number("1943"), "1943", "a title that is just a year stays");
        assert_eq!(strip_leading_number("1234. Song"), "1234. Song", "four digits are not a track number");
        assert_eq!(strip_leading_number("  02  曲"), "曲");
    }

    #[test]
    fn the_plan_splits_present_from_new_and_keeps_album_order() {
        let device = device_index(&[device_track("01 01.旅行的意义.mp3", "01.旅行的意义", "陈绮贞", "华丽的冒险", Some(1))]);
        let index = LocalIndex {
            version: 1,
            scanned_at: 5,
            entries: vec![
                local("旅行的意义", "陈绮贞", "华丽的冒险", Some(1)),
                local("腐朽", "陈绮贞", "华丽的冒险", Some(2)),
                local("花的姿态", "陈绮贞", "华丽的冒险", Some(3)),
            ],
        };
        let plan = plan(&index, &device, &[PathBuf::from("/m")]);
        assert_eq!(plan.present, 1);
        assert_eq!(plan.to_push.len(), 2);
        assert_eq!(plan.bytes, 2000);
        assert_eq!(plan.to_push[0].title, "腐朽");
        assert_eq!(plan.to_push[1].title, "花的姿态");
        assert!(plan.duplicates.is_empty());
    }

    #[test]
    fn two_files_with_one_device_name_keep_the_first() {
        let device = device_index(&[]);
        let mut a = local("曲", "歌手", "专辑", Some(1));
        a.path = PathBuf::from("/m/a/01 曲.mp3");
        let mut b = local("曲", "歌手", "专辑", Some(1));
        b.path = PathBuf::from("/m/b/01 曲.mp3");
        let index = LocalIndex { version: 1, scanned_at: 0, entries: vec![a, b] };
        let plan = plan(&index, &device, &[PathBuf::from("/m")]);
        assert_eq!(plan.to_push.len(), 1);
        assert_eq!(plan.duplicates.len(), 1);
        assert_eq!(plan.duplicates[0].to_string_lossy(), "/m/b/01 曲.mp3");
    }

    #[test]
    fn an_empty_device_pushes_everything() {
        let index = LocalIndex {
            version: 1,
            scanned_at: 0,
            entries: vec![local("一", "a", "x", Some(1)), local("二", "a", "x", Some(2))],
        };
        let plan = plan(&index, &device_index(&[]), &[]);
        assert_eq!(plan.to_push.len(), 2);
        assert!(plan.is_empty() == false);
    }
}
