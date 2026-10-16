//! The background device thread.
//!
//! Everything that touches USB lives here: `nusb` is a blocking API, so the UI
//! thread must never call it. The thread owns the `Session`, executes one
//! [`Command`] at a time, and pushes [`Event`]s back over an mpsc channel,
//! calling `ctx.request_repaint()` so the UI redraws only when there is news.

use std::cell::Cell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};

use crate::cache;
use crate::coverfetch;
use crate::locallib;
use crate::core::{
    id3,
    self,
    device::{Session, ROOT},
    library::{self, Library},
    mtp::{self, fmt},
    tags, usb,
    usb::DeviceInfo,
};
use crate::model::*;

/// How long the worker waits for a command before doing idle work.
const IDLE_TICK: Duration = Duration::from_millis(400);
/// How often we ask the OS whether a Zune has appeared.
const DEVICE_SEARCH_INTERVAL: Duration = Duration::from_secs(3);
/// How often a live session is poked to prove the device is still there.
const HEALTH_INTERVAL: Duration = Duration::from_secs(4);
/// Progress events are rate-limited to this interval.
const PROGRESS_INTERVAL: Duration = Duration::from_millis(120);
/// Cover art larger than this is downscaled before it becomes a texture.
const COVER_MAX: u32 = 512;

// ── public handle ────────────────────────────────────────────────────────────

pub struct Worker {
    tx: Sender<Command>,
    rx: Receiver<Event>,
    cancel: Arc<AtomicBool>,
    /// Aborts the *library read* early, so a diagnostic can run without waiting the
    /// two or three minutes a 2425-track device needs.
    interrupt: Arc<AtomicBool>,
    join: Option<JoinHandle<()>>,
}

/// Set by `ZUNE_NO_COVERS=1`: skip cover work entirely.
///
/// A dev affordance. The fallback ladder is synchronous inside the worker — one album
/// can hold it for seconds between the device read and the online throttle — so a test
/// of any other command would otherwise queue behind every cover-less album in the
/// library. Nothing about the covers themselves changes; they are simply not asked for.
fn covers_disabled() -> bool {
    static ONCE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ONCE.get_or_init(|| std::env::var_os("ZUNE_NO_COVERS").is_some())
}

/// One prefetch per run. Both the cached-library path and the device-refresh path load an
/// album list, and without this the whole library would be walked twice.
static PREFETCH_STARTED: AtomicBool = AtomicBool::new(false);

impl Worker {
    /// Start the device thread. `ctx` is used to request repaints on news.
    pub fn spawn(ctx: egui::Context) -> Self {
        let (cmd_tx, cmd_rx) = channel();
        let (ev_tx, ev_rx) = channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let interrupt = Arc::new(AtomicBool::new(false));
        let thread_cancel = Arc::clone(&cancel);
        let thread_interrupt = Arc::clone(&interrupt);
        let join = thread::Builder::new()
            .name("zune-device".to_owned())
            .spawn(move || run(cmd_rx, Emitter::new(ev_tx, ctx), thread_cancel, thread_interrupt))
            .expect("spawning the device thread");
        Worker { tx: cmd_tx, rx: ev_rx, cancel, interrupt, join: Some(join) }
    }

    pub fn send(&self, cmd: Command) {
        if self.tx.send(cmd).is_err() {
            log::warn!("the device thread is gone; command dropped");
        }
    }

    pub fn try_recv(&self) -> Option<Event> {
        self.rx.try_recv().ok()
    }

    pub fn request_cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    pub fn clear_cancel(&self) {
        self.cancel.store(false, Ordering::Relaxed);
    }

