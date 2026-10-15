//! zune — an open Zune client.
//!
//! Speaks MTP over USB with the Zune (30 / HD) MTPZ handshake, so it can be used
//! from Linux, macOS and Windows without the Microsoft desktop software.

mod device;
mod id3;
mod library;
mod media;
mod mtp;
mod mtpz;
mod tags;
mod usb;

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

use device::{format_name, hexdump, Session, ROOT};
use library::Library;
use mtp::{fmt, prop};
use mtpz::MtpzKeys;

#[derive(Parser)]
#[command(name = "zune", version, about = "Open Zune client — music library over USB (MTP/MTPZ)")]
struct Cli {
    /// Path to the MTPZ key file (default: $MTPZ_DATA, ~/.mtpz-data, ./.mtpz-data)
    #[arg(long, global = true)]
    keys: Option<PathBuf>,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Info is counts-only by default: it must stay fast on a 2400-track library.
    Info {
        /// Also total the library size (needs a property sweep)
        #[arg(long)]
        size: bool,
    },

    /// List tracks on the device, with the metadata it will display
    Tracks {
        /// Only show tracks whose album/artist/title matches this substring
        #[arg(long)]
        grep: Option<String>,
    },

    /// List playlists and abstract albums
    Albums,

    /// List raw objects of a format: audio | video | photo | all
    List {
        #[arg(long, default_value = "audio")]
        kind: String,
    },

    /// Download one object's raw bytes (diagnosis: does the art live inside the file?)
    GetObject {
        /// Object handle, decimal or 0x-prefixed hex
        handle: String,
        /// Where to write the payload
        #[arg(short, long)]
        out: PathBuf,
        /// Read only the first N bytes (GetPartialObject) instead of the whole object
        #[arg(long)]
        bytes: Option<u32>,
    },

    /// Push audio files, transcoding anything the device cannot play
    Push {
        /// Files or directories to push
        paths: Vec<PathBuf>,
        /// Push as-is instead of treating them as audio
        #[arg(long)]
        raw: bool,
    },

    /// Push photos, fitted to the device screen (320x240 / 240x320 baseline JPEG)
    Photo {
        /// Files or directories
        paths: Vec<PathBuf>,
        /// Show what would be pushed without touching the device
        #[arg(long)]
        dry_run: bool,
    },

    /// Push videos, converted to 320x240 WMV
    Video {
        /// Files or directories
        paths: Vec<PathBuf>,
        /// Burn this subtitle file in (SRT/ASS)
        #[arg(long)]
        subs: Option<PathBuf>,
        /// Show what would be pushed without touching the device
        #[arg(long)]
        dry_run: bool,
    },

    /// Push a directory as one album: tracks + playlist + cover art + abstract album
    Album {
        /// Album directory
        dir: PathBuf,
        /// Album title (default: the folder name without its "NN - " prefix)
        #[arg(long)]
        name: Option<String>,
        /// Cover art JPEG (default: the largest JPEG in the directory)
        #[arg(long)]
        cover: Option<PathBuf>,
        /// Do not create a .pla playlist
        #[arg(long)]
        no_playlist: bool,
        /// Show what would be pushed without touching the device
        #[arg(long)]
        dry_run: bool,
    },

    /// Create a .pla playlist from local files (pushing any that are missing)
    Playlist {
        /// Playlist name (without .pla)
        name: String,
        /// Local audio files, or device file names
        tracks: Vec<String>,
        /// Only reference files already on the device
        #[arg(long)]
        device_only: bool,
    },

    /// Read tags from local files (no device needed)
    Tags {
        /// Files or directories
        paths: Vec<PathBuf>,
    },

    /// List the operation codes the device advertises (tells you what it supports)
    Ops,

    /// Dump one object's properties and references (handle, or a track name)
    Props {
        /// Object handle (`0x…`) or a file name / stem on the device
        target: String,
    },

    /// Inspect (and optionally reset) the USB link — diagnosis for a dead session
    Usb {
        /// Reset the device and wait for it to re-enumerate
        #[arg(long)]
        reset: bool,
    },

    /// Convert files to device-ready formats without touching a device
    Convert {
        /// Files or directories
        paths: Vec<PathBuf>,
        /// Output directory
        #[arg(short, long, default_value = "zune-converted")]
        out: PathBuf,
    },

    /// Delete objects
    Rm {
        /// Object handles
        handles: Vec<String>,
        /// Delete a track by file stem or name
        #[arg(long)]
        name: Option<String>,
    },

