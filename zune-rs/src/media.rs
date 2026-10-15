//! Format conversion: whatever the Zune can actually play.
//!
//! The device plays MP3/WMA/AAC audio, WMV/MP4 video and **baseline** JPEG
//! photos fitted to its 320x240 screen. Anything else is converted with ffmpeg
//! (or, for photos, re-encoded here — a large or progressive JPEG lists on the
//! device but refuses to open).

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};

/// A converted payload plus the name it should be stored under.
pub struct Converted {
    pub name: String,
    pub data: Vec<u8>,
}

/// Locate ffmpeg: PATH first, then the usual Homebrew locations.
pub fn ffmpeg() -> Result<PathBuf> {
    if let Ok(p) = which("ffmpeg") {
        return Ok(p);
    }
    for c in ["/opt/homebrew/bin/ffmpeg", "/usr/local/bin/ffmpeg", "/usr/bin/ffmpeg"] {
        let p = PathBuf::from(c);
        if p.is_file() {
            return Ok(p);
        }
    }
    bail!("ffmpeg not found — install it (brew install ffmpeg / apt install ffmpeg)")
}

pub fn which(program: &str) -> Result<PathBuf> {
    let path = std::env::var_os("PATH").context("PATH is not set")?;
    for dir in std::env::split_paths(&path) {
        let cand = dir.join(program);
        if cand.is_file() {
            return Ok(cand);
        }
    }
    bail!("{program} not found on PATH")
}