    /// Ask the worker to stop reading the library early. Called just before queueing a
    /// diagnostic, so "Diagnose covers" answers in seconds instead of after the full
    /// track scan.
    pub fn interrupt_library_read(&self) {
        self.interrupt.store(true, Ordering::Relaxed);
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        let _ = self.tx.send(Command::Shutdown);
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

// ── event emitter ────────────────────────────────────────────────────────────

/// Sends events and wakes the UI. Lives entirely on the device thread, so the
/// throttle timestamp can be a plain `Cell`.
struct Emitter {
    tx: Sender<Event>,
    ctx: egui::Context,
    last_progress: Cell<Instant>,
}

impl Emitter {
    fn new(tx: Sender<Event>, ctx: egui::Context) -> Self {
        Self { tx, ctx, last_progress: Cell::new(Instant::now() - PROGRESS_INTERVAL) }
    }

    /// A second handle for another thread: same channel, same context, its own progress
    /// clock. The cover prefetch runs on a thread of its own and needs to report back.
    fn fork(&self) -> Self {
        Self::new(self.tx.clone(), self.ctx.clone())
    }

    fn send(&self, event: Event) {
        if self.tx.send(event).is_err() {
            // The UI is gone; the worker will notice on its next command.
            return;
        }
        self.ctx.request_repaint();
    }

    fn log(&self, line: LogLine) {
        log::debug!("{}", line.text);
        self.send(Event::Log(line));
    }

    fn link(&self, link: Link) {
        self.send(Event::Link(link));
    }

    fn busy(&self, busy: bool) {
        self.send(Event::Busy(busy));
    }

    fn task_started(&self, task: impl Into<String>, items: usize) {
        self.send(Event::TaskStarted { task: task.into(), items });
    }

    fn task_finished(&self, ok: bool, summary: impl Into<String>) {
        self.send(Event::TaskFinished { ok, summary: summary.into() });
    }

    /// Always delivered (task/step boundaries).
    fn progress(&self, p: Progress) {
        self.last_progress.set(Instant::now());
        self.send(Event::Progress(Box::new(p)));
    }

    /// Rate-limited (byte-level upload callbacks).
    fn progress_throttled(&self, p: Progress) {
        if self.last_progress.get().elapsed() >= PROGRESS_INTERVAL {
            self.progress(p);
        }
    }
}

// ── thread body ──────────────────────────────────────────────────────────────

fn run(rx: Receiver<Command>, ev: Emitter, cancel: Arc<AtomicBool>, interrupt: Arc<AtomicBool>) {
    let mut backend = Backend::default();
    let demo = std::env::var_os("ZUNE_DEMO").is_some();
    if demo {
        ev.log(LogLine::warn(
            "ZUNE_DEMO=1 — showing a synthetic library; no USB device is contacted",
        ));
        demo_library(&ev);
        ev.link(Link::Connected);
    } else {
        // Serve the last library read from disk before USB is touched at all. This is
        // what makes a start instant: no device round trips until Refresh, and the
        // library is still there when the Zune is not plugged in.
        backend.serve_newest_cache(&ev);
        ev.link(Link::Disconnected);
    }

    let mut last_search = Instant::now() - DEVICE_SEARCH_INTERVAL;
    let mut last_health = Instant::now();

    loop {
        let now = Instant::now();
        if !demo
            && backend.session.is_none()
            && !backend.ejected
            && now.duration_since(last_search) >= DEVICE_SEARCH_INTERVAL
        {
            if backend.await_replug && usb::find_device().is_ok() {
                // Still plugged in: do not touch it. A wedged Zune is only recovered by
                // removing the cable, and every attempt risks more stale state.
                last_search = Instant::now();
                continue;
            }
            backend.await_replug = false;
            last_search = now;
            backend.try_connect(&ev);
        }

        match rx.recv_timeout(IDLE_TICK) {
            Ok(Command::Shutdown) => break,
            Ok(Command::Connect) if demo => {
                ev.log(LogLine::info("ZUNE_DEMO=1 — no device is contacted"));
            }
            Ok(cmd) => {
                let force_connect = matches!(cmd, Command::Connect);
                // A diagnostic never waits behind a long library read: the interrupt is
                // raised by the UI before the command lands, and the read loop honours
                // it within a track or two.
                if matches!(
                    cmd,
                    Command::DiagnoseCovers { .. } | Command::DiagnoseAlbums(_)
                ) {
                    interrupt.store(false, Ordering::Relaxed);
                }
                backend.handle(cmd, &ev, &cancel, &interrupt);
                if force_connect {
                    last_search = Instant::now();
                }
                last_health = Instant::now();
            }
            Err(RecvTimeoutError::Timeout) => {
                if backend.session.is_some() && last_health.elapsed() >= HEALTH_INTERVAL {
                    last_health = Instant::now();
                    backend.health_check(&ev);
                }
            }
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }

    backend.shutdown(&ev);
}

// ── device backend ───────────────────────────────────────────────────────────

#[derive(Default)]
struct Backend {
    info: Option<DeviceInfo>,
    session: Option<Session>,
    storage: u32,
    snapshot: DeviceSnapshot,
    /// Last connect error, so a 3-second retry loop does not flood the log.
    last_error: Option<String>,
    /// Set by `eject`: wait for the device to disappear before reconnecting.
    ejected: bool,
    /// Set when the link is wedged in a way a USB reset does not fix. The app then stops
    /// reconnecting on a timer — each attempt on a dead device is pointless traffic —
    /// until the device actually disappears (the user unplugged it) and comes back.
    await_replug: bool,
    /// A stuck pipe means the next connect must reset the device first.
    reset_before_connect: bool,
    /// Which object property carries an album's date on this firmware (probed once).
    album_date_code: Option<u16>,
    /// Lazily built index of the device's `CoverArt` folder: (handle, name, format).
    cover_index: Option<Vec<(u32, String, u16)>>,
    /// Set once the device has refused the bulk property list, so later refreshes skip
    /// straight to per-track reads instead of waiting out the 30-second timeout again.
    bulk_props_unavailable: bool,
    /// Set when the session is being used by a task.
    busy: bool,
    /// The library the device last reported, kept so the embedded-art sweep can run as
    /// soon as a session exists. The cached start loads a library with no session — the
    /// device connects afterwards — so the sweep cannot simply run where the library
    /// arrives.
    albums: Vec<Album>,
    /// The device's tracks as last read (or served from the cache), kept for the sync
    /// planner: it matches on file names and tags, not handles.
    tracks: Vec<Track>,
    /// Set once the sweep has run for the current library.
    harvested: bool,
    /// The track handles of the library currently on screen. One `GetObjectHandles`
    /// round trip against this set answers "did anything change" without paying for the
    /// full sweep, which the device makes unavoidable otherwise: it refuses the bulk
    /// property list (0x9805), so a full read is one property read per track.
    cached_track_handles: std::collections::HashSet<u32>,
}

impl Backend {
    // ── connection ───────────────────────────────────────────────────────────

    fn try_connect(&mut self, ev: &Emitter) {
        let info = match usb::find_device() {
            Ok(info) => info,
            Err(_) => {
                // No device at all: stay quiet, the UI already says "no device".
                self.last_error = None;
                return;
            }
        };
        if let Err(e) = self.connect(info, ev) {
            let msg = format!("{e:#}");
            if self.last_error.as_deref() != Some(msg.as_str()) {
                ev.log(LogLine::error(format!("Could not connect: {msg}")));
                self.last_error = Some(msg);
            }
            self.drop_session();
            ev.link(Link::Disconnected);
        }
    }

    fn connect(&mut self, mut info: DeviceInfo, ev: &Emitter) -> Result<()> {
        ev.link(Link::Connecting);
        ev.log(LogLine::info(format!(
            "{} on USB (serial {} · {})",
            usb::device_name(&info),
            info.serial_number().unwrap_or("-"),
            usb::topology(&info)
        )));

        // A pipe left stalled by an interrupted session cannot be cleared in place
        // on macOS; a reset is the documented way back.
        if self.reset_before_connect {
            ev.log(LogLine::warn("previous session went dead — resetting the device first"));
            usb::reset(&info).context("resetting the device")?;
            info = usb::find_device().context("re-finding the device after the reset")?;
            self.reset_before_connect = false;
        }

        let mut session = Session::new(open_and_clear(&info, ev)?);

        match session.open_session() {
            Ok(()) => {}
            Err(e) if usb::is_stuck(&e) => {
                ev.log(LogLine::warn(format!(
                    "the MTP session did not start ({e:#}) — resetting the Zune and retrying once"
                )));
                drop(session);
                usb::reset(&info).context("resetting after a stuck session")?;
                let fresh = usb::find_device().context("re-finding the device after the reset")?;
                let mut retry = Session::new(open_and_clear(&fresh, ev)?);
                retry
                    .open_session()
                    .context("opening the MTP session after a device reset")?;
                ev.log(LogLine::ok("recovered after a device reset"));
                info = fresh;
                session = retry;
            }
            Err(e) => return Err(e).context("opening the MTP session"),
        }

        let mut snapshot = DeviceSnapshot {
            name: usb::device_name(&info),
            serial: info.serial_number().unwrap_or("-").to_owned(),
            topology: usb::topology(&info),
            ..Default::default()
        };

        match session.device_info() {
            Ok(di) => {
                snapshot.manufacturer = di.manufacturer;
                snapshot.firmware = di.device_version;
                snapshot.mtp_version =
                    format!("{}.{}", di.standard_version >> 8, di.standard_version & 0xff);
            }
            Err(e) => ev.log(LogLine::warn(format!("could not read the device info: {e:#}"))),
        }

        // MTPZ: without the handshake the Zune serves no storage at all. A missing
        // key file is a normal, reportable state — not a crash.
        match core::MtpzKeys::load(None) {
            Ok(keys) => {
                if let Err(e) = session.authenticate(&keys) {
                    let msg = format!("{e:#}");
                    snapshot.key_error = Some(msg.clone());
                    ev.log(LogLine::error(format!("MTPZ handshake failed: {msg}")));
                } else {
                    snapshot.key_path = key_path().map(|p| p.display().to_string());
                    ev.log(LogLine::ok("MTPZ handshake ok — the device is trusted"));
                    match session.storage_ids() {
                        Ok(ids) if !ids.is_empty() => {
                            self.storage = ids[0];
                            session.session_storage = ids[0];
                        }
                        Ok(_) => ev.log(LogLine::warn("the device reported no storage")),
                        Err(e) => ev.log(LogLine::warn(format!("could not list the storage: {e:#}"))),
                    }
                }
            }
            Err(e) => {
                snapshot.key_error = Some(format!("{e:#}"));
                ev.log(LogLine::error(format!(
                    "MTPZ key file not found — the library stays unavailable until it exists.\n{}",
                    key_candidates()
                        .iter()
                        .map(|p| format!("    looked in {}\n", p.display()))
                        .collect::<String>()
                )));
            }
        }

        snapshot.needs_replug = false;
        self.snapshot = snapshot;
        self.session = Some(session);
        self.last_error = None;
        self.await_replug = false;
        // A new session may be a different device (or the same one with a new
        // library): drop everything cached across the old one.
        self.cover_index = None;
        self.album_date_code = None;
        self.bulk_props_unavailable = false;
        // A session may have appeared long after the library did (the cached start loads
        // the library with no device attached). This is the moment the embedded-art sweep
        // becomes possible, so it runs here rather than from `handle`: an idle UI sends no
        // commands, and `busy` is still set during a connect.
        self.harvested = true;
        ev.link(Link::Connected);
        ev.send(Event::Snapshot(Box::new(self.snapshot.clone())));

        if self.snapshot.key_error.is_none() {
            // A cache for *this* serial means the library can be shown at once; the
            // device is only re-read on an explicit Refresh. A cache for a different
            // device must never be shown as this one's, hence the serial match inside.
            if !self.serve_cache_for_serial(ev) {
                // Interruptible: a diagnostic must never wait behind this read.
                let interrupt = Arc::new(AtomicBool::new(false));
                // The automatic pass after a connect takes the cheap route: if the device
                // matches what is already on screen there is nothing to do.
                if let Err(e) = self.refresh(ev, &interrupt, false) {
                    ev.log(LogLine::error(format!("could not read the library: {e:#}")));
                }
            }
        }
        Ok(())
    }

    /// Bring the link back after a stuck failure: drain, then a device reset, and if the
    /// device still will not answer, stop trying and ask for a physical replug.
    fn recover_link(&mut self, ev: &Emitter) {
        // Drain first: a stale response is the one failure a drain *can* fix.
        if let Some(session) = self.session.as_mut() {
            match session.drain_pending_input(std::time::Duration::from_millis(350)) {
                Ok(0) => log::debug!("nothing to drain after the stuck failure"),
                Ok(bytes) => ev.log(LogLine::info(format!(
                    "cleared {bytes} bytes the failed read left buffered"
                ))),
                Err(e) => log::debug!("drain after the stuck failure: {e:#}"),
            }
        }
        self.drop_session();
        ev.link(Link::Disconnected);
        let Some(info) = usb::find_device().ok() else {
            self.await_replug = true;
            return;
        };
        match usb::reset(&info) {
            Ok(()) => {
                ev.log(LogLine::info("USB link reset — reconnecting"));
                self.reset_before_connect = false;
                self.last_error = None;
                self.try_connect(ev);
            }
            Err(e) => {
                self.await_replug = true;
                self.snapshot.needs_replug = true;
                ev.send(Event::Snapshot(Box::new(self.snapshot.clone())));
                ev.log(LogLine::error(format!(
                    "The USB reset did not help ({e:#}). {}",
                    usb::REPLUG_HINT
                )));
            }
        }
    }

    // ── library cache ────────────────────────────────────────────────────────

    /// On start-up, before any device is found: show the most recently read library
    /// from disk so the window is usable (and says it is cached).
    fn serve_newest_cache(&mut self, ev: &Emitter) {
        let Some(cache) = cache::LibraryCache::load_newest() else {
            ev.log(LogLine::info("Looking for a Zune on USB…"));
            return;
        };
        let serial = cache.serial.clone();
        let age = cache::relative_age(cache.read_at, cache::now_unix());
        let tracks = cache.tracks.len();
        let note = Some("device not connected — showing the last library read".to_owned());
        self.apply_cache(cache, ev, false, note);
        ev.log(LogLine::info(format!(
            "cached library for {serial}: {tracks} tracks, read {age} — press Refresh to re-read the device"
        )));
    }

    /// Show the cached library for the device that just connected, if there is one.
    /// Returns `false` when this serial has no cache (the caller then reads the device).
    fn serve_cache_for_serial(&mut self, ev: &Emitter) -> bool {
        let serial = self.snapshot.serial.clone();
        let Some(cache) = cache::LibraryCache::load(&serial) else {
            return false;
        };
        let age = cache::relative_age(cache.read_at, cache::now_unix());
        let stale = cache::is_stale(cache.read_at);
        let tracks = cache.tracks.len();
        let note = if stale {
            format!("cached data is {age} — press Refresh to re-read the device")
        } else {
            "showing cached data — press Refresh to re-read the device".to_owned()
        };
        self.apply_cache(cache, ev, true, Some(note));
        ev.log(LogLine::info(format!(
            "using the cached library for {serial} ({tracks} tracks, read {age}{}) — Refresh re-reads the device",
            if stale { ", possibly stale" } else { "" }
        )));
        true
    }

    /// Hand a cache to the UI as the current library.
    ///
    /// Identity is only restored when the device is *not* connected: a live serial
    /// and firmware are fresher than the file, so they must win.
    fn apply_cache(
        &mut self,
        cache: cache::LibraryCache,
        ev: &Emitter,
        device_connected: bool,
        note: Option<String>,
    ) {
        let read_at = cache.read_at;
        let serial = cache.serial.clone();
        cache.apply_library(&mut self.snapshot);
        cache.apply_storage(&mut self.snapshot);
        if !device_connected {
            cache.apply_identity(&mut self.snapshot);
        }
        // Cover lookups are keyed by serial, so the one the UI will ask about must be
        // the one this cache belongs to, even if the device is new to us.
        if !cache::is_usable_serial(&self.snapshot.serial) {
            self.snapshot.serial = serial;
        }
        let tracks = cache.to_tracks();
        let albums = cache.to_albums();
        self.cached_track_handles = tracks.iter().map(|t| t.handle).collect();
        self.tracks = tracks.clone();
        log::debug!("served {} cached tracks", tracks.len());
        self.busy = false;
        ev.busy(false);
        ev.send(Event::Snapshot(Box::new(self.snapshot.clone())));
        // Device first: most of this library carries its art in the audio files, and that
        // is a few seconds of partial reads rather than minutes of network throttling.
        // With no session yet (the usual cached start), `handle` runs the sweep as soon
        // as the device connects.
        // No bulk sweep from a cached library: its handles may predate a change on the
        // device, and a stale handle costs a full 30-second USB timeout. The per-album
        // fallback still tries the audio file first, with handles taken from the live
        // library the UI was given.
        self.albums = albums.clone();
        self.harvested = true;
        // Whatever is left is not part of the cache: even a fast start has to fetch it, and
        // the Albums page opens straight from this path. Start the background pass here.
        Self::spawn_cover_prefetch(&self.snapshot.serial, &albums, ev);
        ev.send(Event::Library { tracks, albums });
        ev.send(Event::Origin(Box::new(DataOrigin {
            source: Source::Cached,
            read_at: Some(read_at),
            device_connected,
            serial: cache.serial.clone(),
            note,
        })));
    }

    fn drop_session(&mut self) {
        if let Some(mut session) = self.session.take() {
            let _ = session.close_session();
        }
        self.info = None;
    }

    fn shutdown(&mut self, ev: &Emitter) {
        if self.session.is_some() {
            ev.log(LogLine::info("Closing the MTP session"));
            self.drop_session();
        }
    }

    /// Cheap liveness probe while idle: one GetDeviceInfo round trip.
    fn health_check(&mut self, ev: &Emitter) {
        let Some(session) = self.session.as_mut() else { return };
        match session.device_info() {
            Ok(_) => {}
            Err(e) => {
                let stuck = usb::is_stuck(&e);
                ev.log(LogLine::warn(format!("The device stopped answering: {e:#}")));
                self.drop_session();
                ev.link(Link::Disconnected);
                if stuck {
                    // A reset re-enumerates the port but leaves a wedged firmware just as
                    // dead (measured), so do not pretend: ask for the cable instead.
                    self.reset_before_connect = true;
                    self.await_replug = true;
                    self.snapshot.needs_replug = true;
                    ev.send(Event::Snapshot(Box::new(self.snapshot.clone())));
                    ev.log(LogLine::error(format!(
                        "The Zune is wedged and will not come back on its own. {}",
                        usb::REPLUG_HINT
                    )));
                }
            }
        }
    }

    // ── command dispatch ─────────────────────────────────────────────────────

    /// Every audio object handle on the storage, across the formats this device uses.
    fn audio_handles(
        session: &mut Session,
        storage: u32,
    ) -> Result<std::collections::HashSet<u32>> {
        let mut live = std::collections::HashSet::new();
        for format in [fmt::MP3, fmt::WMA, fmt::AAC] {
            let handles = session.object_handles(storage, format, ROOT)?;
            live.extend(handles.into_iter().filter(|h| *h != 0));
        }
        Ok(live)
    }

    /// Run the embedded-art sweep the first time a session and a library are both here.
    fn sweep_embedded_art_if_ready(&mut self, ev: &Emitter) {
        if self.harvested || self.albums.is_empty() || self.session.is_none() || self.busy {
            return;
        }
        self.harvested = true;
        let albums = self.albums.clone();
        let serial = self.snapshot.serial.clone();
        if let Some(session) = self.session.as_mut() {
            Self::harvest_embedded_art(session, &serial, &albums, ev);
        }
    }

    fn handle(
        &mut self,
        cmd: Command,
        ev: &Emitter,
        cancel: &AtomicBool,
        interrupt: &AtomicBool,
    ) {
        self.sweep_embedded_art_if_ready(ev);
        match cmd {
            Command::Shutdown => {}
            Command::Connect => {
                self.ejected = false;
                self.last_error = None;
                if self.session.is_none() {
                    self.try_connect(ev);
                }
            }
            Command::Disconnect => {
                self.drop_session();
                ev.link(Link::Disconnected);
            }
            Command::Eject => {
                ev.log(LogLine::info("Ejecting — closing the MTP session"));
                self.drop_session();
                self.ejected = true;
                ev.link(Link::Disconnected);
                ev.log(LogLine::ok("Ejected. The Zune is safe to unplug."));
            }
            Command::Refresh { force } => {
                // Refresh means "ask again": drop the recorded cover misses so the
                // fallback ladder runs once more.
                let serial = self.snapshot.serial.clone();
                if !serial.is_empty() {
                    let cleared = coverfetch::clear_misses(&serial);
                    if cleared > 0 {
                        ev.log(LogLine::info(format!(
                            "cleared {cleared} recorded cover miss(es) — the local files and \
                             iTunes will be consulted again"
                        )));
                    }
                }
                if let Err(e) = self.refresh(ev, interrupt, force) {
                    // A read that died because the link died must not be retried on the
                    // same wedged session: recover the link, or ask for the cable.
                    if usb::is_stuck(&e) {
                        ev.log(LogLine::error(format!(
                            "Refresh failed and the link looks dead: {e:#}"
                        )));
                        self.recover_link(ev);
                    } else {
                        ev.log(LogLine::error(format!("Refresh failed: {e:#}")));
                    }
                }
            }
            Command::Delete(handles) => self.run_task(ev, "Deleting", |b, ev| b.delete(&handles, ev)),
            Command::PushTracks(paths) => {
                self.run_task(ev, "Pushing", move |b, ev| b.push_tracks(&paths, ev, cancel))
            }
            Command::PushAlbum { dir, name, cover, playlist } => self.run_task(ev, "Pushing album", move |b, ev| {
                b.push_album(&dir, name.as_deref(), cover.as_deref(), playlist, ev, cancel)
            }),
            Command::FetchCover(handle) => self.fetch_cover(handle, ev),
            Command::FetchFallbackCover { key, artist, album, track_handle } => {
                self.fetch_fallback_cover(key, &artist, &album, track_handle, ev)
            }
            Command::SetCover { key, artist, album, path } => {
                self.set_cover(key, &artist, &album, &path, ev)
            }
            Command::SyncScan => {
                // Deliberately not `run_task`: a plan needs no session, and must not
                // demand one — the whole point is that it works with the Zune unplugged.
                if let Err(e) = self.plan_sync(ev) {
                    ev.log(LogLine::error(format!(
                        "Could not scan the local folders: {e:#}"
                    )));
                    ev.busy(false);
                    ev.task_finished(false, "local scan failed");
                }
            }
            Command::SyncSources(sources) => self.set_sync_sources(sources, ev),
            Command::SyncRun(paths) => {
                self.run_task(ev, "Syncing", move |b, ev| b.run_sync(&paths, ev, cancel))
            }
            Command::DiagnoseAlbums(limit) => self.diagnose_albums(limit, ev),
            Command::DiagnoseCovers { images, albums } => self.diagnose_covers(images, albums, ev),
            Command::WifiScan => self.wifi_scan(ev),
            Command::WifiTest => self.wifi_simple(mtp::OP_WIFI_TEST_CONNECTIVITY, "WiFi connectivity test", ev),
            Command::WifiProfile(blob) => self.wifi_profile(&blob, ev),
            Command::UsbReset => self.usb_reset(ev),
            Command::Cancel => {
                cancel.store(true, Ordering::Relaxed);
                ev.log(LogLine::warn("Cancel requested — stopping after the current track"));
            }
        }
    }

    /// Wrap a library-mutating task with the busy flag and progress bookkeeping.
    fn run_task(
        &mut self,
        ev: &Emitter,
        task: &str,
        body: impl FnOnce(&mut Backend, &Emitter) -> Result<String>,
    ) {
        if self.session.is_none() {
            ev.log(LogLine::error(format!("{task} needs a connected device")));
            ev.task_finished(false, "no device");
            return;
        }
        self.busy = true;
        ev.busy(true);
        ev.task_started(task, 0);
        let started = Instant::now();
        let outcome = body(self, ev);
        self.busy = false;
        ev.busy(false);
        match outcome {
            Ok(summary) => {
                let summary = format!("{summary} ({:.1}s)", started.elapsed().as_secs_f32());
                ev.log(LogLine::ok(format!("{task} finished — {summary}")));
                ev.task_finished(true, summary);
            }
            Err(e) => {
                ev.log(LogLine::error(format!("{task} failed: {e:#}")));
                ev.task_finished(false, format!("{e:#}"));
            }
        }
    }

    // ── library ──────────────────────────────────────────────────────────────

    fn refresh(&mut self, ev: &Emitter, interrupt: &AtomicBool, force: bool) -> Result<()> {
        if self.session.is_none() {
            bail!("not connected to a Zune");
        }
        if self.snapshot.key_error.is_some() {
            bail!("the MTPZ session is not trusted yet (no key file)");
        }

        ev.busy(true);
        ev.task_started("Reading the library", 0);
        let started = Instant::now();

        let storage = self.storage;

        // One round trip before the expensive pass. This firmware refuses the bulk property
        // list (0x9805), so a full read is one property read per track — over two minutes
        // for this library, measured. `GetObjectHandles` answers the same question in a
        // single exchange: if the set of object handles is exactly what the library on
        // screen was built from, nothing has changed and there is nothing to re-read.
        //
        // The set is compared exactly, not as a subset, so a deletion is caught too.
        let known = self.cached_track_handles.clone();
        let mut unchanged = false;
        if !force && !known.is_empty() {
            if let Some(session) = self.session.as_mut() {
                // Audio formats only. Asking for `format = 0` returns every object on the
                // device, photos and videos included — measured: 3061 handles against the
                // library's 2425 tracks, which made the comparison always say "changed".
                // Three round trips still beats one property read per track.
                match Self::audio_handles(session, storage) {
                    Ok(live) => {
                        unchanged = live == known;
                        if !unchanged {
                            log::info!(
                                "device changed: {} audio objects vs {} cached ({} new, {} gone)",
                                live.len(),
                                known.len(),
                                live.difference(&known).count(),
                                known.difference(&live).count()
                            );
                        } else {
                            log::info!("device unchanged: {} audio objects", live.len());
                        }
                    }
                    Err(e) => log::debug!("change check unavailable, reading in full: {e:#}"),
                }
            }
        }
        if unchanged {
            let elapsed = started.elapsed().as_secs_f32();
            ev.log(LogLine::ok(format!(
                "device unchanged ({elapsed:.1}s): {} objects, same as the library on screen — \
                 nothing to re-read. Shift-click Refresh to force a full pass.",
                known.len()
            )));
            ev.busy(false);
            return Ok(());
        }

        let session = self.session.as_mut().expect("checked above");

        // Storage info is optional: firmware that does not implement 0x1005 just
        // leaves the numbers blank.
        let sinfo = match session.storage_info(storage) {
            Ok(s) => Some(s),
            Err(e) => {
                log::debug!("GetStorageInfo unavailable: {e:#}");
                None
            }
        };

        let mut lib = Library::new(session)?;
        // Remember that the bulk property list is unavailable, so a refresh does not pay
        // the device's 30-second timeout again (it did, on a real Zune 30).
        lib.bulk_props_unavailable = self.bulk_props_unavailable;
        let read_started = Instant::now();
        let mut interrupted = false;
        let device_tracks = lib.tracks_reported(|done, total| {
            ev.progress_throttled(Progress {
                task: "Reading the library".to_owned(),
                step: format!("track {done} of {total}"),
                items_done: done,
                items_total: total,
                ..Default::default()
            });
            if interrupt.load(Ordering::Relaxed) {
                interrupted = true;
            }
        })?;
        self.bulk_props_unavailable = lib.bulk_props_unavailable;
        if interrupted {
            ev.log(LogLine::warn(
                "library read stopped early — a diagnostic needed the device",
            ));
        }

        ev.progress(Progress {
            task: "Reading the library".to_owned(),
            step: "albums and playlists".to_owned(),
            items_done: device_tracks.len(),
            items_total: device_tracks.len(),
            ..Default::default()
        });
        ev.log(LogLine::info(format!(
            "library read: {} tracks in {:.1}s via {}",
            device_tracks.len(),
            read_started.elapsed().as_secs_f32(),
            lib.last_sweep.label()
        )));

        let groups = lib.groups().unwrap_or_default();
        // Playlists with their member handles, so the cache can diff them later.
        // `groups()` already reads each playlist's references and discards the list;
        // playlists are few, so the second read is noise next to 2425 tracks.
        let playlists: Vec<(u32, String, Vec<u32>)> = groups
            .iter()
            .filter(|g| g.kind == "playlist")
            .map(|g| {
                let name = g.name.clone().unwrap_or_else(|| g.filename.clone());
                (g.handle, name, lib.refs(g.handle))
            })
            .collect();
        let raw_albums = lib.albums_with_refs().unwrap_or_default();

        // The library counts property reads that failed, so "the device has no tags"
        // stays distinguishable from "we asked for the wrong thing".
        if lib.prop_failures > 0 {
            ev.log(LogLine::warn(format!(
                "{} metadata property read(s) were refused by the device{}",
                lib.prop_failures,
                lib.last_prop_error
                    .as_ref()
                    .map(|e| format!(" — last error: {e}"))
                    .unwrap_or_default()
            )));
        }

        // Album properties + cover art. The device's albums were written by Microsoft's
        // own software over the years, so nothing about their shape can be assumed:
        // every candidate is classified by asking the device what it actually is.
        let mut date_code = self.album_date_code;
        let abstract_albums: Vec<AlbumEntry> = raw_albums
            .into_iter()
            .map(|(name, handle, refs)| {
                let album_artist = lib.session.get_prop_str(handle, mtp::prop::ALBUM_ARTIST).ok().filter(|v| !v.is_empty());
                let year = read_album_year(&mut lib.session, handle, &mut date_code);
                AlbumEntry { name, handle, refs, album_artist, year }
            })
            .collect();
        self.album_date_code = date_code;
        if date_code.is_some() {
            log::debug!("album year property: {:#06x}", date_code.unwrap_or(0));
        }

        let tracks: Vec<Track> = device_tracks
            .iter()
            .map(|t| Track {
                handle: t.handle,
                filename: t.filename.clone(),
                size: t.size,
                format: t.format,
                title: t.title.clone().unwrap_or_default(),
                artist: t.artist.clone().unwrap_or_default(),
                album: t.album.clone().unwrap_or_default(),
                track_no: t.track,
                duration: duration_seconds(t.duration_ms),
            })
            .collect();

        let mut albums = build_albums(&tracks, &abstract_albums);
        for album in &mut albums {
            // The UI keys its textures by this: a device handle when the device has
            // art, otherwise a synthetic key derived from artist + album.
            album.cover_key = album
                .cover_handle
                .unwrap_or_else(|| coverfetch::synthetic_key(&album.artist, &album.name));
        }

        // Cover art: resolve per album, and only pay for what cannot be decided from
        // the references we already have.
        ev.progress(Progress {
            task: "Reading the library".to_owned(),
            step: format!("resolving cover art for {} albums", albums.len()),
            items_done: device_tracks.len(),
            items_total: device_tracks.len(),
            ..Default::default()
        });
        let cover_report = resolve_covers(
            &mut lib,
            &mut albums,
            &abstract_albums,
            ev,
            &mut self.cover_index,
        );
        self.albums = albums.clone();
        self.harvested = false;
        let need_fallback = albums.iter().filter(|a| !a.cover_source.is_found()).count();
        if need_fallback > 0 {
            ev.log(LogLine::info(format!(
                "cover art: {need_fallback} of {} albums have no art on the device — the \
                 Albums page looks for those in the local files and online (iTunes)",
                albums.len()
            )));
            // No bulk sweep here: `lib` holds the session for the rest of this function,
            // and the per-album fallback now tries the audio file first anyway.
            Self::spawn_cover_prefetch(&self.snapshot.serial, &albums, ev);
        }
        if cover_report.found > 0 {
            ev.log(LogLine::info(format!(
                "cover art: {} of {} albums have art on the device ({})",
                cover_report.found,
                albums.len(),
                cover_report.sources_summary()
            )));
        } else if !albums.is_empty() {
            ev.log(LogLine::warn(format!(
                "cover art: none of the {} albums resolved to a cover object — open the Log page and press \
                 \"Diagnose albums\" so the reference structure can be checked",
                albums.len()
            )));
        }
        // How the references were read is a fact worth stating: an album with no cover
        // and an album whose reference read failed look identical otherwise.
        ev.log(LogLine::info(format!(
            "object references: served by {}",
            lib.last_refs_path.label()
        )));
        if let Some(error) = &lib.last_refs_error {
            ev.log(LogLine::warn(format!("reference read problem: {error}")));
        }
        for (name, source, reason) in &cover_report.unresolved {
            ev.log(LogLine::warn(format!(
                "[alb] \"{name}\": no cover found ({}){reason}",
                source.label()
            )));
        }

        // Snapshot updates.
        self.snapshot.tracks = tracks.len();
        self.snapshot.albums = albums.len();
        self.snapshot.playlists = groups.iter().filter(|g| g.kind == "playlist").count();
        self.snapshot.library_bytes = tracks.iter().map(|t| t.size as u64).sum();
        // A breakdown makes a surprising library obvious (e.g. "all WMA").
        let mut by_format: std::collections::BTreeMap<u16, usize> = Default::default();
        for track in &tracks {
            *by_format.entry(track.format).or_default() += 1;
        }
        self.snapshot.format_summary = by_format
            .iter()
            .map(|(format, count)| format!("{count} {}", core::device::format_name::of(*format)))
            .collect::<Vec<_>>()
            .join(" · ");
        if let Some(s) = sinfo {
            self.snapshot.capacity = Some(s.max_capacity);
            self.snapshot.free = Some(s.free_space_bytes);
            if !s.description.is_empty() {
                self.snapshot.storage_desc = s.description;
            } else if !s.volume_label.is_empty() {
                self.snapshot.storage_desc = s.volume_label;
            }
        }

        // Persist before handing the library to the UI: a crash right after the
        // event must not leave the next start with a cold cache.
        let mut cache_written = cache::LibraryCache::from_library(
            &self.snapshot,
            cache::now_unix(),
            &tracks,
            &albums,
            &playlists,
        );
        // The device read is the only place that still has the raw Duration (ms)
        // and the album artist; fold those in before writing.
        cache_written.enrich_from_device_tracks(&device_tracks);
        if interrupted {
            // A read cut short by a diagnostic is not the library: writing it down
            // would poison every later start with a truncated cache.
            ev.log(LogLine::warn(
                "library read was interrupted — the cache was left unchanged",
            ));
        } else if let Err(e) = cache_written.save() {
            ev.log(LogLine::warn(format!(
                "could not write the library cache — the next start will read the device again: {e:#}"
            )));
        }

        self.busy = false;
        ev.busy(false);
        self.tracks = tracks.clone();
        ev.send(Event::Library { tracks, albums });
        ev.send(Event::Origin(Box::new(DataOrigin {
            source: Source::Live,
            read_at: Some(cache_written.read_at),
            device_connected: true,
            serial: self.snapshot.serial.clone(),
            note: None,
        })));
        ev.send(Event::Snapshot(Box::new(self.snapshot.clone())));
        let summary = format!(
            "{} tracks · {} albums · {} playlists in {:.1}s",
            self.snapshot.tracks,
            self.snapshot.albums,
            self.snapshot.playlists,
            started.elapsed().as_secs_f32()
        );
        ev.task_finished(true, summary.clone());
        ev.progress(Progress {
            task: "Reading the library".to_owned(),
            step: summary.clone(),
            items_done: self.snapshot.tracks,
            items_total: self.snapshot.tracks,
            ..Default::default()
        });
        Ok(())
    }

    // ── push ─────────────────────────────────────────────────────────────────

    fn push_tracks(&mut self, paths: &[PathBuf], ev: &Emitter, cancel: &AtomicBool) -> Result<String> {
        let files = expand_audio(paths)?;
        if files.is_empty() {
            bail!("no audio files in the selection ({} entries)", paths.len());
        }
        let total = files.len();
        let mut pushed = 0usize;
        let mut failed = 0usize;
        for (index, path) in files.iter().enumerate() {
            if cancel.load(Ordering::Relaxed) {
                ev.log(LogLine::warn(format!(
                    "stopped after {index} of {total} files — the rest of the queue was not pushed"
                )));
                break;
            }
            match self.push_one(path, index, total, ev) {
                Ok(()) => pushed += 1,
                Err(e) => {
                    failed += 1;
                    ev.log(LogLine::error(format!("{}: {e:#}", file_label(path))));
                    // A dead link makes every later file fail too; stop early.
                    if usb::is_stuck(&e) {
                        ev.log(LogLine::warn("the USB link looks dead — stopping the queue"));
                        break;
                    }
                }
            }
        }
        Ok(format!("{pushed} of {total} tracks pushed, {failed} failed"))
    }

    /// Convert one file and push it as a track, with byte-level progress.
    fn push_one(&mut self, path: &Path, index: usize, total: usize, ev: &Emitter) -> Result<()> {
        let label = file_label(path);
        ev.progress(Progress {
            task: "Pushing".to_owned(),
            step: format!("{label} — reading tags"),
            items_done: index,
            items_total: total,
            ..Default::default()
        });
        let track_tags = tags::read(path);

        ev.progress(Progress {
            task: "Pushing".to_owned(),
            step: format!("{label} — converting"),
            items_done: index,
            items_total: total,
            ..Default::default()
        });
        let converted = core::media::to_mp3(path).with_context(|| format!("converting {label}"))?;
        let bytes = converted.data.len() as u64;

        let session = self.session.as_mut().context("not connected")?;
        let mut lib = Library::new(session)?;
        let handle = lib
            .push_track_reported(path, &converted.data, &track_tags, &mut |sent, tot| {
                ev.progress_throttled(Progress {
                    task: "Pushing".to_owned(),
                    step: format!("{label} — {:.1} / {:.1} MB", sent as f64 / 1e6, tot as f64 / 1e6),
                    items_done: index,
                    items_total: total,
                    bytes_done: sent as u64,
                    bytes_total: tot as u64,
                });
            })
            .with_context(|| format!("uploading {label}"))?;

        ev.progress(Progress {
            task: "Pushing".to_owned(),
            step: format!("{label} — done"),
            items_done: index + 1,
            items_total: total,
            bytes_done: bytes,
            bytes_total: bytes,
        });
        ev.log(LogLine::ok(format!(
            "pushed {label} as {} ({:.1} MB)",
            track_tags.title,
            bytes as f64 / 1e6
        )));
        log::debug!("track handle {handle:#010x} for {label}");
        Ok(())
    }

    /// The album workflow from `lib.sync_album`, but reporting progress and
    /// honouring the GUI's options.
    fn push_album(
        &mut self,
        dir: &Path,
        name: Option<&str>,
        cover: Option<&Path>,
        make_playlist: bool,
        ev: &Emitter,
        cancel: &AtomicBool,
    ) -> Result<String> {
        if !dir.is_dir() {
            bail!("{} is not a directory", dir.display());
        }
        let files = library::audio_files(dir)?;
        if files.is_empty() {
            bail!("no audio files in {}", dir.display());
        }

        let mut track_tags: Vec<(PathBuf, core::TrackTags)> =
            files.iter().map(|p| (p.clone(), tags::read(p))).collect();
        track_tags.sort_by_key(|(p, t)| (t.track.unwrap_or(0), p.clone()));

        let album_name = match name.filter(|n| !n.trim().is_empty()) {
            Some(n) => n.trim().to_owned(),
            None => track_tags
                .first()
                .and_then(|(_, t)| t.album.clone())
                .unwrap_or_else(|| library::derive_album_name(dir)),
        };
        let total = track_tags.len();
        ev.log(LogLine::info(format!(
            "Album \"{album_name}\": {total} tracks from {}",
            dir.display()
        )));

        let mut refs: Vec<u32> = Vec::new();
        for (index, (path, track_tags)) in track_tags.iter().enumerate() {
            if cancel.load(Ordering::Relaxed) {
                ev.log(LogLine::warn(format!(
                    "stopped after {index} of {total} tracks; no playlist/album object was created"
                )));
                return Ok(format!("{index} of {total} tracks pushed (cancelled)"));
            }
            let label = file_label(path);
            ev.progress(Progress {
                task: "Pushing album".to_owned(),
                step: format!("{album_name} — {label} ({} of {total})", index + 1),
                items_done: index,
                items_total: total,
                ..Default::default()
            });
            let converted =
                core::media::to_mp3(path).with_context(|| format!("converting {label}"))?;
            let session = self.session.as_mut().context("not connected")?;
            let mut lib = Library::new(session)?;
            let handle = lib
                .push_track_reported(path, &converted.data, track_tags, &mut |sent, tot| {
                    ev.progress_throttled(Progress {
                        task: "Pushing album".to_owned(),
                        step: format!(
                            "{album_name} — {label} {:.1}/{:.1} MB",
                            sent as f64 / 1e6,
                            tot as f64 / 1e6
                        ),
                        items_done: index,
                        items_total: total,
                        bytes_done: sent as u64,
                        bytes_total: tot as u64,
                    });
                })
                .with_context(|| format!("uploading {label}"))?;
            ev.log(LogLine::ok(format!("  ✓ {} — {}", label, track_tags.title)));
            refs.push(handle);
        }

        let session = self.session.as_mut().context("not connected")?;
        let mut lib = Library::new(session)?;

        let playlist = if make_playlist {
            ev.progress(Progress {
                task: "Pushing album".to_owned(),
                step: format!("playlist {album_name}.pla"),
                items_done: total,
                items_total: total,
                ..Default::default()
            });
            let h = lib.create_playlist(&album_name, &refs)?;
            ev.log(LogLine::ok(format!("  ✓ playlist {album_name}.pla")));
            Some(h)
        } else {
            None
        };

        let cover_handle = match library::pick_cover(dir, cover) {
            Some(path) => {
                ev.progress(Progress {
                    task: "Pushing album".to_owned(),
                    step: format!("cover art {}", file_label(&path)),
                    items_done: total,
                    items_total: total,
                    ..Default::default()
                });
                let h = lib.push_cover_art(&path)?;
                ev.log(LogLine::ok(format!("  ✓ cover art {}", file_label(&path))));
                Some(h)
            }
            None => {
                ev.log(LogLine::info("  · no cover art found (looked for the largest JPEG)"));
                None
            }
        };

        let mut album_refs = refs.clone();
        if let Some(c) = cover_handle {
            album_refs.push(c);
        }
        let album_handle = lib.create_abstract_album(&album_name, &album_refs)?;
        ev.log(LogLine::ok(format!(
            "  ✓ abstract album \"{album_name}\" ({album_handle:#010x}) — restart the Zune to see it under Music → Albums"
        )));

        Ok(format!(
            "{total} tracks, playlist {}, cover {}, album object {}",
            playlist.map(hex_or_dash).unwrap_or_else(|| "—".into()),
            cover_handle.map(hex_or_dash).unwrap_or_else(|| "—".into()),
            hex_or_dash(album_handle)
        ))
    }

    // ── delete ───────────────────────────────────────────────────────────────

    fn delete(&mut self, handles: &[u32], ev: &Emitter) -> Result<String> {
        let total = handles.len();
        let mut done = 0usize;
        let mut failures: Vec<String> = Vec::new();
        for (index, handle) in handles.iter().enumerate() {
            ev.progress(Progress {
                task: "Deleting".to_owned(),
                step: format!("{} of {total}", index + 1),
                items_done: index,
                items_total: total,
                ..Default::default()
            });
            let session = self.session.as_mut().context("not connected")?;
            let mut lib = Library::new(session)?;
            match lib.remove(*handle) {
                Ok(()) => done += 1,
                Err(e) => failures.push(format!("{handle:#010x}: {e:#}")),
            }
        }
        for f in &failures {
            ev.log(LogLine::error(format!("could not delete {f}")));
        }
        if done > 0 && !failures.is_empty() {
            ev.log(LogLine::warn(
                "some objects are still on the device — press Refresh to see the real state",
            ));
        }
        Ok(format!("{done} of {total} objects deleted"))
    }

    // ── cover art ────────────────────────────────────────────────────────────

    fn fetch_cover(&mut self, handle: u32, ev: &Emitter) {
        if covers_disabled() {
            return;
        }
        let serial = self.snapshot.serial.clone();

        // A cover downloaded once is decoded from local bytes: the album wall is
        // instant on a cached library, and it still works with the device unplugged.
        if let Some(data) = cache::read_cover(&serial, handle) {
            match decode_cover(&data) {
                Ok(image) => {
                    log::debug!("cover {handle:#010x}: {} bytes from the local cache", data.len());
                    ev.send(Event::Cover { handle, image, origin: "device".to_owned() });
                    return;
                }
                Err(e) => log::debug!(
                    "cached cover {handle:#010x} would not decode ({e:#}) — asking the device"
                ),
            }
        }

        let Some(session) = self.session.as_mut() else {
            ev.send(Event::CoverFailed { handle, reason: "not connected".to_owned() });
            return;
        };

        // Two ways in: GetObject is the spec's answer, but a firmware that keeps its
        // cover art behind a vendor path may only serve the cached thumbnail.
        let mut reasons: Vec<String> = Vec::new();
        let mut payload: Option<Vec<u8>> = None;

        match session.get_object_data(handle) {
            Ok(data) if data.is_empty() => reasons.push("GetObject returned an empty payload".to_owned()),
            Ok(data) => payload = Some(data),
            Err(e) => reasons.push(format!("GetObject: {e:#}")),
        }
        if payload.is_none() {
            match session.get_thumb_data(handle, fmt::JPEG) {
                Ok(data) if data.is_empty() => reasons.push("GetThumb returned an empty payload".to_owned()),
                Ok(data) => payload = Some(data),
                Err(e) => reasons.push(format!("GetThumb: {e:#}")),
            }
        }

        let Some(data) = payload else {
            // The UI logs this (and the tile carries it on hover); the worker keeps it
            // at debug level so one failure produces exactly one visible line.
            let reason = reasons.join(" · ");
            log::debug!("cover {handle:#010x} could not be downloaded: {reason}");
            ev.send(Event::CoverFailed { handle, reason });
            return;
        };

        // Say *what* came back, not just that it would not decode.
        if let Some(kind) = describe_payload(&data) {
            if !kind.is_image() {
                let reason = format!(
                    "the object is {} ({} bytes), not an image — it is not cover art",
                    kind.name(),
                    data.len()
                );
                log::debug!("cover {handle:#010x}: {reason}");
                ev.send(Event::CoverFailed { handle, reason });
                return;
            }
        }

        match decode_cover(&data) {
            Ok(image) => {
                // Cache only bytes that decoded, so a later lookup always decodes.
                if let Err(e) = cache::write_cover(&serial, handle, &data) {
                    log::debug!("cover {handle:#010x} not cached: {e:#}");
                }
                log::debug!(
                    "cover {handle:#010x}: {} bytes, {}x{}",
                    data.len(),
                    image.width,
                    image.height
                );
                ev.send(Event::Cover { handle, image, origin: "device".to_owned() });
            }
            Err(e) => {
                let reason = format!(
                    "decoding {} bytes failed ({e:#}); payload starts {:02x?}",
                    data.len(),
                    &data[..data.len().min(12)]
                );
                log::debug!("cover {handle:#010x}: {reason}");
                ev.send(Event::CoverFailed { handle, reason });
            }
        }
    }

    /// Fast cover-art diagnosis.
    ///
    /// Reads **nothing** of the track library: a handful of image objects plus a few
    /// albums, so it answers in seconds on a device whose full library takes minutes.
    /// The questions are the ones the Mac evidence raised: where do the cover JPEGs
    /// live, and what actually links them to an album?
    fn diagnose_covers(&mut self, images: usize, albums: usize, ev: &Emitter) {
        let Some(session) = self.session.as_mut() else {
            ev.log(LogLine::error("Diagnosing covers needs a connected device"));
            return;
        };
        let storage = self.storage;
        let images = images.clamp(1, 20);
        let albums = albums.clamp(1, 20);
        let started = Instant::now();
        ev.log(LogLine::info(
            "[cov] ---- cover diagnosis (image objects + a few albums only) ----",
        ));

        // ── 1. find the image objects through every route that works ──
        //
        // A Zune 30 refuses the wildcard parent query (parent=0xFFFFFFFF) with
        // GeneralError, so the *direct* listings are the ones that matter: it keeps its
        // 95 cover JPEGs as direct children of the root.
        let mut image_handles: Vec<u32> = Vec::new();
        let mut add = |handles: Vec<u32>, image_handles: &mut Vec<u32>| {
            for handle in handles {
                if !image_handles.contains(&handle) {
                    image_handles.push(handle);
                }
            }
        };
        for (label, format) in [("JPEG (0x3801)", fmt::JPEG), ("CoverArt (0x3802)", fmt::COVER_ART)] {
            match session.children(format, ROOT) {
                Ok(children) => {
                    ev.log(LogLine::ok(format!(
                        "[cov] {label}: {} object(s) directly under the root{}",
                        children.len(),
                        if format == fmt::JPEG { "   <-- the images to inspect" } else { "" }
                    )));
                    if format == fmt::JPEG {
                        add(children, &mut image_handles);
                    }
                }
                Err(e) => ev.log(LogLine::info(format!(
                    "[cov] {label}: root listing failed: {e:#}"
                ))),
            }
            match session.object_handles(storage, format, u32::MAX) {
                Ok(all) => ev.log(LogLine::info(format!(
                    "[cov] {label}: {} object(s) across all parents (wildcard query)",
                    all.len()
                ))),
                Err(e) => ev.log(LogLine::info(format!(
                    "[cov] {label}: the wildcard-parent query is not supported here ({e:#})"
                ))),
            }
        }
        // Images could also sit inside a folder; walk them all (cheap: folders only).
        if let Ok(folders) = session.children(fmt::ASSOCIATION, ROOT) {
            for folder in folders {
                for format in [fmt::JPEG, fmt::COVER_ART] {
                    if let Ok(children) = session.children(format, folder) {
                        if !children.is_empty() {
                            let name = session
                                .object_info(folder)
                                .map(|i| i.filename)
                                .unwrap_or_default();
                            ev.log(LogLine::info(format!(
                                "[cov] folder \"{name}\" holds {} object(s) of format {format:#06x}",
                                children.len()
                            )));
                            add(children, &mut image_handles);
                        }
                    }
                }
            }
        }
        ev.log(LogLine::info(format!(
            "[cov] inspecting {} image object(s)",
            image_handles.len().min(images)
        )));
        if image_handles.is_empty() {
            ev.log(LogLine::warn(
                "[cov] no image objects found — the covers are not JPEG objects on this device",
            ));
        }

        // ── 2. the image objects themselves ──
        let mut images_inspected = 0usize;
        let mut looks_like_art = 0usize;
        let mut looks_like_photo = 0usize;
        let mut images_linking_album = 0usize;
        let mut tracks_with_image_ref = 0usize;
        let mut albums_with_image_ref = 0usize;
        let album_handles: Vec<u32> = session.children(fmt::ABSTRACT_ALBUM, ROOT).unwrap_or_default();
        let album_set: std::collections::HashSet<u32> = album_handles.iter().copied().collect();
        for handle in image_handles.iter().take(images) {
            images_inspected += 1;
            let info = session.object_info(*handle).ok();
            match &info {
                Some(info) => ev.log(LogLine::info(format!(
                    "[cov] image {} [{}] format={:#06x} ({}) size={} parent={} [{}] name=\"{}\"",
                    hex(*handle),
                    handle_kind(*handle),
                    info.format,
                    core::device::format_name::of(info.format),
                    info.size,
                    hex(info.parent),
                    handle_kind(info.parent),
                    info.filename
                ))),
                None => ev.log(LogLine::warn(format!("[cov] image {}: GetObjectInfo failed", hex(*handle)))),
            }
            let refs = session.object_refs(*handle).unwrap_or_default();
            let refs_label: Vec<String> = refs
                .iter()
                .map(|r| format!("{} [{}]", hex(*r), handle_kind(*r)))
                .collect();
            ev.log(LogLine::info(format!(
                "[cov]   its own refs (0x9810): {} {}{}",
                refs.len(),
                if refs.is_empty() { "-".to_owned() } else { refs_label.join(" ") },
                if refs.iter().any(|r| album_set.contains(r)) {
                    "   <-- POINTS AT AN ABSTRACT ALBUM"
                } else {
                    ""
                }
            )));
            // A second route, because the opcode may be the broken one.
            match session.get_prop_raw_public(*handle, mtp::prop::OBJECT_REFS) {
                Ok(raw) => {
                    let property_refs = crate::core::mtp::parse_u32_array(&raw).unwrap_or_default();
                    ev.log(LogLine::info(format!(
                        "[cov]   its refs (0xDC41 property): {} {}",
                        property_refs.len(),
                        property_refs
                            .iter()
                            .map(|r| format!("{} [{}]", hex(*r), handle_kind(*r)))
                            .collect::<Vec<_>>()
                            .join(" ")
                    )));
                }
                Err(e) => ev.log(LogLine::info(format!("[cov]   refs property unavailable: {e:#}"))),
            }
            // Every property the device stores for this object — where a link to an
            // album would show up as a handle, or as an album title.
            ev.log(LogLine::info(format!("[cov]   every property the device stores:")));
            match session.object_prop_list(*handle, 0, u16::MAX, 0) {
                Ok(entries) => {
                    let mut linked_to_album = false;
                    for entry in entries.iter().filter(|e| e.handle == *handle) {
                        ev.log(LogLine::info(format!("[cov]     {}", describe_prop(entry))));
                        // A numeric property whose *value* is an album handle is exactly
                        // the link we are looking for.
                        if let Some(number) = entry.number {
                            if album_set.contains(&number) {
                                linked_to_album = true;
                                ev.log(LogLine::ok(format!(
                                    "[cov]     ^^^ this property points at an abstract album!"
                                )));
                            }
                        }
                    }
                    if linked_to_album {
                        images_linking_album += 1;
                    }
                    ev.log(LogLine::info(format!(
                        "[cov]   verdict: {}",
                        if linked_to_album {
                            "an album handle appears among this image's properties"
                        } else {
                            "no property of this image references an abstract album"
                        }
                    )));
                }
                Err(e) => {
                    ev.log(LogLine::warn(format!("[cov]     bulk property read failed: {e:#}")));
                    for code in [
                        mtp::prop::NAME,
                        mtp::prop::ARTIST,
                        mtp::prop::ALBUM,
                        mtp::prop::ALBUM_ARTIST,
                        mtp::prop::DATE_CREATED,
                        mtp::prop::DATE_MODIFIED,
                        mtp::prop::SIZE,
                    ] {
                        if let Ok(text) = session.get_prop_str(*handle, code) {
                            if !text.is_empty() {
                                ev.log(LogLine::info(format!(
                                    "[cov]     {} = \"{text}\"",
                                    prop_label(code)
                                )));
                            }
                        } else if let Ok(number) = session.get_prop_u32(*handle, code) {
                            ev.log(LogLine::info(format!(
                                "[cov]     {} = {} ({})",
                                prop_label(code),
                                number,
                                hex(number)
                            )));
                        }
                    }
                }
            }
            // An explicit verdict, so the dump answers the question instead of leaving
            // the reader to eyeball hundreds of lines.
            ev.log(LogLine::info(format!(
                "[cov]   parent is {} [{}]{}",
                hex(info.as_ref().map(|i| i.parent).unwrap_or(0)),
                handle_kind(info.as_ref().map(|i| i.parent).unwrap_or(0)),
                if info.as_ref().map(|i| i.parent) == Some(ROOT) {
                    " (the device root)"
                } else {
                    ""
                }
            )));
            // ── the pixel-level verdict ──
            //
            // Properties can only tell us how the firmware labels an object. The bytes
            // say what it *is*: a camera photo is 4:3 or 3:2, carries an EXIF block and
            // is megapixels; album art is square, small, and has no EXIF. This is what
            // settles "are these covers or the user's photos".
            match session.get_object_data(*handle) {
                Ok(bytes) if !bytes.is_empty() => {
                    match image::load_from_memory(&bytes) {
                        Ok(decoded) => {
                            let (w, h) = (decoded.width(), decoded.height());
                            let ratio = w as f32 / h.max(1) as f32;
                            let square = (ratio - 1.0).abs() < 0.06;
                            let has_exif = jpeg_has_exif(&bytes);
                            ev.log(LogLine::info(format!(
                                "[cov]   picture: {w}×{h} (ratio {ratio:.2}), {:.0} KB, EXIF {}",
                                bytes.len() as f32 / 1024.0,
                                if has_exif { "yes" } else { "no" }
                            )));
                            let verdict = match (square, has_exif, w.max(h)) {
                                (true, false, max) if max <= 1400 => "album art (square, no EXIF, display-sized)",
                                (true, true, _) => "a cropped photo (square but carries EXIF)",
                                (false, true, _) => "a camera photo (not square, carries EXIF)",
                                (false, false, max) if max >= 1200 => "a photo (large, not square)",
                                _ => "unclear — small and not square",
                            };
                            if verdict.starts_with("album art") {
                                looks_like_art += 1;
                            } else if verdict.contains("photo") {
                                looks_like_photo += 1;
                            }
                            ev.log(LogLine::info(format!("[cov]   looks like: {verdict}")));
                        }
                        Err(e) => ev.log(LogLine::warn(format!(
                            "[cov]   picture: will not decode ({e:#}); {} bytes starting {}",
                            bytes.len(),
                            magic(&bytes)
                        ))),
                    }
                }
                Ok(_) => ev.log(LogLine::warn(format!("[cov]   GetObject {} -> empty", hex(*handle)))),
                Err(e) => ev.log(LogLine::warn(format!(
                    "[cov]   GetObject {} failed: {e:#}",
                    hex(*handle)
                ))),
            }
            // GetThumb, the fallback path when GetObject is refused.
            match session.get_thumb_data(*handle, fmt::JPEG) {
                Ok(data) if data.is_empty() => {
                    ev.log(LogLine::warn(format!("[cov]   GetThumb {} -> empty", hex(*handle))))
                }
                Ok(data) => ev.log(LogLine::ok(format!(
                    "[cov]   GetThumb {} -> {} bytes, magic {}",
                    hex(*handle),
                    data.len(),
                    magic(&data)
                ))),
                Err(e) => ev.log(LogLine::warn(format!(
                    "[cov]   GetThumb {} failed: {e:#}",
                    hex(*handle)
                ))),
            }
        }

        // ── 3. the albums, and what their references actually contain ──
        ev.log(LogLine::info(format!(
            "[cov] {} abstract album object(s); dumping {albums}",
            album_handles.len()
        )));
        for handle in album_handles.iter().take(albums) {
            let name = session
                .get_prop_str(*handle, mtp::prop::NAME)
                .ok()
                .filter(|v| !v.is_empty())
                .unwrap_or_else(|| "(unnamed)".to_owned());
            ev.log(LogLine::info(format!(
                "[cov] album {} [{}] \"{name}\"",
                hex(*handle),
                handle_kind(*handle)
            )));
            // Both routes, so a broken one is visible rather than silent.
            match session.object_refs(*handle) {
                Ok(refs) => ev.log(LogLine::info(format!(
                    "[cov]   refs via 0x9810: {}",
                    refs.len()
                ))),
                Err(e) => ev.log(LogLine::warn(format!("[cov]   refs via 0x9810 failed: {e:#}"))),
            }
            let refs = session.object_refs(*handle).unwrap_or_default();
            let refs = if refs.is_empty() {
                session
                    .get_prop_raw_public(*handle, mtp::prop::OBJECT_REFS)
                    .ok()
                    .and_then(|raw| crate::core::mtp::parse_u32_array(&raw).ok())
                    .unwrap_or_default()
            } else {
                refs
            };
            let mut format_counts: std::collections::BTreeMap<u16, usize> = Default::default();
            let mut found_image = false;
            for reference in &refs {
                let info = session.object_info(*reference).ok();
                let (format, filename, size) = info
                    .as_ref()
                    .map(|i| (i.format, i.filename.clone(), i.size))
                    .unwrap_or((0, "?".to_owned(), 0));
                *format_counts.entry(format).or_default() += 1;
                let is_image = format == fmt::JPEG || format == fmt::COVER_ART;
                found_image |= is_image;
                ev.log(LogLine::info(format!(
                    "[cov]     ref {} [{}] format={:#06x} ({}) size={} name=\"{}\"{}",
                    hex(*reference),
                    handle_kind(*reference),
                    format,
                    core::device::format_name::of(format),
                    size,
                    filename,
                    if is_image { "   <-- IMAGE REFERENCE" } else { "" }
                )));
            }
            ev.log(LogLine::info(format!(
                "[cov]   {} refs: {}{}",
                refs.len(),
                format_counts
                    .iter()
                    .map(|(f, n)| format!("{f:#06x}({})×{n}", core::device::format_name::of(*f)))
                    .collect::<Vec<_>>()
                    .join(" "),
                if found_image { "   <-- an image is referenced" } else { "   <-- NO image reference" }
            )));

            // The album itself references nothing but audio. Two places are left for a
            // cover link: the album object's *properties*, and its tracks. Both are a
            // bounded number of calls, and both are printed.
            ev.log(LogLine::info("[cov]   the album's own properties:"));
            match session.object_prop_list(*handle, 0, u16::MAX, 0) {
                Ok(entries) => {
                    for entry in entries.iter().filter(|e| e.handle == *handle) {
                        ev.log(LogLine::info(format!("[cov]     {}", describe_prop(entry))));
                    }
                }
                Err(e) => ev.log(LogLine::warn(format!(
                    "[cov]     bulk property read failed: {e:#} — falling back to the known codes"
                ))),
            }

            // Do any of this album's tracks reference an image?
            let mut track_image_refs = 0usize;
            if found_image {
                albums_with_image_ref += 1;
            }
            for reference in refs.iter().take(40) {
                let refs_of_track = session.object_refs(*reference).unwrap_or_default();
                for candidate in &refs_of_track {
                    let is_image = session
                        .object_info(*candidate)
                        .map(|i| i.format == fmt::JPEG || i.format == fmt::COVER_ART)
                        .unwrap_or(false);
                    if is_image {
                        track_image_refs += 1;
                        ev.log(LogLine::ok(format!(
                            "[cov]     track {} references image {}",
                            hex(*reference),
                            hex(*candidate)
                        )));
                    }
                }
            }
            tracks_with_image_ref += track_image_refs;
            ev.log(LogLine::info(format!(
                "[cov]   tracks referencing an image: {track_image_refs}{}",
                if track_image_refs == 0 {
                    "   <-- the track side carries no cover link either"
                } else {
                    ""
                }
            )));
        }

        // ── 4. every folder on the device, and what it holds ──
        ev.log(LogLine::info("[cov] folders:"));
        match session.children(fmt::ASSOCIATION, ROOT) {
            Ok(folders) => {
                if folders.is_empty() {
                    ev.log(LogLine::info("[cov]   the device reports no folders at the root"));
                }
                for folder in folders {
                    let Ok(info) = session.object_info(folder) else { continue };
                    let mut held: Vec<String> = Vec::new();
                    let mut total = 0usize;
                    for (label, format) in [
                        ("jpeg", fmt::JPEG),
                        ("coverart", fmt::COVER_ART),
                        ("mp3", fmt::MP3),
                        ("wma", fmt::WMA),
                        ("aac", fmt::AAC),
                        ("playlist", fmt::PLAYLIST),
                        ("album", fmt::ABSTRACT_ALBUM),
                        ("folder", fmt::ASSOCIATION),
                    ] {
                        if let Ok(children) = session.children(format, folder) {
                            if !children.is_empty() {
                                total += children.len();
                                held.push(format!("{label}×{}", children.len()));
                            }
                        }
                    }
                    ev.log(LogLine::info(format!(
                        "[cov]   folder {} \"{}\" -> {} child(ren): {}",
                        hex(folder),
                        info.filename,
                        total,
                        if held.is_empty() { "-".to_owned() } else { held.join(" ") }
                    )));
                }
            }
            Err(e) => ev.log(LogLine::warn(format!("[cov] listing root folders failed: {e:#}"))),
        }

        // ── 5. the reverse hypothesis: do images point *back* at albums? ──
        //
        // Reading one ref list per image is cheap (one call each) and settles it: if the
        // link is stored on the image side, this finds it.
        if !image_handles.is_empty() && !album_handles.is_empty() {
            ev.log(LogLine::info(format!(
                "[cov] scanning up to 60 image objects for references back to an album…"
            )));
            let mut linked = 0usize;
            for handle in image_handles.iter().take(60) {
                let refs = session.object_refs(*handle).unwrap_or_default();
                let album_hits: Vec<String> = refs
                    .iter()
                    .filter(|r| album_set.contains(r))
                    .map(|r| hex(*r))
                    .collect();
                if !album_hits.is_empty() {
                    linked += 1;
                    images_linking_album += 1;
                    ev.log(LogLine::ok(format!(
                        "[cov]   image {} -> album(s) {}",
                        hex(*handle),
                        album_hits.join(" ")
                    )));
                }
            }
            ev.log(LogLine::info(format!(
                "[cov] {linked} of the scanned images reference an abstract album{}",
                if linked == 0 {
                    " — so the link is *not* stored on the image side"
                } else {
                    ""
                }
            )));
        }

        // ── 6. the sentence this whole dump exists for ──
        let album_side = albums_with_image_ref > 0 || tracks_with_image_ref > 0;
        let image_side = images_linking_album > 0;
        ev.log(LogLine::info(format!(
            "[cov] SUMMARY: {} image object(s) found; {} inspected; {images_linking_album} of them \
             reference an album; {albums_with_image_ref} of {} albums reference an image; \
             {tracks_with_image_ref} track reference(s) to an image",
            image_handles.len(),
            images_inspected,
            album_handles.len().min(albums),
        )));
        ev.log(LogLine::info(format!(
            "[cov] pixel verdict: {looks_like_art} look(s) like album art, {looks_like_photo} look(s) \
             like photographs (of {images_inspected} inspected)"
        )));
        if !album_side && !image_side {
            ev.log(LogLine::warn(format!(
                "[cov] CONCLUSION: nothing on this device links an image to an album — neither the \
                 album's references, nor the album's properties, nor its tracks, nor the image's own \
                 properties or references. {} The albums therefore have no cover art the device can \
                 show; the UI falls back to a local file or iTunes (see the Albums page), and to \
                 the cyan initial placeholder when nothing is found.",
                if looks_like_photo > looks_like_art {
                    "The images themselves are photographs, not cover art."
                } else if looks_like_art > 0 {
                    "The images look like album art, so the firmware simply does not expose the link \
                     over MTP."
                } else {
                    ""
                }
            )));
        } else {
            ev.log(LogLine::ok(format!(
                "[cov] CONCLUSION: the link is on the {} side — implement the resolver accordingly",
                if image_side { "image" } else { "album" }
            )));
        }
        ev.log(LogLine::info(format!(
            "[cov] ---- end ({} ms) ----",
            started.elapsed().as_millis()
        )));
    }

    /// Dump what the device's albums *actually* contain.
    ///
    /// The point is to stop guessing: the albums were written by Microsoft's own
    /// software, so the reference layout, the property codes and whether cover art is
    /// even reachable are all open questions. Everything printed here is copied
    /// verbatim into the Log page, which has a "Copy to clipboard" button.
    fn diagnose_albums(&mut self, limit: usize, ev: &Emitter) {
        let session_bulk_props_unavailable = self.bulk_props_unavailable;
        let Some(session) = self.session.as_mut() else {
            ev.log(LogLine::error("Diagnosing albums needs a connected device"));
            return;
        };
        let limit = limit.clamp(1, 20);
        ev.log(LogLine::info(format!(
            "[alb] ---- album reference dump (first {limit}) — press \"Copy to clipboard\" on this page ----"
        )));

        let abstracts = match session.children(fmt::ABSTRACT_ALBUM, ROOT) {
            Ok(list) => list,
            Err(e) => {
                ev.log(LogLine::error(format!("[alb] listing abstract albums failed: {e:#}")));
                return;
            }
        };
        ev.log(LogLine::info(format!(
            "[alb] the device holds {} abstract album object(s) (format 0xBA03)",
            abstracts.len()
        )));

        for handle in abstracts.iter().take(limit) {
            let Ok(info) = session.object_info(*handle) else {
                ev.log(LogLine::warn(format!("[alb] {handle:#010x}: GetObjectInfo failed")));
                continue;
            };
            let name = session
                .get_prop_str(*handle, mtp::prop::NAME)
                .ok()
                .filter(|v| !v.is_empty())
                .unwrap_or_else(|| info.filename.clone());
            ev.log(LogLine::info(format!(
                "[alb] \"{name}\" handle={} format={:#06x} size={} parent={:#010x} file=\"{}\"",
                hex(*handle),
                info.format,
                info.size,
                info.parent,
                info.filename
            )));

            let prop = |session: &mut Session, code: u16| -> String {
                match session.get_prop_str(*handle, code) {
                    Ok(v) if !v.is_empty() => format!("\"{v}\""),
                    Ok(_) => "empty".to_owned(),
                    Err(_) => "unsupported".to_owned(),
                }
            };
            let name_prop = prop(session, mtp::prop::NAME);
            let artist_prop = prop(session, mtp::prop::ALBUM_ARTIST);
            let date_created = prop(session, mtp::prop::DATE_CREATED);
            let date_modified = prop(session, mtp::prop::DATE_MODIFIED);
            let date_added = prop(session, mtp::prop::DATE_ADDED);
            ev.log(LogLine::info(format!(
                "[alb]   props NAME={name_prop} ALBUM_ARTIST={artist_prop} DATE(0xDC09)={date_created} DATE(0xDC0A)={date_modified} DATE(0xDC99)={date_added}"
            )));

            let refs = session.object_refs(*handle).unwrap_or_default();
            ev.log(LogLine::info(format!(
                "[alb]   refs ({}): {}",
                refs.len(),
                refs.iter().map(|h| hex(*h)).collect::<Vec<_>>().join(" ")
            )));

            let mut formats: std::collections::BTreeMap<u16, usize> = Default::default();
            let mut cover_candidate: Option<u32> = None;
            for r in &refs {
                let Ok(ri) = session.object_info(*r) else {
                    ev.log(LogLine::warn(format!("[alb]     ref {}: GetObjectInfo failed", hex(*r))));
                    continue;
                };
                *formats.entry(ri.format).or_default() += 1;
                let audio = matches!(ri.format, fmt::MP3 | fmt::WMA | fmt::AAC);
                if !audio && cover_candidate.is_none() {
                    cover_candidate = Some(*r);
                }
                ev.log(LogLine::info(format!(
                    "[alb]     ref {} format={:#06x} ({}) size={} name=\"{}\"{}",
                    hex(*r),
                    ri.format,
                    core::device::format_name::of(ri.format),
                    ri.size,
                    ri.filename,
                    if audio { "" } else { "   <-- not audio" }
                )));
            }
            ev.log(LogLine::info(format!(
                "[alb]   ref formats: {}",
                formats
                    .iter()
                    .map(|(f, n)| format!("{f:#06x}({})×{n}", core::device::format_name::of(*f)))
                    .collect::<Vec<_>>()
                    .join(" ")
            )));

            match cover_candidate {
                Some(candidate) => {
                    let label = session
                        .object_info(candidate)
                        .map(|i| core::device::format_name::of(i.format).to_owned())
                        .unwrap_or_else(|_| "?".to_owned());
                    match session.get_object_data(candidate) {
                        Ok(data) if !data.is_empty() => ev.log(LogLine::ok(format!(
                            "[alb]   GetObject {} ({label}) -> {} bytes, magic {}",
                            hex(candidate),
                            data.len(),
                            magic(&data)
                        ))),
                        Ok(_) => ev.log(LogLine::warn(format!(
                            "[alb]   GetObject {} ({label}) -> empty payload",
                            hex(candidate)
                        ))),
                        Err(e) => ev.log(LogLine::warn(format!(
                            "[alb]   GetObject {} ({label}) failed: {e:#}",
                            hex(candidate)
                        ))),
                    }
                    match session.get_thumb_data(candidate, fmt::JPEG) {
                        Ok(data) if !data.is_empty() => ev.log(LogLine::ok(format!(
                            "[alb]   GetThumb  {} -> {} bytes, magic {}",
                            hex(candidate),
                            data.len(),
                            magic(&data)
                        ))),
                        Ok(_) => ev.log(LogLine::warn(format!(
                            "[alb]   GetThumb  {} -> empty payload",
                            hex(candidate)
                        ))),
                        Err(e) => ev.log(LogLine::warn(format!(
                            "[alb]   GetThumb  {} failed: {e:#}",
                            hex(candidate)
                        ))),
                    }
                }
                None => ev.log(LogLine::warn(
                    "[alb]   every reference is audio — no cover object is referenced by this album",
                )),
            }
        }

        // The folder the device keeps cover art in, if any.
        match session.children(fmt::ASSOCIATION, ROOT) {
            Ok(folders) => {
                for folder in folders {
                    let Ok(info) = session.object_info(folder) else { continue };
                    if !info.filename.eq_ignore_ascii_case("CoverArt") {
                        continue;
                    }
                    match session.children(fmt::JPEG, folder) {
                        Ok(children) => {
                            ev.log(LogLine::info(format!(
                                "[alb] CoverArt folder {}: {} JPEG child(ren)",
                                hex(folder),
                                children.len()
                            )));
                            for child in children.iter().take(5) {
                                match session.object_info(*child) {
                                    Ok(ci) => ev.log(LogLine::info(format!(
                                        "[alb]   {} name=\"{}\" size={}",
                                        hex(*child),
                                        ci.filename,
                                        ci.size
                                    ))),
                                    Err(e) => ev.log(LogLine::warn(format!(
                                        "[alb]   {} info failed: {e:#}",
                                        hex(*child)
                                    ))),
                                }
                            }
                            if ui_children_left(children.len()) > 0 {
                                ev.log(LogLine::info(format!(
                                    "[alb]   … and {} more",
                                    ui_children_left(children.len())
                                )));
                            }
                        }
                        Err(e) => ev.log(LogLine::warn(format!(
                            "[alb] CoverArt folder children failed: {e:#}"
                        ))),
                    }
                    // Cover-art objects may not be JPEG format children.
                    match session.children(fmt::COVER_ART, folder) {
                        Ok(children) => ev.log(LogLine::info(format!(
                            "[alb] CoverArt folder {}: {} object(s) of format 0x3802",
                            hex(folder),
                            children.len()
                        ))),
                        Err(e) => ev.log(LogLine::info(format!(
                            "[alb] CoverArt folder 0x3802 children unavailable: {e:#}"
                        ))),
                    }
                }
            }
            Err(e) => ev.log(LogLine::warn(format!("[alb] listing root folders failed: {e:#}"))),
        }
        // Whether the bulk property list works on this firmware decides whether a library
        // takes seconds or minutes. The refresh already found out — and finding out costs
        // a 30-second timeout on a Zune 30, so this reports the recorded verdict rather
        // than paying for it twice.
        if session_bulk_props_unavailable {
            ev.log(LogLine::warn(
                "[alb] bulk property read (0x9805, five params): the device refused it \
                 (recorded during the library read) — libraries are read one track at a time",
            ));
        } else {
            ev.log(LogLine::info(
                "[alb] bulk property read: no verdict recorded yet — press Refresh once and \
                 the log will say whether the device supports it",
            ));
        }

        ev.log(LogLine::info("[alb] ---- end of album dump ----"));
    }

    /// Start fetching the covers the device does not have, on a thread of its own.
    ///
    /// One cover takes about seventeen seconds — nearly all of it the download through
    /// the proxy — and the Albums page would otherwise fetch them one at a time while the
    /// user scrolls. Prefetching writes them into the on-disk cache, so by the time a tile
    /// asks, the answer is already local.
    /// A free function rather than a method: the library reader holds `&mut self.session`
    /// while it runs, and only the serial is actually needed.
    fn spawn_cover_prefetch(serial: &str, albums: &[Album], ev: &Emitter) {
        if covers_disabled() {
            return;
        }
        if PREFETCH_STARTED.swap(true, Ordering::Relaxed) {
            return;
        }
        let missing: Vec<(String, String)> = albums
            .iter()
            .filter(|album| !album.cover_source.is_found())
            .map(|album| (album.artist.clone(), album.name.clone()))
            .collect();
        if missing.is_empty() {
            return;
        }
        if !cache::is_usable_serial(serial) {
            log::debug!("cover prefetch skipped: no usable serial to key the cache by");
            return;
        }
        let serial = serial.to_owned();
        let count = missing.len();
        let done = ev.fork();
        log::debug!("prefetching covers for {count} album(s) in the background");
        std::thread::spawn(move || {
            coverfetch::prefetch(&serial, &missing, &coverfetch::Sources::default());
            done.send(Event::CoversPrefetched { albums: count });
        });
    }

    /// The device has no art for this album: local files first, then iTunes, and cache
    /// whatever comes back so the next run is instant.
    /// Sweep the library for art that lives inside the audio files themselves.
    ///
    /// Device-first by design. The online ladder costs about three seconds per album in
    /// throttling and is wrong about a third of this library; two partial reads per album
    /// is about 5 MB for the whole thing and answers in seconds, from the device's own
    /// data. Whatever this finds is cached, so the wall is largely complete before a
    /// single request leaves the machine, and the ladder only sees what is left.
    fn harvest_embedded_art(
        session: &mut Session,
        serial: &str,
        albums: &[Album],
        ev: &Emitter,
    ) {
        if !cache::is_usable_serial(serial) {
            return;
        }
        let mut found = 0usize;
        let mut tried = 0usize;
        for album in albums {
            // A cover the cache already answers for needs nothing from the device.
            if cache::read_cover(serial, album.cover_key).is_some() {
                continue;
            }
            let Some(handle) = album.track_handles.first().copied() else {
                continue;
            };
            tried += 1;
            match Self::embedded_cover(session, serial, handle, album.cover_key, &album.name, ev) {
                Ok(true) => found += 1,
                Ok(false) => {}
                Err(e) => log::debug!("embedded cover {handle:#010x}: {e:#}"),
            }
        }
        if tried > 0 {
            let line = format!(
                "embedded cover art: {found} of {tried} album(s) carry their art in the audio file"
            );
            log::info!("{line}");
            ev.log(LogLine::info(line));
        }
    }

    /// Pull an album's art out of the audio object itself.
    ///
    /// This is where cover art really is on a Zune 30: the device exposes no MTP read
    /// for its library thumbnails (`MtpSvc.dll` imports `SetItemThumbnail` and
    /// `IsItemArtPresent` but never `GetItemThumbnail`), while a good share of the MP3s
    /// carry a full JPEG in an ID3v2 `APIC` frame. Two partial reads: the first 64 KB to
    /// locate the frame, then exactly the picture's own bytes — because the picture is
    /// ~272 KB and the file is ~8 MB.
    ///
    /// `Ok(true)` means a cover was found, decoded, cached and sent.
    fn embedded_cover(
        session: &mut Session,
        serial: &str,
        handle: u32,
        key: u32,
        album: &str,
        ev: &Emitter,
    ) -> Result<bool> {
        const HEAD: u32 = 64 * 1024;
        // Probe first: most files carry no picture at all, and 64 KB decides that
        // without paying for the megabyte a real cover needs.
        let probe = session.get_partial_object(handle, 0, HEAD)?;
        if !id3::may_carry_a_picture(&probe) {
            return Ok(false);
        }
        // One read of the tag area covers every embedded cover measured on this
        // device (largest so far: 258 KB). Where the picture starts cannot be known
        // without parsing the frames, and the frames need the bytes.
        const PICTURE_AREA: u32 = 1024 * 1024;
        let head = session.get_partial_object(handle, 0, PICTURE_AREA)?;
        let mut pictures = id3::pictures(&head);
        // A front cover beats a scan; order decides which one is tried first.
        pictures.sort_by_key(|picture| if picture.kind == 3 { 0 } else { 1 });
        for picture in pictures {
            let Ok(image) = decode_cover(&picture.data) else {
                log::debug!(
                    "cover {key:#010x}: a {} byte embedded picture would not decode",
                    picture.data.len()
                );
                continue;
            };
            // Cached like any other cover, so the next run needs no device read at all.
            if let Err(e) = cache::write_cover(serial, key, &picture.data) {
                log::debug!("embedded cover {key:#010x} not cached: {e:#}");
            }
            ev.log(LogLine::ok(format!(
                "cover art for \"{album}\": embedded in the audio file ({} KB, {})",
                picture.data.len() / 1024,
                picture.mime
            )));
            ev.send(Event::Cover {
                handle: key,
                image,
                origin: "the audio file's ID3 APIC frame".to_owned(),
            });
            return Ok(true);
        }
        Ok(false)
    }

    /// Adopt a cover the user picked: verify it decodes, cache it, and tell the UI.
    ///
    /// This outranks the ladder by construction — the cache is consulted before
    /// anything else — and the album's recorded miss is dropped so a later Refresh
    /// consults the online sources again instead of trusting the old "not found".
    fn set_cover(&mut self, key: u32, artist: &str, album: &str, path: &Path, ev: &Emitter) {
        let serial = self.snapshot.serial.clone();
        if !cache::is_usable_serial(&serial) {
            ev.log(LogLine::error(
                "no device serial — the cover cache cannot be keyed",
            ));
            return;
        }
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| path.display().to_string());
        let bytes = match std::fs::read(path) {
            Ok(bytes) => bytes,
            Err(e) => {
                ev.log(LogLine::error(format!("could not read {name}: {e}")));
                return;
            }
        };
        let image = match decode_cover(&bytes) {
            Ok(image) => image,
            Err(e) => {
                ev.log(LogLine::error(format!(
                    "{name} is not an image the cover view can decode: {e:#}"
                )));
                return;
            }
        };
        if let Err(e) = cache::write_cover(&serial, key, &bytes) {
            ev.log(LogLine::error(format!("could not cache the cover: {e:#}")));
            return;
        }
        coverfetch::clear_miss(&serial, artist, album);
        ev.log(LogLine::ok(format!(
            "cover art for \"{album}\": {name} ({} KB, your file)",
            bytes.len() / 1024
        )));
        ev.send(Event::Cover {
            handle: key,
            image,
            origin: format!("your file · {name}"),
        });
    }

    // ── one-way sync (local → device) ────────────────────────────────────────

    /// Walk the source folders, then compare them with the library in hand.
    ///
    /// The comparison side is `self.tracks` — whatever the last read or the cache
    /// provided — so a plan can be produced with nothing plugged in. The result is
    /// sent as a whole; the UI keeps it and hands the paths back to `SyncRun`.
    fn plan_sync(&mut self, ev: &Emitter) -> Result<String> {
        let config = locallib::load_config();
        ev.busy(true);
        ev.task_started("Scanning local folders", config.sources.len());
        if config.sources.is_empty() {
            ev.send(Event::SyncPlan(Box::new(crate::sync::SyncPlan::default())));
            ev.busy(false);
            ev.task_finished(true, "no source folders yet");
            return Ok("no source folders yet".to_owned());
        }
        let previous = locallib::load_index();
        let started = Instant::now();
        let (index, stats) = locallib::scan(&config.sources, previous.as_ref())?;
        if let Err(e) = locallib::save_index(&index) {
            ev.log(LogLine::warn(format!(
                "could not write the local index — the next scan will re-read every file: {e:#}"
            )));
        }
        let device = crate::sync::device_index(&self.tracks);
        let plan = crate::sync::plan(&index, &device, &config.sources);
        ev.log(LogLine::info(format!(
            "local scan: {} files in {} folder(s), {} re-read, {} matched on the device ({:.1}s)",
            stats.files,
            config.sources.len(),
            stats.read,
            plan.present,
            started.elapsed().as_secs_f32()
        )));
        let summary = if plan.to_push.is_empty() {
            format!("{} local files · all of them are already on the device", plan.local_files)
        } else {
            format!(
                "{} to push · {} already there · {} local files",
                plan.to_push.len(),
                plan.present,
                plan.local_files
            )
        };
        ev.send(Event::SyncPlan(Box::new(plan)));
        ev.busy(false);
        ev.task_finished(true, summary.clone());
        Ok(summary)
    }

    fn set_sync_sources(&mut self, sources: Vec<PathBuf>, ev: &Emitter) {
        let config = locallib::SyncConfig { version: locallib::INDEX_VERSION, sources };
        if let Err(e) = locallib::save_config(&config) {
            ev.log(LogLine::error(format!(
                "could not save the sync folders: {e:#}"
            )));
            return;
        }
        if let Err(e) = self.plan_sync(ev) {
            ev.log(LogLine::error(format!("Could not scan the local folders: {e:#}")));
            ev.task_finished(false, "local scan failed");
        }
    }

    /// Push every planned file, reporting each one's state as it goes.
    fn run_sync(&mut self, paths: &[PathBuf], ev: &Emitter, cancel: &AtomicBool) -> Result<String> {
        let total = paths.len();
        let mut pushed = 0usize;
        let mut failed = 0usize;
        for (index, path) in paths.iter().enumerate() {
            if cancel.load(Ordering::Relaxed) {
                ev.log(LogLine::warn(format!(
                    "stopped after {index} of {total} files — the rest was not pushed"
                )));
                break;
            }
            let label = file_label(path);
            ev.send(Event::SyncItem(Box::new(SyncItemEvent {
                index,
                total,
                label: label.clone(),
                path: path.clone(),
                state: SyncState::Started,
                note: String::new(),
            })));
            match self.push_one(path, index, total, ev) {
                Ok(()) => {
                    pushed += 1;
                    let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
                    self.note_pushed(path, size);
                    ev.send(Event::SyncItem(Box::new(SyncItemEvent {
                        index,
                        total,
                        label,
                        path: path.clone(),
                        state: SyncState::Done,
                        note: String::new(),
                    })));
                }
                Err(e) => {
                    failed += 1;
                    let note = format!("{e:#}");
                    ev.log(LogLine::error(format!("{label}: {note}")));
                    ev.send(Event::SyncItem(Box::new(SyncItemEvent {
                        index,
                        total,
                        label,
                        path: path.clone(),
                        state: SyncState::Failed,
                        note,
                    })));
                    if usb::is_stuck(&e) {
                        ev.log(LogLine::warn("the USB link looks dead — stopping the sync"));
                        break;
                    }
                }
            }
        }
        let summary = format!("{pushed} pushed, {failed} failed");
        ev.send(Event::SyncFinished { pushed, failed, summary: summary.clone() });
        Ok(summary)
    }

    /// Remember a file this app just pushed, so the next plan knows it is on the
    /// device without paying for a full re-read. The firmware renames the object as it
    /// ingests it, so the entry carries the tags — the tag signature is what matches
    /// on the next pass.
    fn note_pushed(&mut self, path: &Path, size: u64) {
        let tags = tags::read(path);
        self.tracks.push(Track {
            handle: 0,
            filename: tags::stored_name(path),
            size: size.min(u32::MAX as u64) as u32,
            format: 0x3008,
            title: tags.title,
            artist: tags.artist.unwrap_or_default(),
            album: tags.album.unwrap_or_default(),
            track_no: tags.track,
            duration: None,
        });
    }

    fn fetch_fallback_cover(
        &mut self,
        key: u32,
        artist: &str,
        album: &str,
        track_handle: Option<u32>,
        ev: &Emitter,
    ) {
        if covers_disabled() {
            return;
        }
        let serial = self.snapshot.serial.clone();
        if serial.is_empty() {
            ev.send(Event::CoverFailed {
                handle: key,
                reason: "no device serial to key the cover cache by".to_owned(),
            });
            return;
        }

        // The cache first. This is the path the album wall actually uses, and it used to
        // go straight to the ladder — so an album whose cover was already on disk was
        // downloaded again, and with the cache holding most of the library that was most
        // of the network traffic.
        if let Some(data) = cache::read_cover(&serial, key) {
            match decode_cover(&data) {
                Ok(image) => {
                    log::debug!("cover {key:#010x}: {} bytes from the local cache", data.len());
                    ev.send(Event::Cover { handle: key, image, origin: "cache".to_owned() });
                    return;
                }
                Err(e) => log::debug!(
                    "cached cover {key:#010x} would not decode ({e:#}) — asking the ladder"
                ),
            }
        }

        // Before anything online: the art may simply be inside the album's audio file.
        if let Some(handle) = track_handle {
            if let Some(session) = self.session.as_mut() {
                match Self::embedded_cover(session, &serial, handle, key, album, ev) {
                    Ok(true) => return,
                    Ok(false) => {}
                    Err(e) => log::debug!("embedded cover {handle:#010x}: {e:#}"),
                }
            }
        }
        // A miss is remembered: asking iTunes again on every repaint would be rude, and
        // asking on every refresh is pointless.
        if coverfetch::recently_missed(&serial, artist, album) {
            ev.send(Event::CoverFailed {
                handle: key,
                reason: "no cover found recently — not retrying yet (Refresh re-enables it)"
                    .to_owned(),
            });
            return;
        }
        let sources = coverfetch::Sources::default();
        match coverfetch::resolve(&serial, artist, album, &sources) {
            Ok((bytes, origin)) => match decode_cover(&bytes) {
                Ok(image) => {
                    ev.log(LogLine::ok(format!(
                        "cover art for \"{album}\": {}",
                        origin.label()
                    )));
                    ev.send(Event::Cover { handle: key, image, origin: origin.label() });
                }
                Err(e) => {
                    coverfetch::note_miss(&serial, artist, album);
                    let reason = format!("the fetched cover would not decode: {e:#}");
                    ev.log(LogLine::warn(format!("[alb] {album}: {reason}")));
                    ev.send(Event::CoverFailed { handle: key, reason });
                }
            },
            Err(e) => {
                // Only a definite "there is no cover anywhere" is worth remembering. A
                // timeout, a 503 or a refused API call will probably be different next run,
                // and a week-long miss would hide that.
                if coverfetch::is_not_found(&e) {
                    coverfetch::note_miss(&serial, artist, album);
                }
                let reason = format!("{e:#}");
                ev.log(LogLine::info(format!("[alb] {album}: {reason}")));
                ev.send(Event::CoverFailed { handle: key, reason });
            }
        }
    }

    // ── wifi / usb ───────────────────────────────────────────────────────────

    fn wifi_scan(&mut self, ev: &Emitter) {
        let Some(session) = self.session.as_mut() else {
            ev.log(LogLine::error("WiFi scan needs a connected device"));
            return;
        };
        match session.wifi_scan() {
            Ok(bytes) => {
                let shown: Vec<u8> = bytes.iter().copied().take(4096).collect();
                let hex = hex_spaced(&shown);
                let networks = crate::core::wifi::parse_scan(&bytes);
                ev.log(LogLine::ok(format!(
                    "WiFi scan returned {} bytes — {} network(s){}",
                    bytes.len(),
                    networks.len(),
                    if bytes.len() > shown.len() { " (hex truncated to 4 KB)" } else { "" }
                )));
                for network in &networks {
                    ev.log(LogLine::info(format!(
                        "  {:>3}%  {:<10}  {}",
                        network.strength, network.quality(), network.ssid
                    )));
                }
                ev.send(Event::WifiScan { networks, hex });
            }
            Err(e) => ev.log(LogLine::error(format!("WiFi scan failed: {e:#}"))),
        }
    }

    fn wifi_simple(&mut self, opcode: u16, what: &str, ev: &Emitter) {
        let Some(session) = self.session.as_mut() else {
            ev.log(LogLine::error(format!("{what} needs a connected device")));
            return;
        };
        let result = match opcode {
            mtp::OP_WIFI_TEST_CONNECTIVITY => session.wifi_test_connectivity(),
            _ => Err(anyhow::anyhow!("unsupported WiFi opcode {opcode:#06x}")),
        };
        match result {
            Ok(()) => ev.log(LogLine::ok(format!("{what}: the device accepted the request"))),
            Err(e) => ev.log(LogLine::error(format!("{what} failed: {e:#}"))),
        }
    }

    fn wifi_profile(&mut self, blob: &[u8], ev: &Emitter) {
        let Some(session) = self.session.as_mut() else {
            ev.log(LogLine::error("Setting a WiFi profile needs a connected device"));
            return;
        };
        match session.wifi_set_profile(blob) {
            Ok(()) => ev.log(LogLine::ok(format!("WiFi profile committed ({} bytes)", blob.len()))),
            Err(e) => ev.log(LogLine::error(format!("WiFi profile failed: {e:#}"))),
        }
    }

    fn usb_reset(&mut self, ev: &Emitter) {
        ev.log(LogLine::warn("Resetting the USB link (this re-enumerates the device)…"));
        self.drop_session();
        ev.link(Link::Disconnected);
        let result = usb::find_device().and_then(|info| usb::reset(&info));
        match result {
            Ok(()) => {
                ev.log(LogLine::ok("USB reset ok — reconnecting"));
                self.reset_before_connect = false;
                self.last_error = None;
                self.try_connect(ev);
            }
            Err(e) => ev.log(LogLine::error(format!("USB reset failed: {e:#}"))),
        }
    }
}

// ── helpers ──────────────────────────────────────────────────────────────────

/// Open the transport and clear whatever an interrupted session left buffered:
/// the Zune refuses new commands while an unread response is pending.
/// A synthetic library for UI review and screenshots (`ZUNE_DEMO=1`).
///
/// Covers go through the *real* decode path (a JPEG is encoded here and decoded by
/// [`decode_cover`]) and two albums are deliberately left without art, so the tile's
/// placeholder and its failure marker are both exercised.
fn demo_library(ev: &Emitter) {
    #[allow(clippy::type_complexity)]
    let spec: [(&str, &str, u32, usize, bool); 13] = [
        ("Kid A", "Radiohead", 2000, 10, true),
        ("Amnesiac", "Radiohead", 2001, 11, true),
        ("OK Computer", "Radiohead", 1997, 12, true),
        ("In Rainbows", "Radiohead", 2007, 10, true),
        ("Voodoo", "D'Angelo", 2000, 13, true),
        ("The Miseducation of Lauryn Hill", "Lauryn Hill", 1998, 16, true),
        ("Mezzanine", "Massive Attack", 1998, 11, true),
        ("Dummy", "Portishead", 1994, 11, false),
        ("Homogenic", "Björk", 1997, 10, true),
        ("Untrue", "Burial", 2007, 13, false),
        ("Selected Ambient Works 85-92", "Aphex Twin", 1992, 13, true),
        ("Blue Lines", "Massive Attack", 1991, 9, true),
        ("蓝莓之夜", "王家卫", 2007, 8, true),
    ];

    let mut tracks: Vec<Track> = Vec::new();
    let mut albums: Vec<Album> = Vec::new();
    let mut handle = 0x0010_0000u32;

    for (index, (name, artist, year, count, has_art)) in spec.iter().enumerate() {
        let album_handle = {
            handle += 1;
            handle
        };
        let mut track_handles = Vec::new();
        let mut bytes = 0u64;
        let mut duration = 0u32;
        for number in 1..=*count {
            handle += 1;
            // Deliberately generated the way the device reports it — milliseconds —
            // and converted by the same code the real library read uses.
            let raw_ms = (150 + (number as u32 * 37 + index as u32 * 11) % 260) * 1000 + 417;
            let size = 3_000_000 + (number as u32 * 137_000 + index as u32 * 251_000) % 4_000_000;
            duration += duration_seconds(Some(raw_ms)).unwrap_or(0);
            bytes += size as u64;
            track_handles.push(handle);
            tracks.push(Track {
                handle,
                filename: format!("{number:02}.mp3"),
                size,
                format: fmt::MP3,
                title: format!("{name} — movement {number}"),
                artist: artist.to_string(),
                album: name.to_string(),
                track_no: Some(number as u16),
                duration: duration_seconds(Some(raw_ms)),
            });
        }
        let cover_handle = has_art.then(|| {
            handle += 1;
            handle
        });
        albums.push(Album {
            name: name.to_string(),
            handle: Some(album_handle),
            artist: artist.to_string(),
            year: Some(*year),
            track_handles,
            tracks: *count,
            bytes,
            duration,
            cover_handle,
            cover_key: cover_handle
                .unwrap_or_else(|| coverfetch::synthetic_key(artist, name)),
            cover_source: if *has_art { CoverSource::RefCoverArt } else { CoverSource::None },
        });
    }

    albums.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    let total_bytes: u64 = tracks.iter().map(|t| t.size as u64).sum();

    ev.send(Event::Library { tracks, albums: albums.clone() });
    let mut snapshot = DeviceSnapshot {
        name: "Zune 30 (demo)".to_owned(),
        manufacturer: "Microsoft".to_owned(),
        serial: "demo-serial".to_owned(),
        firmware: "1.4".to_owned(),
        mtp_version: "1.0".to_owned(),
        topology: "synthetic".to_owned(),
        storage_desc: "demo volume".to_owned(),
        capacity: Some(31_300_000_000),
        free: Some(12_400_000_000),
        tracks: tracks_len(&albums),
        albums: albums.len(),
        playlists: 4,
        library_bytes: total_bytes,
        key_path: Some("~/.mtpz-data".to_owned()),
        ..Default::default()
    };
    snapshot.tracks = snapshot.tracks.max(1);
    ev.send(Event::Snapshot(Box::new(snapshot)));
    ev.send(Event::Origin(Box::new(DataOrigin {
        source: Source::Live,
        read_at: Some(cache::now_unix()),
        device_connected: true,
        serial: "demo-serial".to_owned(),
        note: Some("synthetic library".to_owned()),
    })));
    ev.progress(Progress {
        task: "Reading the library".to_owned(),
        step: "demo library".to_owned(),
        ..Default::default()
    });

    // Optional: simulate the real-world failure so the failure marker and its message
    // can be reviewed without a device in a broken state.
    let simulate_failure = std::env::var_os("ZUNE_DEMO_FAIL_COVER").is_some();

    // Covers, through the real decoder.
    for (index, album) in albums.iter().enumerate() {
        let Some(cover) = album.cover_handle else { continue };
        if simulate_failure && index == 0 {
            // The exact shape of the failure a Zune produced: a track-sized WMA object
            // that had been mistaken for cover art.
            let reason = "the object is an ASF/WMA (or WMV) media file (2240665 bytes), not an image — it is not cover art";
            ev.send(Event::CoverFailed { handle: cover, reason: reason.to_owned() });
            continue;
        }
        let jpeg = demo_cover_jpeg(index);
        match decode_cover(&jpeg) {
            Ok(image) => ev.send(Event::Cover { handle: cover, image, origin: "demo".to_owned() }),
            Err(e) => {
                let reason = format!("demo cover {index} failed to decode: {e:#}");
                ev.log(LogLine::warn(format!("[alb] {reason}")));
                ev.send(Event::CoverFailed { handle: cover, reason });
            }
        }
    }
}

fn tracks_len(albums: &[Album]) -> usize {
    albums.iter().map(|a| a.tracks).sum()
}

/// A 240×240 JPEG with a neon-ish gradient and a big index digit, so the grid shows
/// something recognisable and the decode path stays honest.
fn demo_cover_jpeg(index: usize) -> Vec<u8> {
    let hue = (index as f32 * 47.0) % 360.0;
    let (r, g, b) = hsv(hue, 0.55, 0.75);
    let image = image::RgbImage::from_fn(240, 240, |x, y| {
        let t = (x + y) as f32 / 480.0;
        let shade = 0.45 + 0.55 * t;
        image::Rgb([
            (r as f32 * shade) as u8,
            (g as f32 * shade) as u8,
            (b as f32 * shade) as u8,
        ])
    });
    let mut out = Vec::new();
    let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 82);
    let _ = image.write_with_encoder(encoder);
    out
}

fn hsv(h: f32, s: f32, v: f32) -> (u8, u8, u8) {
    let c = v * s;
    let x = c * (1.0 - ((h / 60.0) % 2.0 - 1.0).abs());
    let m = v - c;
    let (r, g, b) = match h as u32 / 60 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    (
        ((r + m) * 255.0) as u8,
        ((g + m) * 255.0) as u8,
        ((b + m) * 255.0) as u8,
    )
}

fn open_and_clear(info: &DeviceInfo, ev: &Emitter) -> Result<usb::Transport> {
    let mut transport = usb::Transport::open(info)?;
    match transport.drain_pending_input(Duration::from_millis(350)) {
        Ok(d) if !d.is_empty() => {
            ev.log(LogLine::info(format!(
                "cleared {} bytes left over from an earlier session",
                d.len()
            )));
        }
        Ok(_) => {}
        Err(e) => log::debug!("drain skipped: {e:#}"),
    }
    Ok(transport)
}

/// Build the album list.
///
/// Grouping itself needs no device access: the abstract albums give names and
/// references, and any track whose album tag has no abstract album gets its own
/// group. Cover art, however, is a *device* question — see [`resolve_cover`].
fn build_albums(tracks: &[Track], abstract_albums: &[AlbumEntry]) -> Vec<Album> {
    let by_handle: HashMap<u32, &Track> = tracks.iter().map(|t| (t.handle, t)).collect();
    let mut albums: Vec<Album> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();

    for entry in abstract_albums {
        let track_handles: Vec<u32> = entry
            .refs
            .iter()
            .copied()
            .filter(|h| by_handle.contains_key(h))
            .collect();
        if track_handles.is_empty() {
            continue;
        }
        let stats = album_stats(&track_handles, &by_handle);
        index.insert(entry.name.to_lowercase(), albums.len());
        albums.push(Album {
            name: entry.name.clone(),
            handle: Some(entry.handle),
            artist: entry.album_artist.clone().unwrap_or_else(|| stats.artist.clone()),
            year: entry.year,
            tracks: track_handles.len(),
            bytes: stats.bytes,
            duration: stats.duration,
            track_handles,
            cover_handle: None,
            cover_key: 0,
            cover_source: CoverSource::None,
        });
    }

    // Tracks the abstract albums do not cover (pushed plain, or grouped by tags).
    for track in tracks {
        let name = if track.album.trim().is_empty() {
            "Unknown album".to_owned()
        } else {
            track.album.trim().to_owned()
        };
        let key = name.to_lowercase();
        match index.get(&key) {
            Some(&i) => {
                if !albums[i].track_handles.contains(&track.handle) {
                    albums[i].track_handles.push(track.handle);
                }
            }
            None => {
                index.insert(key, albums.len());
                albums.push(Album {
                    name,
                    handle: None,
                    artist: track.artist.clone(),
                    year: None,
                    tracks: 1,
                    bytes: track.size as u64,
                    duration: track.duration.unwrap_or(0),
                    track_handles: vec![track.handle],
                    cover_handle: None,
                    cover_key: 0,
                    cover_source: CoverSource::None,
                });
            }
        }
    }

    // Recompute the per-album statistics now that every group is complete.
    for album in &mut albums {
        let stats = album_stats(&album.track_handles, &by_handle);
        album.tracks = album.track_handles.len();
        album.bytes = stats.bytes;
        album.duration = stats.duration;
        if album.artist.is_empty() {
            album.artist = stats.artist;
        }
    }

    albums.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    albums
}

/// One abstract album as read from the device, with the properties the tile shows.
#[derive(Debug, Clone)]
pub struct AlbumEntry {
    pub name: String,
    pub handle: u32,
    pub refs: Vec<u32>,
    pub album_artist: Option<String>,
    pub year: Option<u32>,
}

struct AlbumStats {
    bytes: u64,
    duration: u32,
    artist: String,
}

/// Sum size/duration and pick the dominant artist among the member tracks.
fn album_stats(handles: &[u32], by_handle: &HashMap<u32, &Track>) -> AlbumStats {
    let mut bytes = 0u64;
    let mut duration = 0u32;
    let mut artists: HashMap<&str, usize> = HashMap::new();
    for handle in handles {
        let Some(track) = by_handle.get(handle) else { continue };
        bytes += track.size as u64;
        duration += track.duration.unwrap_or(0);
        if !track.artist.trim().is_empty() {
            *artists.entry(track.artist.as_str()).or_default() += 1;
        }
    }
    let artist = artists
        .into_iter()
        .max_by_key(|(_, count)| *count)
        .map(|(name, _)| name.to_owned())
        .unwrap_or_default();
    AlbumStats { bytes, duration, artist }
}

const ALBUM_DIAGNOSE_LIMIT: usize = 6;

// ── album dates ─────────────────────────────────────────────────────────────

/// Object properties that may carry an album's date, in the order we try them.
/// Base-MTP codes first; the vendor-ish one last.
const DATE_CANDIDATES: [u16; 3] = [
    mtp::prop::DATE_CREATED,
    mtp::prop::DATE_MODIFIED,
    mtp::prop::DATE_ADDED,
];

/// Read an album's year, remembering which property code works on this firmware so
/// the other 88 albums cost one call instead of three.
fn read_album_year(session: &mut Session, handle: u32, known: &mut Option<u16>) -> Option<u32> {
    let attempt = |session: &mut Session, code: u16| -> Option<u32> {
        // MTP date-time is a string ("20101018T000000"); some firmware stores an
        // epoch instead, so both shapes are accepted.
        if let Ok(text) = session.get_prop_str(handle, code) {
            if let Some(year) = parse_year_text(&text) {
                return Some(year);
            }
        }
        if let Ok(value) = session.get_prop_u32(handle, code) {
            if let Some(year) = year_from_unix(value) {
                return Some(year);
            }
        }
        None
    };

    if let Some(code) = *known {
        return attempt(session, code);
    }
    for code in DATE_CANDIDATES {
        if let Some(year) = attempt(session, code) {
            *known = Some(code);
            return Some(year);
        }
    }
    None
}

/// `"20101018T030405"` → 2010; a bare `"2010"` works too. Anything implausible is
/// rejected rather than shown as a bogus year.
fn parse_year_text(text: &str) -> Option<u32> {
    let digits: String = text.trim().chars().take_while(char::is_ascii_digit).collect();
    if digits.len() < 4 {
        return None;
    }
    let year: u32 = digits[..4].parse().ok()?;
    (1970..=2100).contains(&year).then_some(year)
}

/// Seconds since the epoch → year, without dragging in a date crate.
fn year_from_unix(seconds: u32) -> Option<u32> {
    if seconds < 31_536_000 || seconds > 4_102_444_800 {
        return None; // outside 1971..2100: not a date we would show
    }
    Some(civil_from_days(seconds as i64 / 86_400).0 as u32)
}

/// Howard Hinnant's `civil_from_days`: days since 1970-01-01 → (year, month, day).
fn civil_from_days(days: i64) -> (i32, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { y + 1 } else { y };
    (year as i32, month as u32, day as u32)
}

// ── cover art resolution ────────────────────────────────────────────────────

/// What a pass over the album list discovered about cover art.
#[derive(Default)]
struct CoverReport {
    found: usize,
    from_refs: usize,
    from_folder: usize,
    from_reverse: usize,
    /// Albums still without art, with the reason, so the Log can say why.
    unresolved: Vec<(String, CoverSource, String)>,
}

impl CoverReport {
    fn sources_summary(&self) -> String {
        let mut parts = Vec::new();
        if self.from_refs > 0 {
            parts.push(format!("{} from album references", self.from_refs));
        }
        if self.from_reverse > 0 {
            parts.push(format!("{} via an image that references the album", self.from_reverse));
        }
        if self.from_folder > 0 {
            parts.push(format!("{} matched by image name", self.from_folder));
        }
        if parts.is_empty() {
            "no cover objects".to_owned()
        } else {
            parts.join(", ")
        }
    }
}

/// Find each album's cover art.
///
/// **Nothing about the reference layout is assumed.** An album's references are
/// classified by asking the device for each object's real format; a reference that is
/// not audio is a candidate, and a candidate whose format is CoverArt (0x3802) wins
/// over one that is merely JPEG. Only if that fails — which is the common case for
/// albums written by Microsoft's software — is the device's `CoverArt` folder indexed
/// and matched by name. Every outcome is recorded, including the failures.
fn resolve_covers(
    lib: &mut Library,
    albums: &mut [Album],
    entries: &[AlbumEntry],
    ev: &Emitter,
    cover_index: &mut Option<Vec<(u32, String, u16)>>,
) -> CoverReport {
    let mut report = CoverReport::default();
    if albums.is_empty() {
        return report;
    }

    // Every handle that is a track, so references can be split without guessing.
    let audio: std::collections::HashSet<u32> = albums
        .iter()
        .flat_map(|a| a.track_handles.iter().copied())
        .collect();

    let refs_for = |handle: Option<u32>| -> Vec<u32> {
        handle
            .and_then(|h| entries.iter().find(|e| e.handle == h))
            .map(|e| e.refs.clone())
            .unwrap_or_default()
    };

    let mut needs_folder = false;
    for index in 0..albums.len() {
        let refs = refs_for(albums[index].handle);
        let mut best: Option<(u32, CoverSource)> = None;
        // Fast filter on the handles we already know are audio, then a *format check*
        // on the rest. The fast filter alone is not enough: a track whose handle is
        // missing from the list (a read failure, a format the enumeration skipped)
        // would otherwise be handed to the image decoder, which is exactly how a
        // 2.2 MB WMA object came to be reported as "cover art could not be decoded".
        for reference in refs.iter().filter(|h| !audio.contains(h)) {
            let Ok(info) = lib.session.object_info(*reference) else { continue };
            let Some(source) = cover_source_for_format(info.format) else { continue };
            let rank = |s: CoverSource| match s {
                CoverSource::RefCoverArt => 2,
                CoverSource::RefJpeg => 1,
                _ => 0,
            };
            if best.map(|(_, s)| rank(source) > rank(s)).unwrap_or(true) {
                best = Some((*reference, source));
            }
        }
        match best {
            Some((handle, source)) => {
                albums[index].cover_handle = Some(handle);
                albums[index].cover_source = source;
                report.found += 1;
                report.from_refs += 1;
            }
            None => needs_folder = true,
        }
    }

    // The link may be stored on the *image* side: a Zune 30 has no CoverArt folder, and
    // (as the on-device diagnosis showed) an album's own references can be empty while
    // 97 cover JPEGs sit in the 0x03 handle segment. Reading each image's references is
    // one call per image — bounded, and it is what settles the question either way.
    let still_missing: Vec<usize> = albums
        .iter()
        .enumerate()
        .filter(|(_, a)| !a.cover_source.is_found())
        .map(|(i, _)| i)
        .collect();
    if !still_missing.is_empty() {
        ev.progress(Progress {
            task: "Reading the library".to_owned(),
            step: "looking for cover art on the image objects".to_owned(),
            ..Default::default()
        });
        if cover_index.is_none() {
            *cover_index = Some(index_cover_art(lib));
        }
        let index = cover_index.as_ref().cloned().unwrap_or_default();
        let album_handles: std::collections::HashSet<u32> =
            albums.iter().filter_map(|a| a.handle).collect();
        let mut reverse: HashMap<u32, u32> = HashMap::new();
        let mut scanned = 0usize;
        for (handle, _, _) in index.iter().take(200) {
            scanned += 1;
            let refs = lib.session.object_refs(*handle).unwrap_or_default();
            for reference in refs {
                if album_handles.contains(&reference) {
                    reverse.entry(reference).or_insert(*handle);
                }
            }
        }
        if !reverse.is_empty() {
            ev.log(LogLine::info(format!(
                "cover art: {} image object(s) scan of {scanned} reference an album — using the reverse link",
                reverse.len()
            )));
        } else if scanned > 0 {
            // Worth a user-visible line once: it rules the reverse hypothesis out.
            ev.log(LogLine::info(format!(
                "cover art: {scanned} image object(s) scanned, none references an album"
            )));
        }
        for index in still_missing {
            let Some(album_handle) = albums[index].handle else { continue };
            if let Some(image) = reverse.get(&album_handle) {
                albums[index].cover_handle = Some(*image);
                albums[index].cover_source = CoverSource::ReverseRef;
                report.found += 1;
                report.from_reverse += 1;
            }
        }
    }

    // The last fallback: match the album name against the image index.
    if needs_folder {
        if cover_index.is_none() {
            ev.progress(Progress {
                task: "Reading the library".to_owned(),
                step: "indexing the device's CoverArt folder".to_owned(),
                ..Default::default()
            });
            *cover_index = Some(index_cover_art(lib));
            log::info!(
                "CoverArt folder index: {} object(s)",
                cover_index.as_ref().map(|i| i.len()).unwrap_or(0)
            );
        }
        let index = cover_index.as_ref().cloned().unwrap_or_default();
        for album in albums.iter_mut() {
            if album.cover_source.is_found() {
                continue;
            }
            match match_cover(&index, &album.name) {
                Some((handle, name)) => {
                    album.cover_handle = Some(handle);
                    album.cover_source = CoverSource::FolderMatch;
                    report.found += 1;
                    report.from_folder += 1;
                    log::debug!("\"{}\" -> cover {} (\"{name}\")", album.name, hex(handle));
                }
                None => report.unresolved.push((
                    album.name.clone(),
                    CoverSource::None,
                    if index.is_empty() {
                        " — the device exposes no CoverArt folder children".to_owned()
                    } else {
                        format!(" — {} folder object(s), none named like the album", index.len())
                    },
                )),
            }
        }
    }

    report
}

/// Which object formats may be treated as cover art.
///
/// Nothing else qualifies — not "a reference that is not a track", not a playlist, and
/// certainly not a WMA file. Cover art on the Zune is either the device's own CoverArt
/// format or a plain JPEG.
fn cover_source_for_format(format: u16) -> Option<CoverSource> {
    match format {
        fmt::COVER_ART => Some(CoverSource::RefCoverArt),
        fmt::JPEG => Some(CoverSource::RefJpeg),
        _ => None,
    }
}

/// Every object in the device's `CoverArt` folders, as (handle, name, format).
fn index_cover_art(lib: &mut Library) -> Vec<(u32, String, u16)> {
    /// Cap, so a pathological device cannot turn this into a minute of I/O.
    const MAX_ENTRIES: usize = 600;
    let mut out: Vec<(u32, String, u16)> = Vec::new();
    let storage = lib.storage;

    // Gather the handles first, then resolve each one: a closure cannot borrow the
    // session while the session is also being queried for more handles.
    let mut handles: Vec<u32> = Vec::new();
    let mut push = |candidates: Vec<u32>, handles: &mut Vec<u32>| {
        for handle in candidates {
            if !handles.contains(&handle) {
                handles.push(handle);
            }
        }
    };

    // **The direct listing is the one that works.** A Zune 30 keeps its 95 cover JPEGs as
    // children of the device root and answers GeneralError to the wildcard-parent query,
    // so that query is a bonus, never the primary source.
    for format in [fmt::JPEG, fmt::COVER_ART] {
        if let Ok(children) = lib.session.children(format, ROOT) {
            push(children, &mut handles);
        }
    }
    // Folders second (some firmware keeps them there).
    if let Ok(folders) = lib.session.children(fmt::ASSOCIATION, ROOT) {
        for folder in folders {
            let name = lib
                .session
                .object_info(folder)
                .map(|i| i.filename)
                .unwrap_or_default();
            for format in [fmt::COVER_ART, fmt::JPEG] {
                let Ok(children) = lib.session.children(format, folder) else { continue };
                if !children.is_empty() {
                    log::debug!("folder \"{name}\" holds {} image object(s)", children.len());
                }
                push(children, &mut handles);
            }
        }
    }
    // The wildcard query last: a bonus on firmware that supports it.
    for format in [fmt::JPEG, fmt::COVER_ART] {
        if let Ok(found) = lib.session.object_handles(storage, format, u32::MAX) {
            push(found, &mut handles);
        }
    }

    for handle in handles.into_iter().take(MAX_ENTRIES) {
        match lib.session.object_info(handle) {
            Ok(info) => out.push((handle, info.filename, info.format)),
            Err(e) => log::debug!("object_info {handle:#010x} failed: {e:#}"),
        }
    }
    out
}

/// Match an album name against the folder index. Exact after normalisation first,
/// then a conservative containment test (at least 4 characters).
fn match_cover(index: &[(u32, String, u16)], album: &str) -> Option<(u32, String)> {
    let want = normalise(album);
    if want.is_empty() {
        return None;
    }
    let mut loose = None;
    for (handle, name, _) in index {
        let have = normalise(name);
        if have.is_empty() {
            continue;
        }
        if have == want {
            return Some((*handle, name.clone()));
        }
        if loose.is_none() && have.len() >= 4 && (want.contains(&have) || have.contains(&want)) {
            loose = Some((*handle, name.clone()));
        }
    }
    loose
}

/// Lowercase, drop punctuation/extension: `"Kid A.jpg"` and `"Kid A"` both → `kida`.
fn normalise(text: &str) -> String {
    let stem = match text.rsplit_once('.') {
        Some((head, ext)) if ext.len() <= 4 => head,
        _ => text,
    };
    stem.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(|c| c.to_lowercase())
        .collect()
}


/// Decode a cover-art payload (JPEG) into RGBA for a texture.
fn decode_cover(data: &[u8]) -> Result<CoverImage> {
    let img = image::load_from_memory(data).context("decoding the cover art")?;
    let img = if img.width() > COVER_MAX || img.height() > COVER_MAX {
        img.resize(COVER_MAX, COVER_MAX, image::imageops::FilterType::Triangle)
    } else {
        img
    };
    let rgba = img.to_rgba8();
    Ok(CoverImage {
        width: rgba.width() as usize,
        height: rgba.height() as usize,
        rgba: rgba.into_raw(),
    })
}

/// Collect the audio files a push selection refers to.
fn expand_audio(paths: &[PathBuf]) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    for path in paths {
        if path.is_dir() {
            out.extend(library::audio_files(path)?);
        } else if path.is_file() {
            let ext = path
                .extension()
                .map(|e| e.to_string_lossy().to_lowercase())
                .unwrap_or_default();
            if library::AUDIO_EXTS.contains(&ext.as_str()) {
                out.push(path.clone());
            } else {
                bail!("{} is not an audio file the Zune can play", path.display());
            }
        } else {
            bail!("{} does not exist", path.display());
        }
    }
    out.sort();
    out.dedup();
    Ok(out)
}