    /// WiFi control (vendor opcodes 0x9225/0x9227/0x9228)
    #[command(subcommand)]
    Wifi(WifiCommand),

    /// Close the session cleanly
    Eject,
}

#[derive(Subcommand)]
enum WifiCommand {
    /// Read back the profiles the device holds (0x9226).
    Profiles,
    /// Ask the device to scan for networks (0x9225)
    Scan,
    /// Commit a WiFi profile blob to the device (0x9227)
    SetProfile {
        /// Hex-encoded profile blob (device-side WZC/NDIS_802_11 layout)
        #[arg(long)]
        hex: Option<String>,
        /// Read the blob from a file instead
        #[arg(long)]
        file: Option<PathBuf>,
    },
    /// Run the device's connectivity test (0x9228)
    Test,
}

fn main() -> Result<()> {
    // RUST_LOG=debug (or nusb=debug) exposes the USB backend's own decisions, which
    // is the only way to see rejections that happen before a transfer reaches the host.
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let cli = Cli::parse();
    let cmd = cli.command.unwrap_or(Command::Info { size: false });
    let key_path = cli.keys.clone();

    // Tag reading, conversion and USB diagnosis are local: no device, no key file.
    match &cmd {
        Command::Tags { paths } => return cmd_tags(paths),
        Command::Convert { paths, out } => return cmd_convert(paths, out),
        Command::Usb { reset } => return cmd_usb(*reset),
        _ => {}
    }

    let (_info, mut s) = connect()?;

    let result = match cmd {
        Command::Info { size } => cmd_info(&mut s, size),
        Command::Tracks { grep } => cmd_tracks(&mut s, key_path.as_deref(), grep.as_deref()),
        Command::Ops => cmd_ops(&mut s),
        Command::Props { target } => cmd_props(&mut s, key_path.as_deref(), &target),
        Command::Albums => cmd_albums(&mut s, key_path.as_deref()),
        Command::List { kind } => cmd_list(&mut s, key_path.as_deref(), &kind),
        Command::GetObject { handle, out, bytes } => {
            cmd_get_object(&mut s, key_path.as_deref(), &handle, &out, bytes)
        }
        Command::Push { paths, raw } => cmd_push(&mut s, key_path.as_deref(), &paths, raw),
        Command::Photo { paths, dry_run } => cmd_photo(&mut s, key_path.as_deref(), &paths, dry_run),
        Command::Video { paths, subs, dry_run } => {
            cmd_video(&mut s, key_path.as_deref(), &paths, subs.as_deref(), dry_run)
        }
        Command::Album { dir, name, cover, no_playlist, dry_run } => cmd_album(
            &mut s,
            key_path.as_deref(),
            &dir,
            name.as_deref(),
            cover.as_deref(),
            !no_playlist,
            dry_run,
        ),
        Command::Playlist { name, tracks, device_only } => {
            cmd_playlist(&mut s, key_path.as_deref(), &name, &tracks, device_only)
        }
        Command::Rm { handles, name } => cmd_rm(&mut s, key_path.as_deref(), &handles, name.as_deref()),
        Command::Wifi(w) => cmd_wifi(&mut s, key_path.as_deref(), w),
        Command::Eject => Ok(()),
        Command::Tags { .. } => unreachable!(),
        Command::Convert { .. } => unreachable!(),
        Command::Usb { .. } => unreachable!(),
    };

    let _ = s.close_session();
    result
}

/// Open the device and start an MTP session.
///
/// A Zune that was left mid-transfer reports `kIOReturnNotResponding` on the very
/// first command, and on macOS the endpoint cannot be un-stalled in place — so a
/// stalled session is escalated to a device reset and one retry.
fn connect() -> Result<(usb::DeviceInfo, Session)> {
    let info = usb::find_device()?;
    eprintln!(
        "» {} connected (serial {} · {})",
        usb::device_name(&info),
        info.serial_number().unwrap_or("-"),
        usb::topology(&info)
    );

    let mut session = Session::new(open_and_clear(&info)?);
    match session.open_session() {
        Ok(()) => Ok((info, session)),
        Err(e) if usb::is_stuck(&e) => {
            eprintln!("» the MTP session did not start: {e:#}");
            eprintln!("» resetting the Zune and retrying once");
            drop(session);
            usb::reset(&info)?;
            let fresh = usb::find_device()?;
            let mut retry = Session::new(open_and_clear(&fresh)?);
            retry.open_session().context("opening MTP session after a device reset")?;
            eprintln!("» recovered after reset");
            Ok((fresh, retry))
        }
        Err(e) => Err(e).context("opening MTP session"),
    }
}