fn run(cmd: &mut Command, what: &str) -> Result<()> {
    let out = cmd
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .output()
        .with_context(|| format!("running ffmpeg for {what}"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        let tail: Vec<&str> = err.lines().rev().take(6).collect();
        bail!(
            "ffmpeg failed for {what}: {}",
            tail.into_iter().rev().collect::<Vec<_>>().join("\n  ")
        );
    }
    Ok(())
}

fn temp_path(ext: &str) -> Result<PathBuf> {
    let dir = std::env::temp_dir();
    let unique = format!(
        "zune-{}-{}.{ext}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    );
    Ok(dir.join(unique))
}

// ── audio ────────────────────────────────────────────────────────────────────

/// True when the file starts with an ID3v2.4 tag. The Zune ignores those, so the
/// track plays untitled unless the tag is rewritten as v2.3.
fn is_id3v24(data: &[u8]) -> bool {
    data.len() > 4 && &data[..3] == b"ID3" && data[3] == 4
}

/// Produce MP3 bytes for the device. MP3 in / MP3 out when possible (`-c copy`
/// only, no re-encode), otherwise a 320 kbit/s transcode.
pub fn to_mp3(path: &Path) -> Result<Converted> {
    let stem = path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    let name = format!("{stem}.mp3");
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    let raw = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;

    if ext == "mp3" && !is_id3v24(&raw) {
        return Ok(Converted { name, data: raw });
    }

    let ff = ffmpeg()?;
    let tmp = temp_path("mp3")?;
    let mut cmd = Command::new(&ff);
    if ext == "mp3" {
        // Only the tag version needs fixing: stream-copy and re-tag.
        cmd.args(["-i"]).arg(path).args(["-c:a", "copy", "-id3v2_version", "3"]);
    } else {
        cmd.args(["-i"]).arg(path).args([
            "-b:a",
            "320k",
            "-map_metadata",
            "0",
            "-id3v2_version",
            "3",
            "-vn",
        ]);
    }
    cmd.arg("-y").arg(&tmp);
    let res = run(&mut cmd, &format!("audio {}", path.display()));
    let data = res.and_then(|_| {
        std::fs::read(&tmp).with_context(|| format!("reading converted {}", tmp.display()))
    });
    let _ = std::fs::remove_file(&tmp);
    Ok(Converted { name, data: data? })
}

// ── video ────────────────────────────────────────────────────────────────────

const VIDEO_EXTS: [&str; 8] = ["wmv", "mp4", "m4v", "avi", "mov", "mkv", "webm", "3gp"];

pub fn is_video(path: &Path) -> bool {
    ext_of(path)
        .map(|e| VIDEO_EXTS.contains(&e.as_str()))
        .unwrap_or(false)
}

/// 320x240 letterboxed WMV2/WMAv2, max 30 fps — verified playing on a Zune 30.
pub fn to_wmv(path: &Path, subtitles: Option<&Path>) -> Result<Converted> {
    let stem = path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    let name = format!("{stem}.wmv");
    let ext = ext_of(path).unwrap_or_default();

    if ext == "wmv" {
        let data = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
        return Ok(Converted { name, data });
    }

    let ff = ffmpeg()?;
    let tmp = temp_path("wmv")?;
    let mut vf = String::from(
        "scale=320:240:force_original_aspect_ratio=decrease:force_divisible_by=2,\
         pad=320:240:(ow-iw)/2:(oh-ih)/2:black,setsar=1",
    );
    if let Some(s) = subtitles {
        vf = format!("subtitles='{}',{vf}", s.display());
    }

    let mut cmd = Command::new(&ff);
    // -ignore_editlist only exists on the MP4/MOV demuxer and errors elsewhere.
    if matches!(ext.as_str(), "mp4" | "m4v" | "mov" | "m4a" | "3gp" | "3g2") {
        cmd.args(["-ignore_editlist", "1"]);
    }
    cmd.args(["-i"])
        .arg(path)
        .args([
            "-vf", &vf, "-fpsmax", "30", "-c:v", "wmv2", "-b:v", "384k", "-minrate", "192k",
            "-maxrate", "512k", "-bufsize", "512k", "-c:a", "wmav2", "-b:a", "128k", "-ar",
            "44100", "-ac", "2", "-map_metadata", "-1",
        ])
        .arg("-y")
        .arg(&tmp);
    let res = run(&mut cmd, &format!("video {}", path.display()));
    let data = res.and_then(|_| {
        std::fs::read(&tmp).with_context(|| format!("reading converted {}", tmp.display()))
    });
    let _ = std::fs::remove_file(&tmp);
    Ok(Converted { name, data: data? })
}

// ── photos ───────────────────────────────────────────────────────────────────

const IMAGE_EXTS: [&str; 10] = ["jpg", "jpeg", "png", "bmp", "gif", "tif", "tiff", "webp", "heic", "heif"];

pub fn is_image(path: &Path) -> bool {
    ext_of(path)
        .map(|e| IMAGE_EXTS.contains(&e.as_str()))
        .unwrap_or(false)
}

pub struct Photo {
    pub name: String,
    pub data: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

/// Fit an image onto the Zune's screen, letterboxed on black, as a baseline JPEG.
///
/// Landscape images get 320x240, portrait ones 240x320 — the device opens
/// full-screen either way. HEIC/HEIF goes through `sips` on macOS.
pub fn to_zune_photo(path: &Path) -> Result<Photo> {
    let stem = path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    let ext = ext_of(path).unwrap_or_default();

    let source = if matches!(ext.as_str(), "heic" | "heif") {
        let tmp = temp_path("jpg")?;
        let sips = which("sips").context(
            "HEIC needs macOS `sips` (or convert the file to JPEG first)",
        )?;
        let mut cmd = Command::new(sips);
        cmd.args(["-s", "format", "jpeg"]).arg(path).arg("--out").arg(&tmp);
        let r = run(&mut cmd, &format!("HEIC {}", path.display()));
        if r.is_err() {
            let _ = std::fs::remove_file(&tmp);
            return Err(r.unwrap_err());
        }
        tmp
    } else {
        path.to_path_buf()
    };

    let decoded = image::open(&source);
    if source != path {
        let _ = std::fs::remove_file(&source);
    }
    let img = decoded.with_context(|| format!("decoding {}", path.display()))?;

    let (tw, th) = if img.width() >= img.height() { (320u32, 240u32) } else { (240, 320) };
    let fitted = img.resize(tw, th, image::imageops::FilterType::Lanczos3).to_rgb8();

    let mut canvas = image::RgbImage::from_pixel(tw, th, image::Rgb([0, 0, 0]));
    let (ox, oy) = ((tw - fitted.width()) / 2, (th - fitted.height()) / 2);
    image::imageops::overlay(&mut canvas, &fitted, ox.into(), oy.into());

    let mut data = Vec::new();
    let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut data, 90);
    canvas
        .write_with_encoder(encoder)
        .with_context(|| format!("encoding JPEG for {}", path.display()))?;

    Ok(Photo { name: format!("{stem}.jpg"), data, width: tw, height: th })
}

fn ext_of(p: &Path) -> Option<String> {
    p.extension().map(|e| e.to_string_lossy().to_lowercase())
}

/// Files directly inside a directory that satisfy `keep`, in name order.
pub fn files_in(dir: &Path, keep: impl Fn(&Path) -> bool) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
        let path = entry?.path();
        if path.is_file() && keep(&path) {
            out.push(path);
        }
    }
    out.sort();
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn id3v24_is_detected() {
        assert!(is_id3v24(b"ID3\x04\x00\x00abc"));
        assert!(!is_id3v24(b"ID3\x03\x00\x00abc"));
        assert!(!is_id3v24(b"\xff\xfb\x90"));
    }

    #[test]
    fn photo_fits_the_screen() {
        let dir = std::env::temp_dir();
        let p = dir.join("zune-media-test.png");
        let img = image::RgbImage::from_pixel(1000, 400, image::Rgb([200, 30, 30]));
        img.save(&p).unwrap();
        let photo = to_zune_photo(&p).unwrap();
        assert_eq!((photo.width, photo.height), (320, 240));
        assert_eq!(photo.name, "zune-media-test.jpg");
        // baseline SOI + SOF0 present, no progressive SOF2
        assert_eq!(&photo.data[..2], &[0xFF, 0xD8]);
        assert!(photo.data.windows(2).any(|w| w == [0xFF, 0xC0]));
        assert!(!photo.data.windows(2).any(|w| w == [0xFF, 0xC2]));
        let _ = std::fs::remove_file(&p);
    }
}