/// What a payload actually is, so a failure names the real format.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum PayloadKind {
    Jpeg,
    Png,
    Bmp,
    Gif,
    /// ASF container — WMA/WMV, i.e. what a Zune track looks like on the wire.
    Asf,
    Mp3,
    Riff,
    Unknown,
}

impl PayloadKind {
    fn is_image(self) -> bool {
        matches!(self, PayloadKind::Jpeg | PayloadKind::Png | PayloadKind::Bmp | PayloadKind::Gif)
    }

    fn name(self) -> &'static str {
        match self {
            PayloadKind::Jpeg => "a JPEG",
            PayloadKind::Png => "a PNG",
            PayloadKind::Bmp => "a BMP",
            PayloadKind::Gif => "a GIF",
            PayloadKind::Asf => "an ASF/WMA (or WMV) media file",
            PayloadKind::Mp3 => "an MP3 file",
            PayloadKind::Riff => "a RIFF (WAV) file",
            PayloadKind::Unknown => "an unrecognised payload",
        }
    }
}

/// Identify a payload from its leading bytes.
fn describe_payload(data: &[u8]) -> Option<PayloadKind> {
    let kind = if data.starts_with(&[0xFF, 0xD8, 0xFF]) {
        PayloadKind::Jpeg
    } else if data.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
        PayloadKind::Png
    } else if data.starts_with(b"BM") {
        PayloadKind::Bmp
    } else if data.starts_with(b"GIF8") {
        PayloadKind::Gif
    } else if data.starts_with(&[0x30, 0x26, 0xB2, 0x75, 0x8E, 0x66, 0xCF, 0x11]) {
        PayloadKind::Asf
    } else if data.starts_with(b"RIFF") {
        PayloadKind::Riff
    } else if data.starts_with(b"ID3") || (data.len() > 1 && data[0] == 0xFF && data[1] & 0xE0 == 0xE0) {
        PayloadKind::Mp3
    } else {
        PayloadKind::Unknown
    };
    Some(kind)
}