/// Open the device and clear anything an earlier session left buffered.
///
/// The Zune keeps an unread response in its bulk-IN buffer and then refuses new
/// commands until it is read, which looks exactly like a dead write. Draining costs
/// one quiet read on a clean device.
fn open_and_clear(info: &usb::DeviceInfo) -> Result<usb::Transport> {
    let mut transport = usb::Transport::open(info)?;
    match transport.drain_pending_input(std::time::Duration::from_millis(350)) {
        Ok(d) if !d.is_empty() => {
            eprintln!("» cleared {} bytes left over from an earlier session", d.len());
        }
        Ok(_) => {}
        Err(e) => eprintln!("  · drain skipped: {e}"),
    }
    Ok(transport)
}

fn cmd_usb(do_reset: bool) -> Result<()> {
    let info = usb::find_device()?;
    usb::describe(&info)?;

    if do_reset {
        println!("\nresetting the device …");
        usb::reset(&info)?;
        let fresh = usb::find_device()?;
        println!("reset ok — re-enumerated at {}", usb::topology(&fresh));
        return Ok(());
    }

    println!("\nclaiming the MTP interface …");
    match usb::Transport::open(&info) {
        Ok(t) => {
            println!(
                "interface claimed · bulk OUT {:#04x} · bulk IN {:#04x} (max packet {}) · unstalled={}",
                t.out_addr, t.in_addr, t.in_max_packet, t.unstalled
            );
            if t.unstalled {
                println!("→ the link looks ready for MTP");
            } else {
                println!("→ retry, or run `zune usb --reset`");
            }
        }
        Err(e) => println!("claim/open failed: {e:#}"),
    }

    println!("\nlink probe (no MTP traffic):");
    if let Err(e) = usb::probe(&info) {
        println!("  probe failed: {e:#}");
    }

    println!("\nbulk IN probe:");
    if let Err(e) = usb::bulk_probe(&info) {
        println!("  bulk probe failed: {e:#}");
    }
    Ok(())
}

fn auth(s: &mut Session, key_path: Option<&Path>) -> Result<()> {
    s.authenticate(&MtpzKeys::load(key_path)?)
        .context("MTPZ handshake (the Zune serves no storage until it is trusted)")?;
    Ok(())
}

fn with_library<T>(
    s: &mut Session,
    key_path: Option<&Path>,
    f: impl FnOnce(&mut Library) -> Result<T>,
) -> Result<T> {
    auth(s, key_path)?;
    let mut lib = Library::new(s)?;
    f(&mut lib)
}

// ── info ─────────────────────────────────────────────────────────────────────

fn cmd_info(s: &mut Session, with_size: bool) -> Result<()> {
    let di = s.device_info()?;
    println!("model            {}", di.model);
    println!("manufacturer     {}", di.manufacturer);
    println!("device version   {}", di.device_version);
    println!("serial           {}", di.serial_number);
    // MTP version is a fixed-point byte count: 100 == 1.00
    println!(
        "MTP version      {}.{:02}",
        di.standard_version / 100,
        di.standard_version % 100
    );
    println!("vendor ext       {:#010x} v{} ({})", di.vendor_ext_id, di.vendor_ext_version, di.vendor_ext_desc);
    println!("operations       {} opcodes", di.operations.len());

    match MtpzKeys::load(None) {
        Ok(keys) => match s.authenticate(&keys) {
            Ok(()) => {
                println!("MTPZ             authenticated");
                let ids = s.storage_ids()?;
                println!(
                    "storages         {}",
                    ids.iter().map(|i| format!("{i:#010x}")).collect::<Vec<_>>().join(", ")
                );
                let mut lib = Library::new(s)?;
                let summary = lib.summary(with_size)?;
                print!(
                    "library          {} tracks · {} albums · {} playlists",
                    summary.tracks, summary.albums, summary.playlists
                );
                match summary.bytes {
                    Some(b) => println!(" · {:.1} MB", b as f64 / 1_048_576.0),
                    None => println!(),
                }
            }
            Err(e) => println!("MTPZ             handshake failed: {e:#}"),
        },
        Err(_) => println!("MTPZ             skipped (no key file)"),
    }
    Ok(())
}

