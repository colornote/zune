#![allow(dead_code, unused_imports)] // snapshot copies carry code the GUI does not call

#![allow(dead_code, unused_imports)] // snapshot copies carry code the GUI does not call

//! Read-only snapshot of the parts of `zune-rs` the GUI needs.
//!
//! Why a copy: `zune-rs/src/usb.rs` is being changed by another agent right now, so
//! a path dependency would drag in half-finished work. Each file here carries its
//! source sha256 in the header; re-sync by re-copying the file and rewriting the
//! `crate::<mod>::` paths to `crate::core::<mod>::`.
//!
//!
//! ## Re-sync recipe
//!
//!   cd zune-rs/src && sha256sum <file>.rs          # record the new hash
//!   # re-copy the file, keep a header with the hash, then:
//!   sed -i -E 's/crate::(mtp|mtpz|usb|device|library|media|tags)::/crate::core::\1::/g' *.rs
//!   # and re-apply the `ZUNE-GUI` hunks (they all carry that marker):
//!   grep -n 'ZUNE-GUI' *.rs
//!
//! Local additions on top of the snapshot (kept clearly marked with `ZUNE-GUI`):
//!   * `mtp.rs`     — GetStorageInfo (0x1005), GetObject (0x1009), u64_at(),
//!                    the Duration property (0xDC89).
//!   * `device.rs`  — storage_info(), get_object_data(), get_prop_u32(), and a
//!                    chunked SendObject with a byte-level progress callback.
//!   * `library.rs` — DeviceTrack.duration, tracks_reported(), push_track_reported(),
//!                    `pub refs()`, albums_with_refs(), `pub pick_cover/derive_album_name`.
//!
//! Nothing here writes to `zune-rs`.

pub mod device;
pub mod library;
pub mod media;
pub mod mtp;
pub mod mtpz;
pub mod tags;
pub mod usb;
pub mod wifi;

pub use device::{DeviceInfo, ObjectInfo, Session, StorageInfo, ROOT};
pub use library::{
    audio_files, derive_album_name, pick_cover, AlbumReport, DeviceGroup, DeviceTrack, Library,
};
pub use mtpz::MtpzKeys;
pub use tags::TrackTags;
pub use usb::{device_name, find_device, is_stuck, topology, Transport};