/// Convert the device's Duration property to seconds.
///
/// MTP says this property is **milliseconds**, and a Zune 30 obeys: a 4:18 track comes
/// back as 257969. Formatting that as seconds produced "4299:29" per track and a
/// 791-hour album. Values below 10 000 are read as seconds instead — no real track is
/// under ten seconds, so that only catches firmware that reports seconds.
fn duration_seconds(raw_ms: Option<u32>) -> Option<u32> {
    let raw = raw_ms?;
    if raw == 0 {
        return None;
    }
    Some(if raw < 10_000 { raw } else { (raw + 500) / 1000 })
}

/// Does this JPEG carry an EXIF (APP1/"Exif") segment? Cameras write one; cover art
/// normally does not, which is half of the "photo or cover" answer.
fn jpeg_has_exif(bytes: &[u8]) -> bool {
    if !bytes.starts_with(&[0xFF, 0xD8]) {
        return false;
    }
    let mut offset = 2usize;
    while offset + 4 <= bytes.len() {
        if bytes[offset] != 0xFF {
            offset += 1;
            continue;
        }
        let marker = bytes[offset + 1];
        // Start-of-scan or end-of-image: no more metadata segments.
        if marker == 0xDA || marker == 0xD9 {
            return false;
        }
        let length = u16::from_be_bytes([bytes[offset + 2], bytes[offset + 3]]) as usize;
        if marker == 0xE1 {
            let body = &bytes[offset + 4..(offset + 2 + length).min(bytes.len())];
            return body.starts_with(b"Exif\0");
        }
        offset += 2 + length;
    }
    false
}