// ── listing ──────────────────────────────────────────────────────────────────

fn cmd_tracks(s: &mut Session, key_path: Option<&Path>, grep: Option<&str>) -> Result<()> {
    with_library(s, key_path, |lib| {
        let mut tracks = lib.tracks()?;
        if let Some(g) = grep {
            let g = g.to_lowercase();
            tracks.retain(|t| {
                let fields = [t.title.clone(), t.artist.clone(), t.album.clone(), Some(t.filename.clone())];
                fields
                    .into_iter()
                    .flatten()
                    .any(|v| v.to_lowercase().contains(&g))
            });
        }
        if tracks.is_empty() {
            println!("(no tracks)");
            return Ok(());
        }
        println!("{:<12} {:>4} {:<30} {:<24} {:<24} {:>8}", "HANDLE", "TRK", "TITLE", "ARTIST", "ALBUM", "SIZE");
        for t in &tracks {
            println!(
                "{:<12} {:>4} {:<30} {:<24} {:<24} {:>7.1}M",
                library::hex(t.handle),
                t.track.map(|n| n.to_string()).unwrap_or_else(|| "-".into()),
                truncate(t.title.as_deref().unwrap_or(&t.filename), 30),
                truncate(t.artist.as_deref().unwrap_or("-"), 24),
                truncate(t.album.as_deref().unwrap_or("-"), 24),
                t.size as f64 / 1_048_576.0
            );
        }
        let bytes: u64 = tracks.iter().map(|t| t.size as u64).sum();
        println!("\n{} tracks · {:.1} MB", tracks.len(), bytes as f64 / 1_048_576.0);
        if lib.prop_failures > 0 {
            println!(
                "(!) {} property reads failed — labels may be incomplete. Last error: {}",
                lib.prop_failures,
                lib.last_prop_error.as_deref().unwrap_or("-")
            );
        }
        Ok(())
    })
}

fn cmd_ops(s: &mut Session) -> Result<()> {
    let di = s.device_info()?;
    println!("{} operations advertised by the device:\n", di.operations.len());
    for op in &di.operations {
        println!("  {op:#06x}  {}", mtp::op_name(*op));
    }
    println!("\nthe ones this client depends on:");
    for (label, op) in [
        ("GetObjectPropValue", mtp::OP_GET_OBJECT_PROP),
        ("SetObjectPropValue", mtp::OP_SET_OBJECT_PROP),
        ("GetObjectReferences", mtp::OP_GET_OBJECT_REFS),
        ("SetObjectReferences", mtp::OP_SET_OBJECT_REFS),
        ("SendWMDRMPDReq (MTPZ)", mtp::OP_SEND_WMDRM_PD_REQ),
        ("wifi scan", mtp::OP_WIFI_SCAN),
        ("wifi set-profile", mtp::OP_WIFI_SET_PROFILE),
    ] {
        let yes = di.operations.contains(&op);
        println!("  {label:<24} {op:#06x}  {}", if yes { "supported" } else { "NOT advertised" });
    }
    Ok(())
}

fn cmd_props(s: &mut Session, key_path: Option<&Path>, target: &str) -> Result<()> {
    with_library(s, key_path, |lib| {
        let handle = if target.starts_with("0x") || target.starts_with("0X") {
            parse_handle(target)?
        } else {
            // A file name, with or without its extension: match the way the firmware
            // normalises object names.
            let stem = Path::new(target)
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| target.to_string());
            let want = tags::device_key(&format!("{stem}.mp3"));
            lib.tracks()?
                .into_iter()
                .find(|t| tags::device_key(&t.filename) == want)
                .map(|t| t.handle)
                .with_context(|| format!("no track matching '{target}'"))?
        };

        let info = lib.session.object_info(handle)?;
        println!("handle     {}", library::hex(handle));
        println!("format     {:#06x} ({})", info.format, format_name::of(info.format));
        println!("size       {} bytes", info.size);
        println!("parent     {:#010x}", info.parent);
        println!("filename   {}", info.filename);
        for (label, prop) in [
            ("Name", prop::NAME),
            ("Artist", prop::ARTIST),
            ("Album", prop::ALBUM),
            ("AlbumArtist", prop::ALBUM_ARTIST),
        ] {
            match lib.session.get_prop_str(handle, prop) {
                Ok(v) => println!("{label:<10} {prop:#06x}  {v:?}"),
                Err(e) => println!("{label:<10} {prop:#06x}  <error: {e:#}>"),
            }
        }
        match lib.session.get_prop_u16(handle, prop::TRACK) {
            Ok(v) => println!("Track      {:#06x}  {v}", prop::TRACK),
            Err(e) => println!("Track      {:#06x}  <error: {e:#}>", prop::TRACK),
        }
        match lib.session.object_refs(handle) {
            Ok(r) => println!(
                "refs       {} [{}]",
                r.len(),
                r.iter().map(|h| library::hex(*h)).collect::<Vec<_>>().join(", ")
            ),
            Err(e) => println!("refs       <error: {e:#}>"),
        }
        Ok(())
    })
}