/// The Zune encodes an object's kind in the high byte of its handle: 0x01 audio,
/// 0x03 image, 0x06 abstract album, 0x07 playlist. Naming it makes a dump readable and
/// makes "the covers are in a different segment" obvious.
fn handle_kind(handle: u32) -> String {
    match (handle >> 24) as u8 {
        0x00 => "root".to_owned(),
        0x01 => "audio".to_owned(),
        0x03 => "image".to_owned(),
        0x06 => "album".to_owned(),
        0x07 => "playlist".to_owned(),
        other => format!("0x{other:02x}?"),
    }
}

/// Readable name for the property codes we know, hex otherwise.
fn prop_label(code: u16) -> String {
    match code {
        mtp::prop::SIZE => "ObjectSize".to_owned(),
        mtp::prop::NAME => "Name".to_owned(),
        mtp::prop::ARTIST => "Artist".to_owned(),
        mtp::prop::ALBUM => "Album".to_owned(),
        mtp::prop::ALBUM_ARTIST => "AlbumArtist".to_owned(),
        mtp::prop::TRACK => "Track".to_owned(),
        mtp::prop::DURATION => "Duration".to_owned(),
        mtp::prop::OBJECT_REFS => "ObjectReferences".to_owned(),
        mtp::prop::DATE_CREATED => "DateCreated".to_owned(),
        mtp::prop::DATE_MODIFIED => "DateModified".to_owned(),
        mtp::prop::DATE_ADDED => "DateAdded(0xDC99)".to_owned(),
        other => format!("{other:#06x}"),
    }
}