fn cmd_albums(s: &mut Session, key_path: Option<&Path>) -> Result<()> {
    with_library(s, key_path, |lib| {
        let groups = lib.groups()?;
        if groups.is_empty() {
            println!("(no playlists or albums)");
            return Ok(());
        }
        for g in &groups {
            println!(
                "{:<10} {:<12} {:>3} tracks  {:<40} {}",
                g.kind,
                library::hex(g.handle),
                g.tracks,
                truncate(g.name.as_deref().unwrap_or(&g.filename), 40),
                g.filename
            );
        }
        Ok(())
    })
}

fn cmd_list(s: &mut Session, key_path: Option<&Path>, kind: &str) -> Result<()> {
    auth(s, key_path)?;
    let storage = *s.storage_ids()?.first().context("no storage")?;
    s.session_storage = storage;
    let formats: Vec<u16> = match kind {
        "audio" => vec![fmt::MP3, fmt::WMA, fmt::AAC],
        "video" => vec![fmt::WMV, fmt::MP4],
        "photo" => vec![fmt::JPEG],
        _ => vec![0],
    };
    for f in formats {
        for h in s.children(f, ROOT)? {
            let info = s.object_info(h)?;
            println!(
                "{:<12} {:<10} {:>9} bytes  {}",
                library::hex(h),
                format_name::of(info.format),
                info.size,
                info.filename
            );
        }
    }
    Ok(())
}

// ── pushing ──────────────────────────────────────────────────────────────────

fn cmd_get_object(
    s: &mut Session,
    key_path: Option<&Path>,
    handle: &str,
    out: &Path,
    bytes: Option<u32>,
) -> Result<()> {
    auth(s, key_path)?;
    let storage = *s.storage_ids()?.first().context("no storage")?;
    s.session_storage = storage;
    let handle = handle
        .trim()
        .trim_start_matches("0x")
        .trim_start_matches("0X");
    let handle = u32::from_str_radix(handle, 16).context("handle must be hex")?;
    let data = match bytes {
        Some(count) => s.get_partial_object(handle, 0, count)?,
        None => s.get_object_bytes(handle)?,
    };
    std::fs::write(out, &data)
        .with_context(|| format!("writing {}", out.display()))?;
    println!(
        "{} -> {} ({} bytes)",
        library::hex(handle),
        out.display(),
        data.len()
    );
    Ok(())
}

fn cmd_push(s: &mut Session, key_path: Option<&Path>, paths: &[PathBuf], raw: bool) -> Result<()> {
    let files = expand(paths)?;
    if files.is_empty() {
        anyhow::bail!("nothing to push");
    }
    with_library(s, key_path, |lib| {
        let (mut ok, mut skipped) = (0usize, 0usize);
        for path in &files {
            if raw {
                let data = std::fs::read(path)?;
                let name = path.file_name().unwrap_or_default().to_string_lossy().to_string();
                let format = format_for(path);
                let handle = lib.session.create_object(ROOT, format, &name, data.len() as u32, 0, 0, 0)?;
                lib.session.push_object_data(&data)?;
                println!("  ✓ {} -> {}", name, library::hex(handle));
                ok += 1;
                continue;
            }
            if !is_audio(path) {
                println!("  · skip {} (not audio; use --raw, photo or video)", path.display());
                skipped += 1;
                continue;
            }
            let t = tags::read(path);
            let converted = media::to_mp3(path)
                .with_context(|| format!("converting {}", path.display()))?;
            let handle = lib.push_track(path, &converted.data, &t)?;
            println!(
                "  ✓ {:<44} {}{}  {}  {:.1} MB",
                converted.name,
                t.title,
                t.artist.as_deref().map(|a| format!(" · {a}")).unwrap_or_default(),
                library::hex(handle),
                converted.data.len() as f64 / 1_048_576.0
            );
            ok += 1;
        }
        println!("\n{ok} pushed, {skipped} skipped");
        Ok(())
    })
}

fn cmd_photo(s: &mut Session, key_path: Option<&Path>, paths: &[PathBuf], dry_run: bool) -> Result<()> {
    let files = collect(paths, media::is_image)?;
    with_library(s, key_path, |lib| {
        let mut ok = 0usize;
        for path in &files {
            let photo = media::to_zune_photo(path)
                .with_context(|| format!("preparing {}", path.display()))?;
            if dry_run {
                println!(
                    "  · would push {:<40} {}x{}  {:.0} KB",
                    photo.name,
                    photo.width,
                    photo.height,
                    photo.data.len() as f64 / 1024.0
                );
                continue;
            }
            let handle = lib.push_photo(&photo)?;
            println!(
                "  ✓ {:<40} {}x{}  {:>7.0} KB  {}",
                photo.name,
                photo.width,
                photo.height,
                photo.data.len() as f64 / 1024.0,
                library::hex(handle)
            );
            ok += 1;
        }
        if dry_run {
            println!("\n{} photos, dry run — nothing written", files.len());
        } else {
            println!("\n{ok}/{} photos pushed to Pictures", files.len());
        }
        Ok(())
    })
}

fn cmd_video(
    s: &mut Session,
    key_path: Option<&Path>,
    paths: &[PathBuf],
    subs: Option<&Path>,
    dry_run: bool,
) -> Result<()> {
    let files = collect(paths, media::is_video)?;
    with_library(s, key_path, |lib| {
        let mut ok = 0usize;
        for path in &files {
            if dry_run {
                println!("  · would convert+push {}", path.display());
                continue;
            }
            println!("  » converting {} …", path.file_name().unwrap_or_default().to_string_lossy());
            let converted = media::to_wmv(path, subs)
                .with_context(|| format!("converting {}", path.display()))?;
            let stem = path.file_stem().unwrap_or_default().to_string_lossy().to_string();
            let handle = lib.push_video(&converted.name, &converted.data, &stem)?;
            println!(
                "  ✓ {:<40} {:>7.1} MB  {}",
                converted.name,
                converted.data.len() as f64 / 1_048_576.0,
                library::hex(handle)
            );
            ok += 1;
        }
        if dry_run {
            println!("\n{} videos, dry run — nothing written", files.len());
        } else {
            println!("\n{ok}/{} videos pushed to Video", files.len());
        }
        Ok(())
    })
}

fn cmd_convert(paths: &[PathBuf], out: &Path) -> Result<()> {
    std::fs::create_dir_all(out).with_context(|| format!("creating {}", out.display()))?;
    let files = collect_any(paths)?;
    let (mut audio, mut photos, mut videos, mut other) = (0, 0, 0, 0);
    for path in &files {
        let name = path.file_name().unwrap_or_default().to_string_lossy().to_string();
        if is_audio(path) {
            let c = media::to_mp3(path)?;
            let dst = out.join(&c.name);
            std::fs::write(&dst, &c.data)?;
            println!("  mp3    {:<36} -> {:<36} {:>7.1} MB", name, c.name, c.data.len() as f64 / 1_048_576.0);
            audio += 1;
        } else if media::is_image(path) {
            let p = media::to_zune_photo(path)?;
            std::fs::write(out.join(&p.name), &p.data)?;
            println!("  jpeg   {:<36} -> {:<36} {}x{} {:>6.0} KB", name, p.name, p.width, p.height, p.data.len() as f64 / 1024.0);
            photos += 1;
        } else if media::is_video(path) {
            let c = media::to_wmv(path, None)?;
            std::fs::write(out.join(&c.name), &c.data)?;
            println!("  wmv    {:<36} -> {:<36} {:>7.1} MB", name, c.name, c.data.len() as f64 / 1_048_576.0);
            videos += 1;
        } else {
            println!("  · skip {name}");
            other += 1;
        }
    }
    println!("\n{audio} audio · {photos} photos · {videos} videos · {other} skipped -> {}", out.display());
    Ok(())
}