/// One decoded property-list entry, with the handle's kind spelled out.
fn describe_prop(entry: &crate::core::mtp::PropEntry) -> String {
    match (&entry.text, entry.number) {
        (Some(text), _) => format!("{} = \"{text}\"", prop_label(entry.property)),
        (None, Some(number)) => format!(
            "{} = {} ({}) [{}]",
            prop_label(entry.property),
            number,
            hex(number),
            handle_kind(number)
        ),
        (None, None) => format!("{} = (empty)", prop_label(entry.property)),
    }
}

/// `#0x0004a1b2` style handle, so a dump can be pasted back without ambiguity.
fn hex(handle: u32) -> String {
    format!("{handle:#010x}")
}

/// The first bytes of a payload, so a wrong format is recognisable at a glance.
fn magic(data: &[u8]) -> String {
    let head: Vec<String> = data.iter().take(12).map(|b| format!("{b:02x}")).collect();
    let kind = if data.starts_with(&[0xFF, 0xD8, 0xFF]) {
        " JPEG"
    } else if data.starts_with(&[0x89, b'P', b'N', b'G']) {
        " PNG"
    } else if data.starts_with(b"BM") {
        " BMP"
    } else {
        ""
    };
    format!("{}{kind}", head.join(" "))
}

fn ui_children_left(total: usize) -> usize {
    total.saturating_sub(5)
}

fn file_label(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| path.display().to_string())
}

fn hex_or_dash(h: u32) -> String {
    format!("{h:#010x}")
}

fn hex_spaced(data: &[u8]) -> String {
    data.chunks(16)
        .map(|chunk| {
            chunk
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Where the MTPZ key file is looked for — mirrors `MtpzKeys::load(None)`.
pub fn key_candidates() -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Ok(env) = std::env::var("MTPZ_DATA") {
        candidates.push(PathBuf::from(env));
    }
    if let Some(home) = std::env::var_os("HOME") {
        candidates.push(PathBuf::from(home).join(".mtpz-data"));
    }
    candidates.push(PathBuf::from(".mtpz-data"));
    candidates
}

pub fn key_path() -> Option<PathBuf> {
    key_candidates().into_iter().find(|p| p.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exif_detection_separates_a_camera_photo_from_cover_art() {
        // A minimal JPEG: SOI, APP1 "Exif", then SOS.
        let mut camera = vec![0xFF, 0xD8];
        camera.extend_from_slice(&[0xFF, 0xE1, 0x00, 0x0A]);
        camera.extend_from_slice(b"Exif\0\0\0");
        camera.extend_from_slice(&[0xFF, 0xDA, 0x00, 0x02]);
        assert!(jpeg_has_exif(&camera));

        // Same shape, but the metadata segment is a JFIF APP0 and there is no Exif.
        let mut cover = vec![0xFF, 0xD8];
        cover.extend_from_slice(&[0xFF, 0xE0, 0x00, 0x10]);
        cover.extend_from_slice(b"JFIF\0");
        cover.extend_from_slice(&[0u8; 8]);
        cover.extend_from_slice(&[0xFF, 0xDA, 0x00, 0x02]);
        assert!(!jpeg_has_exif(&cover));

        assert!(!jpeg_has_exif(&[]));
        assert!(!jpeg_has_exif(&[0x89, b'P', b'N', b'G']));
        // The demo covers the board renders are not photos.
        assert!(!jpeg_has_exif(&demo_cover_jpeg(0)));
    }

    #[test]
    fn the_recovery_advice_is_the_one_that_works() {
        // Measured on macOS: `zune usb` (drain + probe) and a USB device reset both
        // leave a wedged Zune just as dead — only removing the cable brings it back.
        let hint = crate::core::usb::REPLUG_HINT;
        assert!(hint.contains("unplug"), "{hint}");
        assert!(hint.contains("3 seconds"), "{hint}");
        assert!(hint.contains("physically"), "{hint}");
        assert!(
            !hint.contains("zune usb"),
            "the advice must not point at a command that does not work: {hint}"
        );
        // And no stuck-pipe message may fall back to that advice either.
        let stuck = crate::core::usb::test_transfer_error_not_responding();
        let message = format!("{stuck:#}").to_lowercase();
        assert!(!message.contains("zune usb"), "{message}");
        assert!(message.contains("unplug"), "{message}");
    }

    #[test]
    fn cover_sources_are_distinguishable_in_the_ui() {
        // Each strategy needs its own readable label: "why is there no cover" must be
        // answerable from the album page alone.
        for source in [
            CoverSource::RefCoverArt,
            CoverSource::RefJpeg,
            CoverSource::ReverseRef,
            CoverSource::FolderMatch,
            CoverSource::None,
        ] {
            assert!(!source.label().is_empty());
        }
        assert!(CoverSource::RefCoverArt.is_found());
        assert!(CoverSource::ReverseRef.is_found());
        assert!(CoverSource::FolderMatch.is_found());
        assert!(!CoverSource::None.is_found());
    }

    #[test]
    fn handle_segments_name_the_object_kind() {
        // From the live device: 0x01 audio, 0x03 image, 0x06 abstract album,
        // 0x07 playlist. This is what made "the covers are in another segment" visible.
        assert_eq!(handle_kind(0x0100_1316), "audio");
        assert_eq!(handle_kind(0x0300_0a0c), "image");
        assert_eq!(handle_kind(0x0600_12c1), "album");
        assert_eq!(handle_kind(0x0700_0662), "playlist");
        assert_eq!(handle_kind(0), "root");
        assert_eq!(handle_kind(0x0F00_0001), "0x0f?");
    }

    #[test]
    fn property_dumps_are_readable() {
        use crate::core::mtp::PropEntry;
        // A string property.
        let text = PropEntry {
            handle: 0x0300_0a0c,
            property: mtp::prop::NAME,
            text: Some("ht_080917_094308.jpg".into()),
            number: None,
        };
        assert_eq!(describe_prop(&text), "Name = \"ht_080917_094308.jpg\"");
        // A numeric property is shown as a number *and* as a handle with its kind, so a
        // link to an album object cannot hide behind a bare integer.
        let number = PropEntry {
            handle: 0x0300_0a0c,
            property: 0xDE00,
            text: None,
            number: Some(0x0600_12c1),
        };
        let described = describe_prop(&number);
        assert!(described.contains("0x060012c1"), "{described}");
        assert!(described.contains("album"), "{described}");
        assert_eq!(prop_label(mtp::prop::OBJECT_REFS), "ObjectReferences");
        assert_eq!(prop_label(0xDEFA), "0xdefa");
    }

    #[test]
    fn durations_are_milliseconds_as_the_device_reports_them() {
        // From a real Zune: a 4:18 track arrives as 257969.
        assert_eq!(duration_seconds(Some(257_969)), Some(258));
        assert_eq!(format_duration(duration_seconds(Some(257_969)).unwrap()), "4:18");
        // Eleven of those was the 791-hour album.
        let album = 257_969u32 * 11;
        assert_eq!(format_duration(duration_seconds(Some(album)).unwrap()), "47:18");
        // A firmware that reports seconds is handled, not mangled.
        assert_eq!(duration_seconds(Some(258)), Some(258));
        assert_eq!(duration_seconds(Some(9_999)), Some(9_999));
        assert_eq!(duration_seconds(Some(10_000)), Some(10));
        assert_eq!(duration_seconds(Some(0)), None);
        assert_eq!(duration_seconds(None), None);
    }

    #[test]
    fn the_bulk_sweep_maps_every_property_we_show() {
        use crate::core::mtp::PropEntry;
        let entries = vec![
            PropEntry { handle: 7, property: mtp::prop::SIZE, text: None, number: Some(4_000_000) },
            PropEntry { handle: 7, property: mtp::prop::NAME, text: Some("Song".into()), number: None },
            PropEntry { handle: 7, property: mtp::prop::ARTIST, text: Some("Artist".into()), number: None },
            PropEntry { handle: 7, property: mtp::prop::ALBUM, text: Some("Album".into()), number: None },
            PropEntry { handle: 7, property: mtp::prop::TRACK, text: None, number: Some(3) },
            PropEntry { handle: 7, property: mtp::prop::DURATION, text: None, number: Some(257_969) },
            // A property we do not display must be ignored, not panic.
            PropEntry { handle: 7, property: 0xDC0B, text: Some("keywords".into()), number: None },
            PropEntry { handle: 8, property: mtp::prop::NAME, text: Some("Other".into()), number: None },
        ];
        let map = crate::core::library::collect_props(entries);
        let track = crate::core::library::DeviceTrack::from_props(7, mtp::fmt::MP3, map.get(&7));
        assert_eq!(track.size, 4_000_000);
        assert_eq!(track.title.as_deref(), Some("Song"));
        assert_eq!(track.artist.as_deref(), Some("Artist"));
        assert_eq!(track.album.as_deref(), Some("Album"));
        assert_eq!(track.track, Some(3));
        assert_eq!(duration_seconds(track.duration_ms), Some(258));
        // An object with no swept properties still produces a usable (empty) track.
        let lonely = crate::core::library::DeviceTrack::from_props(99, mtp::fmt::WMA, None);
        assert_eq!(lonely.size, 0);
        assert!(lonely.title.is_none());
        assert_eq!(lonely.format, mtp::fmt::WMA);
    }

    #[test]
    fn only_real_image_formats_may_be_cover_art() {
        // Regression, from a live run: a 2.2 MB WMA track listed among an album's
        // references was fetched as cover art because it was "not a track handle".
        // Format is the only admissible test.
        assert_eq!(cover_source_for_format(fmt::COVER_ART), Some(CoverSource::RefCoverArt));
        assert_eq!(cover_source_for_format(fmt::JPEG), Some(CoverSource::RefJpeg));
        for format in [
            fmt::MP3,
            fmt::WMA,
            fmt::AAC,
            fmt::WMV,
            fmt::MP4,
            fmt::PLAYLIST,
            fmt::ASSOCIATION,
            fmt::ABSTRACT_ALBUM,
            0x0000,
            0xFFFF,
        ] {
            assert_eq!(
                cover_source_for_format(format),
                None,
                "{format:#06x} must never be treated as cover art"
            );
        }
    }

    #[test]
    fn payloads_are_identified_so_failures_name_the_real_format() {
        // The bytes that produced "the image format could not be determined".
        assert_eq!(
            describe_payload(&[0x30, 0x26, 0xB2, 0x75, 0x8E, 0x66, 0xCF, 0x11, 0x00]),
            Some(PayloadKind::Asf)
        );
        assert!(!PayloadKind::Asf.is_image());
        assert!(PayloadKind::Asf.name().contains("WMA"));

        assert_eq!(describe_payload(&[0xFF, 0xD8, 0xFF, 0xE0]), Some(PayloadKind::Jpeg));
        assert!(PayloadKind::Jpeg.is_image());
        assert_eq!(
            describe_payload(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]),
            Some(PayloadKind::Png)
        );
        assert_eq!(describe_payload(b"RIFF____WAVE"), Some(PayloadKind::Riff));
        assert_eq!(describe_payload(b"ID3\x04\x00"), Some(PayloadKind::Mp3));
        assert_eq!(describe_payload(&[0xFF, 0xFB, 0x90, 0x00]), Some(PayloadKind::Mp3));
        assert!(!PayloadKind::Mp3.is_image());
        assert!(!PayloadKind::Unknown.is_image());
    }

    #[test]
    fn album_names_normalise_across_art_naming() {
        // The two sides of a cover match: an album title and a cover file name.
        assert_eq!(normalise("Kid A"), "kida");
        assert_eq!(normalise("Kid A.jpg"), "kida");
        assert_eq!(normalise("The Miseducation of Lauryn Hill"), "themiseducationoflaurynhill");
        assert_eq!(
            normalise("The Miseducation of Lauryn Hill.jpeg"),
            normalise("The Miseducation of Lauryn Hill")
        );
        assert_eq!(normalise("蓝莓之夜.jpg"), "蓝莓之夜");
    }

    #[test]
    fn covers_match_exactly_first_then_loosely_but_never_wildly() {
        let index = vec![
            (1u32, "Blur - Song 2.jpg".to_owned(), fmt::JPEG),
            (2, "Kid A.jpg".to_owned(), fmt::COVER_ART),
            (3, "Abbey Road.jpg".to_owned(), fmt::COVER_ART),
        ];
        assert_eq!(match_cover(&index, "Kid A").map(|(h, _)| h), Some(2));
        assert_eq!(match_cover(&index, "Kid A ").map(|(h, _)| h), Some(2));
        // A loose game: the album name appears inside the file name.
        assert_eq!(
            match_cover(&index, "Abbey Road (Remastered)").map(|(h, _)| h),
            Some(3)
        );
        // Nothing plausible: no match rather than a wrong cover.
        assert_eq!(match_cover(&index, "Nevermind"), None);
        assert_eq!(match_cover(&index, "x"), None);
        assert_eq!(match_cover(&[], "Kid A"), None);
    }

    #[test]
    fn years_parse_from_both_shapes_and_reject_nonsense() {
        assert_eq!(parse_year_text("20101018T030405"), Some(2010));
        assert_eq!(parse_year_text("1997"), Some(1997));
        assert_eq!(parse_year_text("1997-08-01"), Some(1997));
        assert_eq!(parse_year_text(""), None);
        assert_eq!(parse_year_text("n/a"), None);
        // Out of range dates are dropped rather than shown as a wrong year.
        assert_eq!(parse_year_text("0001"), None);
        assert_eq!(parse_year_text("9999"), None);

        // Unix epoch: 2000-01-01, 2007-06-15.
        assert_eq!(year_from_unix(946_684_800), Some(2000));
        assert_eq!(year_from_unix(1_182_211_200), Some(2007));
        assert_eq!(year_from_unix(0), None);
        assert_eq!(year_from_unix(4_102_444_801), None);
    }

    #[test]
    fn civil_dates_are_correct_at_leap_boundaries() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(59), (1970, 3, 1));
        assert_eq!(civil_from_days(10_957), (2000, 1, 1)); // leap year
        assert_eq!(civil_from_days(11_016), (2000, 2, 29));
        assert_eq!(civil_from_days(-1), (1969, 12, 31));
    }

    #[test]
    fn albums_group_tracks_and_sum_their_statistics() {
        let tracks = vec![
            Track {
                handle: 1,
                filename: "01.mp3".into(),
                size: 1000,
                format: fmt::MP3,
                title: "one".into(),
                artist: "A".into(),
                album: "Album".into(),
                track_no: Some(1),
                duration: Some(60),
            },
            Track {
                handle: 2,
                filename: "02.mp3".into(),
                size: 2000,
                format: fmt::MP3,
                title: "two".into(),
                artist: "A".into(),
                album: "Album".into(),
                track_no: Some(2),
                duration: Some(90),
            },
            Track {
                handle: 3,
                filename: "x.mp3".into(),
                size: 500,
                format: fmt::WMA,
                title: "x".into(),
                artist: "B".into(),
                album: String::new(),
                track_no: None,
                duration: None,
            },
        ];
        let entries = vec![AlbumEntry {
            name: "Album".to_owned(),
            handle: 99,
            refs: vec![1, 2, 42], // 42 is the cover-art reference
            album_artist: Some("A".to_owned()),
            year: Some(2000),
        }];
        let albums = build_albums(&tracks, &entries);
        assert_eq!(albums.len(), 2);
        let album = albums.iter().find(|a| a.name == "Album").unwrap();
        assert_eq!(album.track_handles, vec![1, 2]);
        assert_eq!(album.tracks, 2);
        assert_eq!(album.bytes, 3000);
        assert_eq!(album.duration, 150);
        assert_eq!(album.artist, "A");
        assert_eq!(album.year, Some(2000));
        // Cover resolution needs the device, so it is not guessed here.
        assert_eq!(album.cover_source, CoverSource::None);
        let unknown = albums.iter().find(|a| a.name == "Unknown album").unwrap();
        assert_eq!(unknown.tracks, 1);
        assert_eq!(unknown.bytes, 500);
        // A WMA track must be representable: the device's own library is full of them.
        assert_eq!(tracks[2].format, fmt::WMA);
    }

    #[test]
    fn every_demo_cover_is_a_decodable_jpeg() {
        // A single bad index would show up as a mysterious "!" badge in the grid.
        for index in 0..16 {
            let jpeg = demo_cover_jpeg(index);
            assert!(
                jpeg.starts_with(&[0xFF, 0xD8, 0xFF]),
                "demo cover {index} is not a JPEG: {:02x?}",
                &jpeg[..4]
            );
            let decoded = decode_cover(&jpeg).unwrap_or_else(|e| panic!("cover {index}: {e:#}"));
            assert_eq!((decoded.width, decoded.height), (240, 240));
            assert_eq!(decoded.rgba.len(), 240 * 240 * 4);
        }
    }
}