fn cmd_album(
    s: &mut Session,
    key_path: Option<&Path>,
    dir: &Path,
    name: Option<&str>,
    cover: Option<&Path>,
    make_playlist: bool,
    dry_run: bool,
) -> Result<()> {
    with_library(s, key_path, |lib| {
        let report = lib.sync_album(dir, name, cover, make_playlist, dry_run, |line| println!("{line}"))?;
        if dry_run {
            println!("\n(dry run — nothing written)");
        } else {
            println!(
                "\n✓ {} — {} tracks, playlist {}, cover {}, album {}",
                report.album,
                report.pushed,
                opt_hex(report.playlist),
                opt_hex(report.cover),
                opt_hex(report.album_handle)
            );
            println!("  Restart the Zune for the metadata to appear under Music → Albums.");
        }
        Ok(())
    })
}

fn cmd_playlist(
    s: &mut Session,
    key_path: Option<&Path>,
    name: &str,
    tracks: &[String],
    device_only: bool,
) -> Result<()> {
    with_library(s, key_path, |lib| {
        let existing = lib.tracks()?;
        let mut refs: Vec<u32> = Vec::new();
        for entry in tracks {
            let path = PathBuf::from(entry);
            let local = path.is_file();
            let stem = if local {
                path.file_stem().unwrap_or_default().to_string_lossy().to_string()
            } else {
                Path::new(entry).file_stem().unwrap_or_default().to_string_lossy().to_string()
            };
            let key = tags::device_key(&format!("{stem}.mp3"));
            let found = existing
                .iter()
                .find(|t| tags::device_key(&t.filename) == key)
                .map(|t| t.handle);

            match (found, device_only, local) {
                (Some(h), _, _) => {
                    refs.push(h);
                    println!("  · {} -> {}", stem, library::hex(h));
                }
                (None, true, _) => println!("  · skip {stem} (not on device)"),
                (None, false, true) => {
                    let data = std::fs::read(&path)?;
                    let t = tags::read(&path);
                    let h = lib.push_track(&path, &data, &t)?;
                    println!("  ✓ pushed {stem} -> {}", library::hex(h));
                    refs.push(h);
                }
                (None, false, false) => println!("  · skip {stem} (no such file, not on device)"),
            }
        }
        let h = lib.create_playlist(name, &refs)?;
        println!("\n✓ playlist '{name}' with {} tracks ({})", refs.len(), library::hex(h));
        Ok(())
    })
}

fn cmd_rm(s: &mut Session, key_path: Option<&Path>, handles: &[String], name: Option<&str>) -> Result<()> {
    with_library(s, key_path, |lib| {
        if let Some(n) = name {
            let stem = Path::new(n).file_stem().unwrap_or_default().to_string_lossy().to_string();
            let removed = lib.remove_track(&stem)?;
            println!("{} {stem}", if removed { "deleted" } else { "not found:" });
        }
        for h in handles {
            let handle = parse_handle(h)?;
            lib.remove(handle)?;
            println!("deleted {}", library::hex(handle));
        }
        Ok(())
    })
}

fn cmd_wifi(s: &mut Session, key_path: Option<&Path>, w: WifiCommand) -> Result<()> {
    match w {
        WifiCommand::Profiles => {
            auth(s, key_path)?;
            let data = s.wifi_get_profiles()?;
            if data.len() < 4 {
                println!("the device answered {} bytes — no profile list", data.len());
                return Ok(());
            }
            let count = u32::from_le_bytes([data[0], data[1], data[2], data[3]]) as usize;
            println!("{count} profile(s), {} bytes total", data.len());
            for index in 0..count {
                let start = 4 + index * 320;
                let Some(record) = data.get(start..start + 320) else {
                    println!("  [{}] record runs past the end of the answer", index + 1);
                    break;
                };
                // The record layout is the one the writer uses: NDIS SSID, then the four
                // 32-bit fields, then the key block.
                let ssid_len = u32::from_le_bytes([record[0], record[1], record[2], record[3]])
                    .min(32) as usize;
                let ssid = String::from_utf8_lossy(&record[4..4 + ssid_len]).to_string();
                let field = |at: usize| {
                    u32::from_le_bytes([
                        record[at],
                        record[at + 1],
                        record[at + 2],
                        record[at + 3],
                    ])
                };
                let (ctl, auth, cipher, konst) = (field(0x24), field(0x28), field(0x2c), field(0x30));
                let key_len = field(0x34 + 8) as usize;
                println!(
                    "  [{}] ssid={:?} auth={auth} cipher={cipher} ctl={ctl} const={konst} keyLen={key_len}",
                    index + 1,
                    ssid
                );
                println!(
                    "      raw: {}",
                    record[..48]
                        .iter()
                        .map(|b| format!("{b:02x}"))
                        .collect::<Vec<_>>()
                        .join(" ")
                );
            }
        }
        WifiCommand::Scan => {
            auth(s, key_path)?;
            let data = s.wifi_scan()?;
            println!("{} bytes of scan result", data.len());
            println!("{}", hexdump(&data));
        }
        WifiCommand::SetProfile { hex, file } => {
            auth(s, key_path)?;
            let blob = match (hex, file) {
                (Some(h), _) => hex::decode(h.trim()).context("--hex is not valid hex")?,
                (None, Some(p)) => std::fs::read(p).context("reading profile blob")?,
                (None, None) => anyhow::bail!("pass --hex or --file"),
            };
            s.wifi_set_profile(&blob)?;
            println!("profile committed ({} bytes)", blob.len());
        }
        WifiCommand::Test => {
            auth(s, key_path)?;
            s.wifi_test_connectivity()?;
            println!("connectivity test triggered");
        }
    }
    Ok(())
}

// ── local tags ───────────────────────────────────────────────────────────────

fn cmd_tags(paths: &[PathBuf]) -> Result<()> {
    if paths.is_empty() {
        anyhow::bail!("pass files or directories");
    }
    for path in expand(paths)? {
        let t = tags::read(&path);
        println!(
            "{:<44} {:<30} {:<22} {:<24} {:>3}",
            path.file_name().unwrap_or_default().to_string_lossy(),
            truncate(&t.title, 30),
            truncate(t.artist.as_deref().unwrap_or("-"), 22),
            truncate(t.album.as_deref().unwrap_or("-"), 24),
            t.track.map(|n| n.to_string()).unwrap_or_else(|| "-".into())
        );
    }
    Ok(())
}

// ── helpers ──────────────────────────────────────────────────────────────────

/// Expand paths into every file, whatever the kind.
fn collect_any(paths: &[PathBuf]) -> Result<Vec<PathBuf>> {
    collect(paths, |_| true)
}

fn is_audio(p: &Path) -> bool {
    p.extension()
        .map(|e| library::AUDIO_EXTS.contains(&e.to_string_lossy().to_lowercase().as_str()))
        .unwrap_or(false)
}

/// Expand paths into media files of one kind (directories are searched one level).
fn collect(paths: &[PathBuf], keep: impl Fn(&Path) -> bool + Copy) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    for p in paths {
        if p.is_dir() {
            out.extend(media::files_in(p, keep)?);
        } else if p.is_file() {
            if keep(p) {
                out.push(p.clone());
            } else {
                eprintln!("  · skipping unsupported file {}", p.display());
            }
        } else {
            anyhow::bail!("{} does not exist", p.display());
        }
    }
    out.sort();
    out.dedup();
    if out.is_empty() {
        anyhow::bail!("no matching files");
    }
    Ok(out)
}

fn format_for(p: &Path) -> u16 {
    match p.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default().as_str() {
        "wma" => fmt::WMA,
        "aac" | "m4a" => fmt::AAC,
        "wmv" => fmt::WMV,
        "mp4" | "m4v" => fmt::MP4,
        "jpg" | "jpeg" => fmt::JPEG,
        _ => fmt::MP3,
    }
}

/// Expand directories into the files they contain (one level of recursion for
/// album folders, which are usually flat).
fn expand(paths: &[PathBuf]) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    for p in paths {
        if p.is_dir() {
            out.extend(library::audio_files(p)?);
        } else if p.is_file() {
            out.push(p.clone());
        } else {
            anyhow::bail!("{} does not exist", p.display());
        }
    }
    out.sort();
    out.dedup();
    if out.is_empty() {
        anyhow::bail!("no audio files found");
    }
    Ok(out)
}

fn parse_handle(s: &str) -> Result<u32> {
    let t = s.trim();
    if let Some(h) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        u32::from_str_radix(h, 16).with_context(|| format!("bad handle {s}"))
    } else {
        t.parse::<u32>().with_context(|| format!("bad handle {s}"))
    }
}

fn opt_hex(v: Option<u32>) -> String {
    v.map(library::hex).unwrap_or_else(|| "-".into())
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        let cut: String = s.chars().take(n.saturating_sub(1)).collect();
        format!("{cut}…")
    }
}

