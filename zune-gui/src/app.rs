//! The egui front end.
//!
//! The UI thread only ever reads the snapshots the worker hands it and sends
//! [`Command`]s back; it never touches USB.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use anyhow::{Context, Result};
use eframe::egui::{
    self, Align, Align2, Color32, CornerRadius, FontId, Layout, Rect, RichText, Sense, Stroke, Vec2,
};

use crate::cache;
use crate::design::anim::Motion;
use crate::design::paint::{self, GlowResponse, GLOW_MD, GLOW_SM, GLOW_TEXT};
use crate::design::tokens::{
    body, body_sm, display, h1, h2, h3, human_bytes, label, mono, r_md, r_pill, r_sm, subtitle,
    tracked, BODY_SM, CARD_GAP, CANVAS, DISPLAY, F_LIGHT, F_XLIGHT, HAIRLINE, NEON_AMBER, NEON_LIME,
    NEON_MAGENTA, NEON_ORANGE, SP_12, SP_16, SP_24, SP_32, SP_8, SURFACE_1, SURFACE_2, TEXT_1,
    TEXT_2, TEXT_3,
};
use crate::ui::track_row::{track_row, TrackRow};
use crate::core::library;
use crate::model::*;
use crate::theme::{self, *};
use crate::ui::album_tile::{AlbumTile, CoverBadge};
use crate::ui::board::{self, BoardState};
use crate::ui::device_pill::{device_pill, PillState};
use crate::ui::glass_card::GlassCard;
use crate::ui::icon_button::IconButton;
use crate::ui::icons::Icon;
use crate::ui::progress_ring::ProgressRing;
use crate::ui::section::section_rule;
use crate::worker::{self, Worker};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Page {
    /// The Bento sheet: library totals, capacity, device — the front door.
    Home,
    Albums,
    Artists,
    Tracks,
    Push,
    Device,
    Wifi,
    Log,
    /// The design-system component board (a dev surface, so it sits last).
    Board,
}

impl Page {
    const ALL: [Page; 9] = [
        Page::Home,
        Page::Albums,
        Page::Artists,
        Page::Tracks,
        Page::Push,
        Page::Device,
        Page::Wifi,
        Page::Log,
        Page::Board,
    ];

    fn title(self) -> &'static str {
        match self {
            Page::Home => "Collection",
            Page::Tracks => "Tracks",
            Page::Albums => "Albums",
            Page::Artists => "Artists",
            Page::Push => "Push",
            Page::Device => "Device",
            Page::Wifi => "Wi-Fi",
            Page::Log => "Log",
            Page::Board => "Design",
        }
    }

    fn subtitle(self) -> &'static str {
        match self {
            Page::Home => "everything on the device, at a glance",
            Page::Tracks => "every track on the device",
            Page::Albums => "albums and their cover art",
            Page::Artists => "the names behind the albums",
            Page::Push => "copy music to the Zune",
            Page::Device => "identity, storage and the MTPZ session",
            Page::Wifi => "vendor WiFi opcodes 0x9225 / 0x9227 / 0x9228",
            Page::Log => "everything the device thread reported",
            Page::Board => "v1.1 — 极致纯黑 · 小面积霓虹撞色 · 纯白大字号",
        }
    }

    /// The accent. v1.1 §2.1.2 allows the product exactly one neon; the per-page
    /// signatures were tried and pulled back out — six hues on one screen read as a
    /// paint chart, not as a system. Every page returns the same one now.
    fn accent(self) -> Color32 {
        NEON_ORANGE
    }

    /// The navigation glyph. Chosen so the rail can be read at a glance: a disc for
    /// the cover wall, a player body for the hardware page, a tray for uploads.
    fn icon(self) -> crate::ui::icons::Icon {
        use crate::ui::icons::Icon;
        match self {
            Page::Home => Icon::Home,
            Page::Albums => Icon::Disc,
            Page::Artists => Icon::Person,
            Page::Tracks => Icon::Note,
            Page::Push => Icon::Upload,
            Page::Device => Icon::Device,
            Page::Wifi => Icon::Wifi,
            Page::Log => Icon::List,
            Page::Board => Icon::Grid,
        }
    }
}

/// Ordering of the album wall.
#[derive(Clone, Copy, PartialEq, Eq)]
enum AlbumSort {
    Name,
    Artist,
    Year,
}

impl AlbumSort {
    const ALL: [AlbumSort; 3] = [AlbumSort::Name, AlbumSort::Artist, AlbumSort::Year];

    fn label(self) -> &'static str {
        match self {
            AlbumSort::Name => "Name",
            AlbumSort::Artist => "Artist",
            AlbumSort::Year => "Year",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SortKey {
    Track,
    Title,
    Artist,
    Album,
    Duration,
    Size,
}

impl SortKey {
    fn label(self) -> &'static str {
        match self {
            SortKey::Track => "#",
            SortKey::Title => "Title",
            SortKey::Artist => "Artist",
            SortKey::Album => "Album",
            SortKey::Duration => "Length",
            SortKey::Size => "Size",
        }
    }
}

/// A track flattened for rendering, so the table closure does not borrow the
/// library while it mutates the selection.
#[derive(Clone)]
struct Row {
    handle: u32,
    track_no: Option<u16>,
    title: String,
    artist: String,
    album: String,
    length: String,
    size: u32,
}

enum CoverState {
    Loading,
    Ready(egui::TextureHandle),
    Failed(String),
}

/// One row of the Artists page: the library's albums folded by artist name.
struct ArtistRow {
    name: String,
    albums: usize,
    tracks: usize,
    duration: u32,
    /// The thumbnail is borrowed from one of the artist's albums — the device has no
    /// image object of its own for an artist. These three mirror the album wall's own
    /// lookup: a device handle to download, or a synthetic key plus an album name to
    /// send down the fallback ladder.
    key: u32,
    handle: Option<u32>,
    album_name: String,
    /// The album's first track: its file may carry the art in an ID3 frame.
    track_handle: Option<u32>,
    /// Whether that album has artwork *on the device*. Not a gate on asking for a cover:
    /// a Zune 30 stores art in the audio files, so this is false for nearly every album
    /// and the fallback ladder is the normal path, not the exception.
    has_art: bool,
    /// Whether the representative album has been picked yet. Separate from `has_art`
    /// because "this album has art on the device" and "stop looking for a better album"
    /// are different questions.
    art_chosen: bool,
}

pub struct ZuneApp {
    worker: Worker,
    link: Link,
    snapshot: DeviceSnapshot,
    /// Where `tracks`/`albums` came from, so the badge can never describe the wrong
    /// library. Sent by the worker right after every library.
    origin: DataOrigin,
    tracks: Vec<Track>,
    albums: Vec<Album>,
    logs: Vec<LogLine>,
    last_log: Option<LogLine>,
    progress: Option<Progress>,
    busy: bool,
    last_summary: Option<(bool, String)>,

    page: Page,
    search: String,
    sort: SortKey,
    sort_desc: bool,
    selected: HashSet<u32>,
    album_filter: Option<String>,

    covers: HashMap<u32, CoverState>,
    /// Where each cover came from ("device", "local file · x.mp3", "online · iTunes"),
    /// so the album page can say it instead of implying everything is from the Zune.
    cover_origins: HashMap<u32, String>,
    cover_pending: HashSet<u32>,

    /// Paths queued for the push page.
    queue: Vec<PathBuf>,
    path_input: String,
    album_mode: bool,
    album_name: String,
    cover_path: String,
    make_playlist: bool,

    /// Albums page: ordering, filter and the album whose detail view is open.
    album_sort: AlbumSort,
    album_search: String,
    album_open: Option<String>,
    /// Artists page: the filter over the folded artist list.
    artist_search: String,
    /// Set once the first frame has run (demo mode uses it to wait for the worker).
    warmed_up: bool,
    /// Set by `ZUNE_REFRESH_ON_START=1|force`: run one Refresh once the device is up.
    /// A dev affordance like `ZUNE_WIFI_SCAN` — driving a button from a screenshot
    /// session is unreliable when another window can take focus mid-run.
    refresh_on_start: Option<bool>,
    /// P0 component board demo state.
    board: BoardState,
    confirm_delete: Option<Vec<u32>>,
    wifi_hex: String,
    /// Networks from the last scan, strongest first. Empty until one has run.
    wifi_networks: Vec<crate::core::wifi::WifiNetwork>,
    /// Set by `ZUNE_WIFI_SCAN=1`: run one scan as soon as the device is connected.
    /// A dev affordance like `ZUNE_PAGE` — clicking a button in a screenshot session is
    /// not always reliable, and this makes the scan reachable without one.
    wifi_autoscan: bool,
    wifi_profile_input: String,
    wifi_note: String,
    /// Set by `ZUNE_SCREENSHOT`: draw for a while, save one PNG, close.
    shot: Option<Shot>,
}

/// `ZUNE_SCREENSHOT=/path/out.png` — draw, save one PNG, close.
///
/// The pixels come from egui's own framebuffer rather than from the screen, which is the
/// point: `screencapture` can only photograph whatever is on top, and the machine is
/// usually in use for something else while a UI change wants checking. `ZUNE_PAGE` picks
/// the page; `ZUNE_SCREENSHOT_FRAMES` (default 120) is how long to let the cached library
/// and its covers arrive before the shutter opens.
struct Shot {
    path: PathBuf,
    wait: u32,
    frame: u32,
    asked: bool,
}

impl Shot {
    fn from_env() -> Option<Self> {
        let path = std::env::var_os("ZUNE_SCREENSHOT")?;
        let wait = std::env::var("ZUNE_SCREENSHOT_FRAMES")
            .ok()
            .and_then(|value| value.trim().parse().ok())
            .unwrap_or(120);
        Some(Shot {
            path: PathBuf::from(path),
            wait,
            frame: 0,
            asked: false,
        })
    }
}

impl ZuneApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        // Fonts (Inter variable + JetBrains Mono + CJK fallback) and the tokens.
        crate::design::install(&cc.egui_ctx);
        let worker = Worker::spawn(cc.egui_ctx.clone());
        Self {
            worker,
            link: Link::Disconnected,
            snapshot: DeviceSnapshot::default(),
            origin: DataOrigin::default(),
            tracks: Vec::new(),
            albums: Vec::new(),
            logs: Vec::new(),
            last_log: None,
            progress: None,
            busy: false,
            last_summary: None,
            // Dev affordance: ZUNE_PAGE=home|albums|artists|tracks|push|device|wifi|log|
            // design lets the screenshot tooling park on a page without a click. The
            // default is the front door.
            page: match std::env::var("ZUNE_PAGE").ok().as_deref() {
                Some("home") | Some("collection") => Page::Home,
                Some("tracks") => Page::Tracks,
                Some("albums") => Page::Albums,
                Some("artists") => Page::Artists,
                Some("push") => Page::Push,
                Some("device") => Page::Device,
                Some("wifi") => Page::Wifi,
                Some("log") => Page::Log,
                Some("design") | Some("board") => Page::Board,
                _ => Page::Home,
            },
            search: String::new(),
            sort: SortKey::Album,
            sort_desc: false,
            selected: HashSet::new(),
            album_filter: None,
            covers: HashMap::new(),
            cover_origins: HashMap::new(),
            cover_pending: HashSet::new(),
            queue: Vec::new(),
            path_input: String::new(),
            album_mode: false,
            album_name: String::new(),
            cover_path: String::new(),
            make_playlist: true,
            album_sort: AlbumSort::Name,
            album_search: String::new(),
            // Dev affordance: ZUNE_ALBUM="Kid A" opens that detail view directly,
            // so the page can be reviewed/screenshot without a click.
            album_open: std::env::var("ZUNE_ALBUM").ok(),
            artist_search: String::new(),
            warmed_up: false,
            refresh_on_start: match std::env::var("ZUNE_REFRESH_ON_START").ok().as_deref() {
                Some("1") | Some("quick") | Some("true") => Some(false),
                Some("force") | Some("full") => Some(true),
                _ => None,
            },
            board: BoardState::default(),
            confirm_delete: None,
            wifi_hex: String::new(),
            wifi_networks: Vec::new(),
            wifi_autoscan: std::env::var_os("ZUNE_WIFI_SCAN").is_some(),
            wifi_profile_input: String::new(),
            wifi_note: String::new(),
            shot: Shot::from_env(),
        }
    }

    /// The `ZUNE_SCREENSHOT` state machine: wait, ask, save, close.
    ///
    /// Three frames are involved and none of them can be skipped. Asking is one frame
    /// (`ViewportCommand` is read after the frame is submitted), the answer arrives as an
    /// input event on a later one, and the window has to stay alive until it does.
    fn screenshot_tick(&mut self, ctx: &egui::Context) {
        let Some(mut shot) = self.shot.take() else { return };
        shot.frame += 1;
        if shot.frame < shot.wait {
            self.shot = Some(shot);
            ctx.request_repaint();
            return;
        }
        if !shot.asked {
            shot.asked = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(Default::default()));
            self.shot = Some(shot);
            ctx.request_repaint();
            return;
        }
        let image = ctx.input(|input| {
            input.events.iter().find_map(|event| match event {
                egui::Event::Screenshot { image, .. } => Some(image.clone()),
                _ => None,
            })
        });
        match image {
            Some(image) => {
                match save_png(&shot.path, &image) {
                    Ok(()) => eprintln!("» screenshot written to {}", shot.path.display()),
                    Err(e) => eprintln!("» screenshot failed: {e:#}"),
                }
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            // Not this frame. Ask again rather than giving up: the reply can land either
            // side of the frame the request went out on, depending on the backend.
            None => {
                shot.asked = false;
                self.shot = Some(shot);
                ctx.request_repaint();
            }
        }
    }

    // ── event pump ───────────────────────────────────────────────────────────

    fn drain_events(&mut self, ctx: &egui::Context) {
        // ZUNE_DEMO builds a whole synthetic library, covers included, on the worker
        // thread. Waiting once for it to land keeps the very first frame from racing
        // the worker — which matters because eframe's screenshot hook fires on pass 2.
        if !self.warmed_up {
            self.warmed_up = true;
            if std::env::var_os("ZUNE_DEMO").is_some() {
                std::thread::sleep(std::time::Duration::from_millis(700));
            }
        }
        while let Some(event) = self.worker.try_recv() {
            match event {
                Event::Link(link) => self.link = link,
                Event::Snapshot(snapshot) => self.snapshot = *snapshot,
                Event::Library { tracks, albums } => {
                    let live: HashSet<u32> = tracks.iter().map(|t| t.handle).collect();
                    self.selected.retain(|h| live.contains(h));
                    let covers: HashSet<u32> = albums.iter().filter_map(|a| a.cover_handle).collect();
                    self.covers.retain(|h, _| covers.contains(h));
                    self.cover_pending.clear();
                    self.tracks = tracks;
                    self.albums = albums;
                }
                Event::Origin(origin) => self.origin = *origin,
                Event::Log(line) => {
                    self.last_log = Some(line.clone());
                    self.logs.push(line);
                    if self.logs.len() > 800 {
                        self.logs.remove(0);
                    }
                }
                Event::Progress(progress) => self.progress = Some(*progress),
                Event::TaskStarted { task, items } => {
                    self.busy = true;
                    self.progress = Some(Progress {
                        task,
                        items_total: items,
                        ..Default::default()
                    });
                }
                Event::TaskFinished { ok, summary } => {
                    self.busy = false;
                    self.last_summary = Some((ok, summary));
                    self.progress = None;
                }
                Event::Busy(busy) => self.busy = busy,
                Event::Cover { handle, image, origin } => {
                    let texture = ctx.load_texture(
                        format!("cover-{handle}"),
                        egui::ColorImage::from_rgba_unmultiplied(
                            [image.width, image.height],
                            &image.rgba,
                        ),
                        egui::TextureOptions::LINEAR,
                    );
                    self.covers.insert(handle, CoverState::Ready(texture));
                    self.cover_origins.insert(handle, origin);
                    self.cover_pending.remove(&handle);
                }
                Event::CoverFailed { handle, reason } => {
                    // The device refused it or the payload would not decode. This is the
                    // *expected* outcome for the thirty-odd albums nothing online matches,
                    // so it goes to the Log without claiming the status bar: a running
                    // commentary of covers that do not exist is noise, not status.
                    self.logs.push(LogLine::info(format!(
                        "cover art {handle:#010x} unavailable: {reason}"
                    )));
                    self.covers.insert(handle, CoverState::Failed(reason));
                    self.cover_pending.remove(&handle);
                }
                Event::CoversPrefetched { albums } => {
                    // The background fetch has already written these covers to disk, so the
                    // next frame can pick them up locally. Dropping the failure marks is
                    // what makes it try again; anything that already has a texture keeps it.
                    self.logs.push(LogLine::info(format!(
                        "cover prefetch: {albums} album(s) checked — loading any newly cached covers"
                    )));
                    self.last_log = self.logs.last().cloned();
                    self.covers.retain(|_, state| !matches!(state, CoverState::Failed(_)));
                    self.cover_pending.clear();
                }
                Event::WifiScan { networks, hex } => {
                    self.wifi_note = format!("{} network(s) visible", networks.len());
                    self.wifi_networks = networks;
                    self.wifi_hex = hex;
                }
            }
        }
    }

    /// Files and folders dropped on the window go straight into the push queue.
    fn take_dropped_files(&mut self, ctx: &egui::Context) {
        let dropped: Vec<PathBuf> = ctx.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .map(|f| f.path().to_path_buf())
                .collect()
        });
        if dropped.is_empty() {
            return;
        }
        for path in dropped {
            self.add_to_queue(path);
        }
        self.page = Page::Push;
    }

    fn add_to_queue(&mut self, path: PathBuf) {
        if !path.exists() {
            self.logs.push(LogLine::error(format!("{} does not exist", path.display())));
            return;
        }
        if path.is_dir() {
            let added = self.queue.len();
            self.queue.push(path.clone());
            log::debug!("queued folder {}", path.display());
            let _ = added;
        } else {
            self.queue.push(path);
        }
    }

    // ── derived views ────────────────────────────────────────────────────────

    fn visible_rows(&self) -> Vec<Row> {
        let needle = self.search.trim().to_lowercase();
        let mut rows: Vec<Row> = self
            .tracks
            .iter()
            .filter(|t| {
                if let Some(album) = &self.album_filter {
                    if !t.album.eq_ignore_ascii_case(album) {
                        return false;
                    }
                }
                if needle.is_empty() {
                    return true;
                }
                [&t.title, &t.artist, &t.album, &t.filename]
                    .iter()
                    .any(|field| field.to_lowercase().contains(&needle))
            })
            .map(|t| Row {
                handle: t.handle,
                track_no: t.track_no,
                title: t.display_title().to_string(),
                artist: t.artist.clone(),
                album: t.album.clone(),
                length: t.duration_label(),
                size: t.size,
            })
            .collect();

        let key = self.sort;
        rows.sort_by(|a, b| {
            let order = match key {
                SortKey::Track => a
                    .track_no
                    .unwrap_or(u16::MAX)
                    .cmp(&b.track_no.unwrap_or(u16::MAX))
                    .then_with(|| a.title.to_lowercase().cmp(&b.title.to_lowercase())),
                SortKey::Title => a.title.to_lowercase().cmp(&b.title.to_lowercase()),
                SortKey::Artist => a
                    .artist
                    .to_lowercase()
                    .cmp(&b.artist.to_lowercase())
                    .then_with(|| a.album.to_lowercase().cmp(&b.album.to_lowercase()))
                    .then_with(|| a.track_no.unwrap_or(0).cmp(&b.track_no.unwrap_or(0))),
                SortKey::Album => a
                    .album
                    .to_lowercase()
                    .cmp(&b.album.to_lowercase())
                    .then_with(|| a.track_no.unwrap_or(0).cmp(&b.track_no.unwrap_or(0))),
                SortKey::Duration => a.length.cmp(&b.length),
                SortKey::Size => a.size.cmp(&b.size),
            };
            if self.sort_desc {
                order.reverse()
            } else {
                order
            }
        });
        rows
    }

    /// Albums in display order, filtered by the album search box.
    fn album_rows(&self) -> Vec<(&Album, String)> {
        let needle = self.album_search.trim().to_lowercase();
        let mut rows: Vec<(&Album, String)> = self
            .albums
            .iter()
            .filter(|a| {
                needle.is_empty()
                    || a.name.to_lowercase().contains(&needle)
                    || a.artist.to_lowercase().contains(&needle)
            })
            .map(|a| {
                let mut meta = Vec::with_capacity(3);
                if !a.artist.is_empty() {
                    meta.push(a.artist.clone());
                }
                // The device often stores no date at all: omit it rather than printing
                // a placeholder that looks like missing data.
                if let Some(year) = a.year {
                    meta.push(year.to_string());
                }
                meta.push(format!("{} 首", a.tracks));
                let subtitle = meta.join(" · ");
                (a, subtitle)
            })
            .collect();

        match self.album_sort {
            AlbumSort::Name => rows.sort_by(|a, b| a.0.name.to_lowercase().cmp(&b.0.name.to_lowercase())),
            AlbumSort::Artist => rows.sort_by(|a, b| {
                a.0.artist
                    .to_lowercase()
                    .cmp(&b.0.artist.to_lowercase())
                    .then_with(|| a.0.name.to_lowercase().cmp(&b.0.name.to_lowercase()))
            }),
            AlbumSort::Year => rows.sort_by(|a, b| {
                b.0.year
                    .unwrap_or(0)
                    .cmp(&a.0.year.unwrap_or(0))
                    .then_with(|| a.0.name.to_lowercase().cmp(&b.0.name.to_lowercase()))
            }),
        }
        rows
    }

    /// Collection — the Bento sheet.
    ///
    /// Three cards across: the library's totals, the capacity ring, and the device. A
    /// strip of the albums that do have art sits underneath. The page owns the orange
    /// signature, so every neon on it is orange and nothing else — and per v1.1 §2.1.2
    /// none of it is a fill, only figures, a ring, a rule and a glow.
    fn home_page(&mut self, ui: &mut egui::Ui, motion: &mut Motion) {
        self.banner(ui);

        const CARD_H: f32 = 158.0;
        const RING_W: f32 = 238.0;
        const DEVICE_W: f32 = 252.0;
        /// Where each card's own content starts, below its label. One number for all
        /// three so the figures sit on a shared line across the row — they used to be
        /// 36, a hand-tuned 22 and 28, which is why the ring looked low and the device
        /// name looked high.
        const CARD_BODY_TOP: f32 = 34.0;
        /// How far a card's last line sits above its bottom edge. The library and device
        /// cards both end in a footnote, and anchoring both from the bottom is what puts
        /// them on the same baseline.
        const CARD_FOOT: f32 = 4.0;
        /// Vertical padding, shared by all three cards. The ring card used to be 16 while
        /// the others were 20, which put its label four pixels above theirs — invisible on
        /// its own, obvious next to the other two.
        const CARD_PAD_Y: f32 = 20.0;
        let library_w = (ui.available_width() - RING_W - DEVICE_W - CARD_GAP * 2.0).max(300.0);

        let mut goto: Option<Page> = None;
        let mut open_album: Option<String> = None;

        ui.horizontal(|ui| {
            // ── the library's own numbers ───────────────────────────────────────
            GlassCard::new(Vec2::new(library_w, CARD_H))
                .accent(NEON_ORANGE)
                .padding(Vec2::new(SP_24, CARD_PAD_Y))
                .salt(1)
                .show(ui, motion, |ui, rect, _motion| {
                    let painter = ui.painter();
                    let head = painter.layout_no_wrap(tracked("library"), label(), TEXT_3);
                    painter.galley(rect.left_top(), head, TEXT_3);

                    let stats = [
                        (thousands(self.snapshot.tracks as u64), "TRACKS"),
                        (thousands(self.snapshot.albums as u64), "ALBUMS"),
                        (thousands(self.snapshot.playlists as u64), "PLAYLISTS"),
                    ];
                    let column = rect.width() / 3.0;
                    for (index, (value, name)) in stats.iter().enumerate() {
                        let x = rect.left() + column * index as f32;
                        let y = rect.top() + CARD_BODY_TOP;
                        let number = painter.layout_no_wrap(value.clone(), h1(), TEXT_1);
                        paint::glow_galley(
                            painter,
                            egui::pos2(x, y),
                            &number,
                            NEON_ORANGE,
                            GLOW_TEXT,
                            GlowResponse::IDLE,
                        );
                        let height = number.size().y;
                        painter.galley(egui::pos2(x, y), number, TEXT_1);
                        // Follow the figure's own height instead of a fixed 52 px: the
                        // galley carries leading above the cap, and hard-coding the gap
                        // left it looking adrift under a 44 pt number.
                        let caption = painter.layout_no_wrap(tracked(name), label(), TEXT_3);
                        painter.galley(egui::pos2(x, y + height + 1.0), caption, TEXT_3);
                    }

                    if self.snapshot.library_bytes > 0 {
                        // The format breakdown belongs here rather than in a section of its
                        // own: it answers the same question as the figure above it ("what is
                        // on here"), and "all WMA" is easier to notice next to the number it
                        // explains. This is also the only place the client shows it.
                        let mut foot_text =
                            format!("{} of audio", human_bytes(self.snapshot.library_bytes));
                        if !self.snapshot.format_summary.is_empty() {
                            foot_text.push_str(" · ");
                            foot_text.push_str(&self.snapshot.format_summary);
                        }
                        let foot = painter.layout_no_wrap(foot_text, body_sm(), TEXT_2);
                        painter.galley(
                            egui::pos2(rect.left(), rect.bottom() - CARD_FOOT - foot.size().y),
                            foot,
                            TEXT_2,
                        );
                    }
                });

            // ── capacity ────────────────────────────────────────────────────────
            GlassCard::new(Vec2::new(RING_W, CARD_H))
                .accent(NEON_ORANGE)
                // Horizontal padding is tighter because this card is narrower; vertical
                // matches the other two so all three labels land on one line.
                .padding(Vec2::new(SP_16, CARD_PAD_Y))
                .salt(2)
                .show(ui, motion, |ui, rect, motion| {
                    let head = ui
                        .painter()
                        .layout_no_wrap(tracked("capacity"), label(), TEXT_3);
                    ui.painter().galley(rect.left_top(), head, TEXT_3);

                    let used = self
                        .snapshot
                        .used()
                        .unwrap_or(self.snapshot.library_bytes);
                    let capacity = self.snapshot.capacity.unwrap_or(0);
                    let fraction = if capacity > 0 {
                        used as f32 / capacity as f32
                    } else {
                        0.0
                    };
                    let headline = match self.snapshot.free {
                        Some(free) => human_bytes(free),
                        None => "—".to_owned(),
                    };
                    // An 88px ring cannot hold "9.5 GB" at the figure size, so the unit
                    // moves down into the caption where it belongs anyway.
                    let (figure, unit) = match headline.split_once(' ') {
                        Some((value, unit)) => (value.to_owned(), unit.to_owned()),
                        None => (headline.clone(), String::new()),
                    };
                    let caption = if unit.is_empty() {
                        "FREE".to_owned()
                    } else {
                        format!("{unit} FREE")
                    };
                    // Centre the ring in whatever is left below the label. The old 22 px
                    // was tuned against one card height and stopped being right the
                    // moment either the padding or the ring changed.
                    const RING: f32 = 88.0;
                    let body = (rect.height() - CARD_BODY_TOP - RING) / 2.0;
                    ui.add_space(CARD_BODY_TOP + body.max(0.0));
                    ui.with_layout(Layout::top_down(Align::Center), |ui| {
                        ProgressRing::new(fraction, &figure, &caption)
                            .size(RING)
                            .accent(NEON_ORANGE)
                            .show(ui, motion, 1);
                    });
                });

            // ── the device ──────────────────────────────────────────────────────
            GlassCard::new(Vec2::new(DEVICE_W, CARD_H))
                .accent(NEON_ORANGE)
                .padding(Vec2::new(SP_24, CARD_PAD_Y))
                .salt(3)
                .show(ui, motion, |ui, rect, _motion| {
                    let painter = ui.painter();
                    let head = painter.layout_no_wrap(tracked("device"), label(), TEXT_3);
                    painter.galley(rect.left_top(), head, TEXT_3);

                    let name = if self.snapshot.name.is_empty() {
                        "not connected"
                    } else {
                        self.snapshot.name.as_str()
                    };
                    let name_galley = painter.layout_no_wrap(name.to_owned(), h2(), TEXT_1);
                    let name_y = rect.top() + CARD_BODY_TOP;
                    let name_h = name_galley.size().y;
                    painter.galley(egui::pos2(rect.left(), name_y), name_galley, TEXT_1);

                    // One lit dot for the link, then the facts a support ticket needs.
                    let dot = egui::pos2(rect.left() + 4.0, name_y + name_h + 8.0);
                    let tint = if self.link == Link::Connected {
                        NEON_LIME
                    } else {
                        NEON_ORANGE
                    };
                    paint::glow_circle(painter, dot, 3.5, tint, GLOW_SM, GlowResponse::IDLE);
                    painter.circle_filled(dot, 3.5, tint);
                    let state = painter.layout_no_wrap(
                        if self.link == Link::Connected {
                            "trusted MTPZ session".to_owned()
                        } else {
                            "no MTPZ session".to_owned()
                        },
                        body_sm(),
                        TEXT_2,
                    );
                    painter.galley(egui::pos2(rect.left() + 14.0, dot.y - state.size().y / 2.0), state, TEXT_2);

                    // Stacked from the bottom edge rather than down from the top, so the
                    // last line lands where the library card's footnote does. Collected
                    // first and laid out in reverse for the same reason.
                    let mut footnotes: Vec<String> = Vec::new();
                    if !self.snapshot.serial.is_empty() {
                        let serial = &self.snapshot.serial[..self.snapshot.serial.len().min(18)];
                        footnotes.push(format!("sn  {serial}"));
                    }
                    if !self.snapshot.firmware.is_empty() {
                        footnotes.push(format!("fw  {}", self.snapshot.firmware));
                    }
                    let mut y = rect.bottom() - CARD_FOOT;
                    for text in footnotes {
                        let galley = painter.layout_no_wrap(text, mono(11.0), TEXT_3);
                        y -= galley.size().y;
                        painter.galley(egui::pos2(rect.left(), y), galley, TEXT_3);
                        y -= 3.0;
                    }
                });
        });

        // ── the wall ────────────────────────────────────────────────────────
        // Request before drawing: the worker is cache-first, so the albums whose art is
        // already on disk come straight back and the rest fall through to the online
        // ladder without blocking the frame. Note this cannot filter on cover_source —
        // that is 0 for every album on this device, because the Zune genuinely stores no
        // cover objects. "Has art" is a fact about the cache, not about the library.
        let first: Vec<(u32, Option<u32>, String, String, Option<u32>)> = self
            .albums
            .iter()
            .take(12)
            .map(|album| {
                (
                    album.cover_key,
                    album.cover_handle,
                    album.artist.clone(),
                    album.name.clone(),
                    album.track_handles.first().copied(),
                )
            })
            .collect();
        for (key, handle, artist, album, track_handle) in first {
            if self.covers.contains_key(&key) || self.cover_pending.contains(&key) {
                continue;
            }
            match handle {
                Some(handle) => self.request_cover(handle),
                None => {
                    self.cover_pending.insert(key);
                    self.worker.send(Command::FetchFallbackCover {
                        key,
                        artist,
                        album,
                        track_handle,
                    });
                }
            }
        }

        /// Wide enough for a caption to be readable, narrow enough that eight of them plus
        /// gaps fill the column.
        const STRIP_TILE: f32 = 124.0;
        let ready = |key: &u32| matches!(self.covers.get(key), Some(CoverState::Ready(_)));
        let mut strip: Vec<&Album> = self.albums.iter().take(12).collect();
        // Albums that already have art lead the strip; the rest keep their order behind
        // them, so the row fills up as covers land instead of staying a row of blanks.
        strip.sort_by_key(|album| !ready(&album.cover_key));
        strip.truncate(8);
        let shown = strip.iter().filter(|album| ready(&album.cover_key)).count();
        let total = self.albums.len();

        ui.add_space(SP_24);
        section_rule(ui, "cover art", Some(&format!("{shown} of {total} albums")));
        if total == 0 {
            self.empty_hint(ui, "No library yet — Refresh reads the device once it is connected.");
        } else {
            ui.horizontal_top(|ui| {
                for album in &strip {
                    ui.vertical(|ui| {
                    ui.set_width(STRIP_TILE);
                    let (rect, response) =
                        ui.allocate_exact_size(Vec2::splat(STRIP_TILE), Sense::click());
                    match self.covers.get(&album.cover_key) {
                        Some(CoverState::Ready(texture)) => {
                            paint::rounded_image(ui.painter(), texture.id(), rect, r_md(), CANVAS);
                        }
                        _ => {
                            // Same quiet placeholder as the album wall.
                            let painter = ui.painter();
                            painter.rect_filled(rect, r_md(), SURFACE_1);
                            paint::paint_dot_grid(painter, rect.shrink(2.0), &paint::dot_grid(ui.ctx()));
                            paint::hairline(painter, rect, r_md());
                        }
                    }
                    // The same two-line caption the album wall puts under its tiles, so a
                    // cover says the same thing in both places. Without it the strip was
                    // eight anonymous squares and you had to hover to learn what they were.
                    ui.add_space(SP_8);
                    ui.add(
                        egui::Label::new(RichText::new(&album.name).font(h3()).color(TEXT_1))
                            .truncate(),
                    );
                    ui.add_space(1.0);
                    ui.add(
                        egui::Label::new(
                            RichText::new(&album.artist).font(body_sm()).color(TEXT_2),
                        )
                        .truncate(),
                    );
                    if response.clicked() {
                        open_album = Some(album.name.clone());
                    }
                    response.on_hover_text(format!("{} · {}", album.name, album.artist));
                    });
                }
            });
        }

        // ── shortcuts ───────────────────────────────────────────────────────
        ui.add_space(SP_24);
        section_rule(ui, "go to", None);
        ui.horizontal(|ui| {
            for page in [Page::Albums, Page::Artists, Page::Tracks, Page::Push, Page::Wifi] {
                if nav_chip(ui, page.icon(), page.title()).clicked() {
                    goto = Some(page);
                }
            }
        });

        if let Some(album) = open_album {
            self.album_open = Some(album);
            self.page = Page::Albums;
        } else if let Some(page) = goto {
            self.page = page;
        }
    }

    /// The library folded by artist name, in alphabetical order.
    ///
    /// Case is folded for grouping — "paramore" and "Paramore" are one artist — but the
    /// first spelling the library produced is the one shown. The device has no artist
    /// objects, so every number here is summed from the albums it owns.
    fn artist_rows(&self) -> Vec<ArtistRow> {
        let needle = self.artist_search.trim().to_lowercase();
        let mut by_name: HashMap<String, ArtistRow> = HashMap::new();
        for album in &self.albums {
            let display = if album.artist.trim().is_empty() {
                "Unknown Artist".to_owned()
            } else {
                album.artist.trim().to_owned()
            };
            let row = by_name
                .entry(display.to_lowercase())
                .or_insert_with(|| ArtistRow {
                    name: display.clone(),
                    albums: 0,
                    tracks: 0,
                    duration: 0,
                    key: album.cover_key,
                    handle: album.cover_handle,
                    album_name: album.name.clone(),
                    track_handle: album.track_handles.first().copied(),
                    has_art: album.cover_source.is_found(),
                    art_chosen: album.cover_source.is_found(),
                });
            row.albums += 1;
            row.tracks += album.tracks;
            row.duration += album.duration;
            // The thumbnail wants an album that can actually supply one. Device art wins,
            // then a cover already on disk, then whatever was seen first — the first album
            // an artist has is often the one without art, and on a Zune 30 *no* album has
            // device art, so "already on disk" is the case that matters here.
            if !row.art_chosen
                && (album.cover_source.is_found()
                    || self.covers.contains_key(&album.cover_key))
            {
                row.key = album.cover_key;
                row.handle = album.cover_handle;
                row.album_name = album.name.clone();
                row.track_handle = album.track_handles.first().copied();
                row.has_art = album.cover_source.is_found();
                row.art_chosen = true;
            }
        }
        let mut rows: Vec<ArtistRow> = by_name
            .into_values()
            .filter(|row| needle.is_empty() || row.name.to_lowercase().contains(&needle))
            .collect();
        rows.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
        rows
    }

    /// Artists: a tall, calm list. The page signature is magenta, so the only neon here
    /// is the hover marker and the monogram of an artist with no cached cover.
    fn artists_page(&mut self, ui: &mut egui::Ui, motion: &mut Motion) {
        self.banner(ui);
        let rows = self.artist_rows();
        let mut jump: Option<String> = None;

        // Ask for the thumbnails before drawing, using exactly the album wall's two
        // routes: a device handle gets downloaded, and an album with no art object goes
        // down the fallback ladder. `cover_pending` keeps this from repeating every frame.
        //
        // Note what this does *not* test: `row.has_art`. That flag says whether the
        // *device* has a cover object, and on a Zune 30 the answer is nearly always no —
        // the art is in the audio files' ID3 tags and has to come down the ladder. Gating
        // on it meant every artist drew a monogram, forever. The album wall never tested
        // it, which is exactly why the wall worked and this page did not.
        for row in &rows {
            if self.covers.contains_key(&row.key) || self.cover_pending.contains(&row.key) {
                continue;
            }
            match row.handle {
                Some(handle) => self.request_cover(handle),
                None => {
                    self.cover_pending.insert(row.key);
                    self.worker.send(Command::FetchFallbackCover {
                        key: row.key,
                        artist: row.name.clone(),
                        album: row.album_name.clone(),
                        track_handle: row.track_handle,
                    });
                }
            }
        }

        // Same toolbar idiom as Albums and Tracks: the count is the headline, filters sit
        // beside it, and the right-hand side carries facts about the list in mono.
        ui.horizontal(|ui| {
            stat_inline(ui, &thousands(rows.len() as u64), "artists", TEXT_1);
            ui.add_space(SP_16);
            text_field(ui, &mut self.artist_search, "filter artists", 220.0);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let tracks: usize = rows.iter().map(|r| r.tracks).sum();
                ui.label(
                    RichText::new(format!("{} tracks", thousands(tracks as u64)))
                        .font(mono(11.0))
                        .color(TEXT_3),
                );
            });
        });
        ui.add_space(SP_12);

        if rows.is_empty() {
            self.empty_hint(ui, "No artist matches that filter.");
            return;
        }

        egui::ScrollArea::vertical()
            .auto_shrink([false; 2])
            .show(ui, |ui| {
                for (index, row) in rows.iter().enumerate() {
                    let (rect, response) = ui.allocate_exact_size(
                        Vec2::new(ui.available_width(), 64.0),
                        Sense::click(),
                    );
                    let id = ui.id().with(("artist-row", index));
                    let hover = crate::design::anim::spring_at(
                        ui,
                        id.with("hover"),
                        response.hovered() as u8 as f32,
                        motion,
                    );
                    let painter = ui.painter();

                    if hover > 0.01 {
                        painter.rect_filled(rect, r_md(), paint::with_alpha(SURFACE_2, hover));
                        // The marker grows out of the row's centre rather than sliding in.
                        let bar = Rect::from_min_max(
                            egui::pos2(rect.left() + 2.0, rect.center().y - 11.0 * hover),
                            egui::pos2(rect.left() + 5.0, rect.center().y + 11.0 * hover),
                        );
                        paint::neon_fill(
                            painter,
                            bar,
                            r_pill(),
                            NEON_ORANGE,
                            GLOW_MD,
                            GlowResponse::hovered(hover),
                        );
                    }

                    // Thumbnail: one of the artist's covers when it is already cached,
                    // else a monogram on the surface tone.
                    // r_md, the same corner the album wall uses — these are the same
                    // covers, and two radii for one image reads as a mistake.
                    let thumb = Rect::from_center_size(
                        egui::pos2(rect.left() + 42.0, rect.center().y),
                        Vec2::splat(44.0),
                    );
                    match self.covers.get(&row.key) {
                        Some(CoverState::Ready(texture)) => {
                            paint::rounded_image(painter, texture.id(), thumb, r_md(), CANVAS);
                        }
                        _ => {
                            painter.rect_filled(thumb, r_md(), SURFACE_1);
                            paint::hairline(painter, thumb, r_md());
                            let initial = row
                                .name
                                .chars()
                                .next()
                                .map(|c| c.to_uppercase().to_string())
                                .unwrap_or_default();
                            painter.text(
                                thumb.center(),
                                Align2::CENTER_CENTER,
                                initial,
                                FontId::new(19.0, egui::FontFamily::Name(F_LIGHT.into())),
                                paint::with_alpha(NEON_ORANGE, 0.75),
                            );
                        }
                    }

                    painter.text(
                        egui::pos2(rect.left() + 76.0, rect.center().y - 9.0),
                        Align2::LEFT_CENTER,
                        &row.name,
                        h2(),
                        TEXT_1,
                    );
                    painter.text(
                        egui::pos2(rect.left() + 76.0, rect.center().y + 15.0),
                        Align2::LEFT_CENTER,
                        format!(
                            "{} album{} · {} tracks",
                            row.albums,
                            if row.albums == 1 { "" } else { "s" },
                            row.tracks
                        ),
                        body_sm(),
                        TEXT_3,
                    );
                    painter.text(
                        egui::pos2(rect.right() - 16.0, rect.center().y),
                        Align2::RIGHT_CENTER,
                        duration_label(row.duration),
                        mono(11.0),
                        TEXT_3,
                    );

                    if response.clicked() {
                        jump = Some(row.name.clone());
                    }
                    response.on_hover_text(format!("Show {} in Albums", row.name));
                }
            });

        if let Some(name) = jump {
            // Reuse the album filter so the wall lands on exactly this artist, sorted by
            // artist so the result reads as one block.
            self.album_search = name;
            self.album_sort = AlbumSort::Artist;
            self.page = Page::Albums;
        }
    }

    fn request_cover(&mut self, handle: u32) {
        // Synthetic keys (top bit set) are derived from artist + album *because* the device
        // has no cover object for that album. Asking it for one can only ever fail, and it
        // used to spend a GetObject plus a GetThumb per album to be told so — which is how
        // "expected a DATA container, got type 0x0003" ended up in the status bar. Those
        // covers come from the online ladder instead, which the worker prefetches.
        if handle & 0x8000_0000 != 0 {
            return;
        }
        if self.covers.contains_key(&handle) || self.cover_pending.contains(&handle) {
            return;
        }
        self.cover_pending.insert(handle);
        self.worker.send(Command::FetchCover(handle));
    }

    // ── panels ───────────────────────────────────────────────────────────────

    /// Where the library on screen came from, as a phrase: `cached · 3 min ago`, or
    /// `live`. Shared by the status line and the top bar's capsule tooltip.
    fn origin_text(&self) -> String {
        match (self.origin.source, self.origin.read_at) {
            (Source::Cached, Some(at)) => {
                format!("cached · {}", cache::relative_age(at, cache::now_unix()))
            }
            (Source::Cached, None) => "cached".to_owned(),
            (Source::Live, _) => "live".to_owned(),
        }
    }

    /// A quiet statement of where the library on screen came from: `cached · 3 min
    /// ago`, or `live`. text-2, because it is a fact about the data, not an alarm.
    fn data_origin_label(&self, ui: &mut egui::Ui) {
        // Nothing to qualify until there is a library (or a device) to describe.
        if self.tracks.is_empty() && self.link != Link::Connected && self.origin.read_at.is_none() {
            return;
        }
        let text = self.origin_text();
        let response = ui.label(RichText::new(text).color(TEXT_2).size(12.0));
        // The tooltip carries the rest of the provenance: which device, whether it is
        // on the end of the cable, and the worker's one-line note.
        let mut hover = String::new();
        if !self.origin.serial.is_empty() {
            hover.push_str(&format!("device {}\n", self.origin.serial));
        }
        hover.push_str(if self.origin.device_connected {
            "device connected\n"
        } else {
            "device not connected\n"
        });
        if let Some(note) = &self.origin.note {
            hover.push_str(note);
        }
        response.on_hover_text(hover.trim_end().to_owned());
    }

    /// The chrome. Three things, per UI_DESIGN §3: the mark on the left, the current page
    /// name in the middle at label weight (it echoes the 88pt title below rather than
    /// competing with it), and one device capsule on the right. The serial, the MTPZ tag
    /// and the cache age used to live here as loose text; they belong to the Device page,
    /// and the capsule carries a tooltip for the one that matters at a glance.
    fn top_bar(&mut self, ui: &mut egui::Ui, motion: &mut Motion) {
        egui::Panel::top("top")
            .exact_size(58.0)
            .frame(
                // Pure black chrome: the boundary is the hairline, not a lighter fill.
                egui::Frame::NONE
                    .fill(BG)
                    .stroke(Stroke::new(1.0, crate::design::tokens::HAIRLINE))
                    .inner_margin(egui::Margin::symmetric(14, 8)),
            )
            .show(ui, |ui| {
                ui.horizontal_centered(|ui| {
                    zune_mark(ui, 26.0);
                    ui.add_space(SP_8);
                    theme::wordmark(ui);

                    ui.add_space(SP_32);
                    ui.label(
                        RichText::new(tracked(self.page.title()))
                            .font(label())
                            .color(TEXT_3),
                    );

                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        // ── actions ─────────────────────────────────────────
                        let connected = self.link == Link::Connected;
                        if IconButton::new()
                            .enabled(connected && !self.busy)
                            .tip("Close the MTP session so the Zune can be unplugged")
                            .show(ui, motion, 1, Icon::Eject)
                            .clicked()
                        {
                            self.worker.send(Command::Eject);
                        }
                        if IconButton::new()
                            .enabled(connected && !self.busy)
                            .tip(
                                "Re-read the device — it checks whether anything changed first, \
                                 which takes a second. Shift-click to force the full pass.",
                            )
                            .show(ui, motion, 2, Icon::Refresh)
                            .clicked()
                        {
                            // Shift is the difference between a one-round-trip change check
                            // and a two-minute property sweep over every track.
                            let force = ui.input(|i| i.modifiers.shift);
                            self.worker.clear_cancel();
                            self.worker.send(Command::Refresh { force });
                        }
                        if !connected
                            && IconButton::new()
                                .enabled(!self.busy)
                                .tip("Open the device")
                                .show(ui, motion, 3, Icon::Sync)
                                .clicked()
                        {
                            self.worker.send(Command::Connect);
                        }

                        ui.add_space(SP_12);

                        // ── the device capsule ─────────────────────────────
                        let state = if self.busy {
                            PillState::Transferring
                        } else {
                            match self.link {
                                Link::Connected => PillState::Connected,
                                Link::Connecting => PillState::Connecting,
                                Link::Disconnected => PillState::Disconnected,
                            }
                        };
                        let detail = if self.link == Link::Connected {
                            match (self.snapshot.used(), self.snapshot.capacity) {
                                (Some(used), Some(cap)) if cap > 0 => format!(
                                    "{} · {} free of {}",
                                    self.snapshot.name,
                                    human_bytes(cap - used),
                                    human_bytes(cap)
                                ),
                                _ => self.snapshot.name.clone(),
                            }
                        } else if self.snapshot.key_error.is_some() {
                            "MTPZ key missing".to_owned()
                        } else {
                            String::new()
                        };
                        let hinted = if detail.is_empty() {
                            detail.clone()
                        } else {
                            format!(
                                "{detail}\n{}\nserial {}",
                                self.origin_text(),
                                self.snapshot.serial
                            )
                        };
                        device_pill(ui, motion, 1, state, &detail)
                            .on_hover_text(hinted);
                    });
                });
            });
    }

    fn side_bar(&mut self, ui: &mut egui::Ui) {
        egui::Panel::left("nav")
            .exact_size(178.0)
            .frame(
                egui::Frame::NONE
                    .fill(BG)
                    .inner_margin(egui::Margin::symmetric(10, 12)),
            )
            .show(ui, |ui| {
                // A short window used to crop the bottom of the rail — the library figures
                // were the first thing to go. Scrolling costs nothing when it is not needed
                // and keeps the figures reachable when it is.
                egui::ScrollArea::vertical()
                    .auto_shrink([false; 2])
                    .show(ui, |ui| {
                ui.add_space(2.0);
                // The rail takes the *current* page's signature colour, so it changes hue as
                // you move through the app instead of stamping one brand orange over every
                // room (v1.1 §2.1.2 — the mark, the marker and the figures all agree).
                let accent = self.page.accent();
                for page in Page::ALL {
                    let selected = self.page == page;
                    let (rect, response) =
                        ui.allocate_exact_size(Vec2::new(ui.available_width(), 34.0), egui::Sense::click());
                    let painter = ui.painter();
                    if selected {
                        painter.rect_filled(rect, crate::design::tokens::r_sm(), CARD);
                    } else if response.hovered() {
                        painter.rect_filled(rect, crate::design::tokens::r_sm(), crate::design::paint::with_alpha(Color32::WHITE, 0.05));
                    }
                    if selected {
                        // The indicator grows from the vertical centre, and glows
                        // (glow-md, per the v1.1 neon spec).
                        let bar = egui::Rect::from_min_max(
                            egui::pos2(rect.left() + 1.0, rect.center().y - 9.0),
                            egui::pos2(rect.left() + 4.5, rect.center().y + 9.0),
                        );
                        crate::design::paint::neon_fill(
                            painter,
                            bar,
                            crate::design::tokens::r_pill(),
                            accent,
                            crate::design::paint::GLOW_MD,
                            crate::design::paint::GlowResponse::IDLE,
                        );
                    }
                    let color = if selected {
                        accent
                    } else if response.hovered() {
                        TEXT_1
                    } else {
                        MUTED
                    };
                    // The icon leads the label, and both take the row's state colour, so
                    // a selected row reads as one lit object instead of a bar plus some
                    // text that happens to sit next to it.
                    page.icon().draw(
                        painter,
                        egui::Rect::from_center_size(
                            egui::pos2(rect.left() + 26.0, rect.center().y),
                            Vec2::splat(17.0),
                        ),
                        color,
                    );
                    painter.text(
                        egui::pos2(rect.left() + 46.0, rect.center().y),
                        egui::Align2::LEFT_CENTER,
                        page.title(),
                        crate::design::tokens::body(),
                        color,
                    );
                    if response.clicked() {
                        self.page = page;
                    }
                }

                ui.add_space(14.0);
                ui.separator();
                ui.add_space(10.0);
                ui.label(RichText::new("LIBRARY").color(MUTED).size(10.0).strong());
                ui.add_space(6.0);
                // The value carries the row, the unit is a quiet suffix. Printing "2425
                // tracks" as one string made every figure the same weight and turned four
                // numbers into a paragraph.
                rail_stat(ui, &thousands(self.snapshot.tracks as u64), "tracks", TEXT_1);
                rail_stat(ui, &thousands(self.snapshot.albums as u64), "albums", TEXT_1);
                rail_stat(ui, &thousands(self.snapshot.playlists as u64), "playlists", TEXT_1);
                // The one place the rail spends its accent: how much is on the device.
                let size = theme::human_bytes(self.snapshot.library_bytes);
                let (size_value, size_unit) = match size.split_once(' ') {
                    Some((value, unit)) => (value.to_owned(), unit.to_owned()),
                    None => (size, String::new()),
                };
                rail_stat(ui, &size_value, &size_unit, NEON_ORANGE);
                ui.add_space(10.0);
                if self.queue.len() > 0 {
                    theme::tag(ui, &format!("{} queued", self.queue.len()), ORANGE);
                }
                if !self.selected.is_empty() {
                    theme::tag(ui, &format!("{} selected", self.selected.len()), YELLOW);
                }
                    });
            });
    }

    fn status_bar(&mut self, ui: &mut egui::Ui) {
        egui::Panel::bottom("status")
            .exact_size(46.0)
            .frame(
                egui::Frame::NONE
                    .fill(BG)
                    .stroke(Stroke::new(1.0, crate::design::tokens::HAIRLINE))
                    .inner_margin(egui::Margin::symmetric(14, 6)),
            )
            .show(ui, |ui| {
                ui.horizontal_centered(|ui| {
                    match (&self.progress, self.busy) {
                        (Some(progress), true) => {
                            let bar = egui::ProgressBar::new(progress.fraction())
                                .desired_width(260.0)
                                .animate(true)
                                .fill(NEON_AMBER)
                                .text(format!("{:.0}%", progress.fraction() * 100.0));
                            ui.add(bar);
                            ui.label(RichText::new(&progress.task).strong());
                            ui.label(RichText::new(&progress.step).color(MUTED));
                            if ui.button("Stop after current").clicked() {
                                self.worker.request_cancel();
                            }
                        }
                        _ => match (&self.last_summary, &self.last_log) {
                            (Some((ok, summary)), _) => {
                                // A status line is furniture, not an alarm. A small lit
                                // dot carries the verdict and the sentence stays in body
                                // colour; painting the whole line in saturated red made
                                // every lookup failure the loudest thing on screen.
                                let (rect, _) =
                                    ui.allocate_exact_size(Vec2::new(10.0, 10.0), Sense::hover());
                                let tint = if *ok { NEON_LIME } else { NEON_MAGENTA };
                                let painter = ui.painter();
                                paint::glow_circle(
                                    painter,
                                    rect.center(),
                                    3.0,
                                    tint,
                                    GLOW_SM,
                                    GlowResponse::IDLE,
                                );
                                painter.circle_filled(rect.center(), 3.0, tint);
                                let width = (ui.available_width() - 260.0).max(120.0);
                                ui.allocate_ui_with_layout(
                                    egui::vec2(width, 20.0),
                                    Layout::left_to_right(Align::Center),
                                    |ui| {
                                        ui.add(
                                            egui::Label::new(
                                                RichText::new(summary.as_str()).color(TEXT_1),
                                            )
                                            .truncate(),
                                        );
                                    },
                                );
                            }
                            (None, Some(line)) => {
                                let (rect, _) =
                                    ui.allocate_exact_size(Vec2::new(10.0, 10.0), Sense::hover());
                                let tint = level_color(line.level);
                                let painter = ui.painter();
                                paint::glow_circle(
                                    painter,
                                    rect.center(),
                                    3.0,
                                    tint,
                                    GLOW_SM,
                                    GlowResponse::IDLE,
                                );
                                painter.circle_filled(rect.center(), 3.0, tint);
                                let width = (ui.available_width() - 260.0).max(120.0);
                                ui.allocate_ui_with_layout(
                                    egui::vec2(width, 20.0),
                                    Layout::left_to_right(Align::Center),
                                    |ui| {
                                        ui.add(
                                            egui::Label::new(
                                                RichText::new(line.text.replace('\n', " "))
                                                    .color(TEXT_2),
                                            )
                                            .truncate(),
                                        );
                                    },
                                );
                            }
                            (None, None) => {
                                ui.label(RichText::new("Ready").color(TEXT_3));
                            }
                        },
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if self.busy {
                            let (rect, _) = ui.allocate_exact_size(Vec2::new(10.0, 10.0), Sense::hover());
                            ui.painter().circle_filled(rect.center(), 4.0, NEON_AMBER);
                            ui.label(RichText::new("working…").color(NEON_AMBER));
                        }
                        if self.link == Link::Connected && !self.snapshot.topology.is_empty() {
                            ui.label(RichText::new(&self.snapshot.topology).color(MUTED).size(11.0));
                        }
                    });
                });
            });
    }

    // ── pages ────────────────────────────────────────────────────────────────

    /// The page title.
    ///
    /// `rule` draws the neon underline the Design page used to draw for itself. That
    /// page is why this takes a title argument at all: it is the one page whose heading
    /// differs from its rail label, and the one that wanted a rule, and those two
    /// differences were enough for it to grow a second implementation — which then
    /// drifted, cropping the glyphs on eight pages while the ninth looked fine.
    fn page_header(&mut self, ui: &mut egui::Ui, title: &str, rule: bool) {
        // v1.1 §2.2.1: the page title is 88 pt ExtraLight and it *bleeds*. It is
        // clipped by the content column instead of wrapping or shrinking. The italic
        // subtitle underneath is the Metro counterweight to all that white.
        let galley = ui.painter().layout_no_wrap(title.to_owned(), display(), TEXT_1);
        // Anchor to the top of the box, not the bottom. Lining the galley up by its
        // bottom edge pushed the ascenders above the clip rect, so the box cropped the
        // cap height of every title — "Albums" lost the top of its A. The tight box is
        // still the point, it just has to crop the descender underneath instead.
        // How tall the text actually is, not a guess. At 88 pt ExtraLight the galley is
        // taller than the font size (leading plus ascender), and a box sized from the font
        // size alone cropped the bottom of the glyphs. Taking the galley's own height means
        // the box always fits what it holds.
        let height = galley.size().y;
        log::debug!(
            "page title {:?}: galley {:.1}px tall, DISPLAY {DISPLAY}, self-reported {:?}",
            self.page.title(),
            height,
            galley.size()
        );
        let (rect, _) =
            ui.allocate_exact_size(egui::vec2(ui.available_width(), height), Sense::hover());
        let painter = ui.painter().with_clip_rect(rect);
        painter.galley(egui::pos2(rect.left(), rect.top()), galley, TEXT_1);

        if rule {
            // A neon element, so it glows — the same two-layer treatment as the mark.
            let rule_y = rect.bottom() - SP_8;
            let rule_rect = egui::Rect::from_min_max(
                egui::pos2(rect.left(), rule_y - 1.0),
                egui::pos2(rect.left() + 240.0, rule_y + 1.0),
            );
            paint::glow_rect(
                ui.painter(),
                rule_rect,
                egui::CornerRadius::same(2),
                NEON_ORANGE,
                GLOW_SM,
                GlowResponse::IDLE,
            );
            ui.painter().line_segment(
                [
                    egui::pos2(rect.left(), rule_y),
                    egui::pos2(rect.left() + 240.0, rule_y),
                ],
                Stroke::new(2.0, NEON_ORANGE),
            );
        }

        ui.add_space(2.0);
        ui.label(
            RichText::new(self.page.subtitle())
                .font(subtitle())
                .color(TEXT_3),
        );
        ui.add_space(SP_8);
    }

    fn banner(&mut self, ui: &mut egui::Ui) {
        // A wedged link outranks everything: no read can succeed until the cable moves.
        if self.snapshot.needs_replug {
            egui::Frame::NONE
                .fill(crate::design::paint::with_alpha(NEON_MAGENTA, 0.14))
                .stroke(Stroke::new(1.0, NEON_MAGENTA))
                .corner_radius(CornerRadius::same(3))
                .inner_margin(egui::Margin::symmetric(12, 10))
                .show(ui, |ui| {
                    ui.label(
                        RichText::new("The Zune is wedged — unplug it and plug it back in")
                            .color(NEON_ORANGE)
                            .strong(),
                    );
                    ui.label(
                        RichText::new(
                            "A USB reset only re-enumerates the port; the firmware needs the cable \
                             physically removed. Unplug the Zune, wait about 3 seconds, plug it \
                             back in, then press Connect.",
                        )
                        .font(body_sm())
                        .color(TEXT_2),
                    );
                });
            ui.add_space(SP_8);
        }
        let Some(error) = self.snapshot.key_error.clone() else { return };
        egui::Frame::NONE
            .fill(RED.gamma_multiply(0.18))
            .stroke(Stroke::new(1.0, RED))
            .corner_radius(CornerRadius::same(3))
            .inner_margin(egui::Margin::symmetric(12, 10))
            .show(ui, |ui| {
                ui.label(RichText::new("MTPZ key file missing — the device will not serve its library").color(RED).strong());
                ui.label(
                    RichText::new(
                        "Put the 5-line hex key file where the client looks for it, then press Connect:",
                    )
                    .color(TEXT),
                );
                for candidate in worker::key_candidates() {
                    ui.label(RichText::new(format!("  {}\n", candidate.display())).monospace().color(MUTED));
                }
                ui.collapsing("details", |ui| {
                    ui.label(RichText::new(error).monospace().size(11.0).color(MUTED));
                });
            });
        ui.add_space(6.0);
    }

    fn empty_hint(&mut self, ui: &mut egui::Ui, text: &str) {
        ui.vertical_centered(|ui| {
            ui.add_space(30.0);
            ui.label(RichText::new(text).color(MUTED).size(15.0));
        });
    }

    fn tracks_page(&mut self, ui: &mut egui::Ui) {
        self.banner(ui);
        let filtered = self.album_filter.is_some() || !self.search.trim().is_empty();
        ui.horizontal(|ui| {
            // No "Search" caption: the hint text already says what the field takes, and a
            // label beside a box that has a label is one label too many.
            text_field(ui, &mut self.search, "search title, artist, album or file", 300.0);
            if filtered && quiet_button(ui, "Clear filter", TEXT_2).clicked() {
                self.search.clear();
                self.album_filter = None;
            }
            if let Some(album) = self.album_filter.clone() {
                theme::tag(ui, &format!("album: {album}"), ORANGE);
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                // Mono, and out of the way: the sort state is a fact about the table, not
                // a control, and the header already shows which column is active.
                let arrow = if self.sort_desc { "↓" } else { "↑" };
                ui.label(
                    RichText::new(format!("{arrow} {}", self.sort.label()))
                        .font(mono(11.0))
                        .color(TEXT_3),
                );
            });
        });
        ui.add_space(4.0);

        if self.tracks.is_empty() {
            let hint = if self.link == Link::Connected {
                "No tracks on the device yet — use the Push page, or drag files onto this window."
            } else {
                "Not connected. Plug in the Zune; the client reconnects on its own."
            };
            self.empty_hint(ui, hint);
            return;
        }

        let rows = self.visible_rows();

        // Header row: click to sort.
        ui.horizontal(|ui| {
            for (index, (width, label)) in theme::COLS.iter().enumerate() {
                let key = match index {
                    0 => None,
                    1 => Some(SortKey::Track),
                    2 => Some(SortKey::Title),
                    3 => Some(SortKey::Artist),
                    4 => Some(SortKey::Album),
                    5 => Some(SortKey::Duration),
                    _ => Some(SortKey::Size),
                };
                // Numeric columns are monospaced and right-aligned so their digits line up
                // down the column; the text columns take the tracked uppercase treatment the
                // rest of the chrome uses. Sorting is the only thing these labels do, so the
                // active one is the only one that gets a colour.
                let numeric = matches!(index, 1 | 5 | 6);
                let colour = match key {
                    Some(key) if self.sort == key => ORANGE,
                    _ => TEXT_3,
                };
                let text = if numeric {
                    RichText::new(tracked(&label.to_uppercase())).font(mono(10.0)).color(colour)
                } else {
                    RichText::new(tracked(&label.to_uppercase())).size(10.0).color(colour)
                };
                match key {
                    Some(key) => {
                        let clicked = ui
                            .allocate_ui_with_layout(
                                Vec2::new(*width, 18.0),
                                if numeric {
                                    Layout::right_to_left(Align::Center)
                                } else {
                                    Layout::left_to_right(Align::Center)
                                },
                                |ui| ui.add(egui::Button::new(text).frame(false)),
                            )
                            .inner
                            .clicked();
                        if clicked {
                            if self.sort == key {
                                self.sort_desc = !self.sort_desc;
                            } else {
                                self.sort = key;
                                self.sort_desc = false;
                            }
                        }
                    }
                    None => {
                        ui.add_sized([*width, 18.0], egui::Label::new(""));
                    }
                }
            }
        });
        ui.separator();

        let mut toggle: Option<(u32, bool)> = None;
        let total_height = ui.available_height() - 30.0;
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .max_height(total_height)
            .show_rows(ui, theme::ROW_HEIGHT, rows.len(), |ui, range| {
                for index in range {
                    let row = &rows[index];
                    let selected = self.selected.contains(&row.handle);
                    // Reserve the row's fill and marker slots first. egui paints in call
                    // order, so a background drawn after the widgets would sit on top of
                    // them; setting a deferred `Shape::Noop` slot is the one-pass way to
                    // get it underneath.
                    let fill = ui.painter().add(egui::Shape::Noop);
                    let marker = ui.painter().add(egui::Shape::Noop);

                    let inner = ui.allocate_ui_with_layout(
                        Vec2::new(ui.available_width(), theme::ROW_HEIGHT),
                        Layout::left_to_right(Align::Center),
                        |ui| {
                        // A stock checkbox put a grey disc on each of 2425 rows. A dot that
                        // is barely there until you reach for it keeps the column scannable
                        // and still makes selection discoverable.
                        let (dot, dot_response) =
                            ui.allocate_exact_size(Vec2::splat(18.0), Sense::click());
                        let center = dot.center();
                        let painter = ui.painter();
                        if selected {
                            painter.circle_filled(center, 5.5, ORANGE);
                            let s = 2.2;
                            painter.add(egui::Shape::line(
                                vec![
                                    center + Vec2::new(-s, 0.0),
                                    center + Vec2::new(-s * 0.2, s * 0.8),
                                    center + Vec2::new(s * 1.15, -s * 0.85),
                                ],
                                Stroke::new(1.6, CANVAS),
                            ));
                        } else if dot_response.hovered() {
                            painter.circle_stroke(center, 5.5, Stroke::new(1.0, TEXT_2));
                        } else {
                            painter.circle_filled(
                                center,
                                1.8,
                                paint::with_alpha(Color32::WHITE, 0.20),
                            );
                        }
                        if dot_response.clicked() {
                            toggle = Some((row.handle, !selected));
                        }
                        // Widths come from the same table the header uses, so a column can
                        // only be resized in one place. Three levels of brightness: the title
                        // is what you read, the artist is what you scan, the rest is reference.
                        let number = row
                            .track_no
                            .map(|n| n.to_string())
                            .unwrap_or_else(|| "—".to_string());
                        cell_right(ui, theme::COLS[1].0, RichText::new(number).font(mono(11.0)).color(TEXT_3));
                        let title = if selected {
                            RichText::new(&row.title).color(ORANGE)
                        } else {
                            RichText::new(&row.title).color(TEXT_1)
                        };
                        cell(ui, theme::COLS[2].0, title, false);
                        cell(
                            ui,
                            theme::COLS[3].0,
                            RichText::new(if row.artist.is_empty() { "—" } else { &row.artist })
                                .color(TEXT_2),
                            false,
                        );
                        cell(
                            ui,
                            theme::COLS[4].0,
                            RichText::new(if row.album.is_empty() { "—" } else { &row.album })
                                .color(TEXT_3),
                            false,
                        );
                        cell_right(ui, theme::COLS[5].0, RichText::new(&row.length).font(mono(11.0)).color(TEXT_3));
                        cell_right(
                            ui,
                            theme::COLS[6].0,
                            RichText::new(theme::human_bytes(row.size as u64)).font(mono(11.0)).color(TEXT_3),
                        );
                        },
                    );

                    // The rect comes from the layout (accurate for any row height the content
                    // ends up with); the hover test comes from the pointer, because egui's own
                    // hover state for a row inside a virtualised list cannot be trusted — its
                    // response id is shared and only the last row in the frame would ever win.
                    let rect = inner.response.rect.expand2(Vec2::new(6.0, 2.0));
                    let hovering = ui
                        .ctx()
                        .pointer_latest_pos()
                        .is_some_and(|pos| rect.contains(pos));
                    if selected {
                        ui.painter().set(
                            fill,
                            egui::Shape::rect_filled(
                                rect,
                                r_sm(),
                                paint::with_alpha(ORANGE, 0.10),
                            ),
                        );
                    } else if hovering {
                        ui.painter().set(
                            fill,
                            egui::Shape::rect_filled(
                                rect,
                                r_sm(),
                                paint::with_alpha(SURFACE_2, 0.75),
                            ),
                        );
                    }
                    if selected || hovering {
                        // The signature marker, at the row's own height.
                        let bar = Rect::from_min_size(
                            egui::pos2(rect.left(), rect.center().y - 8.0),
                            Vec2::new(2.0, 16.0),
                        );
                        ui.painter().set(
                            marker,
                            egui::Shape::rect_filled(bar, CornerRadius::same(1), ORANGE),
                        );
                    }
                }
            });
        if let Some((handle, checked)) = toggle {
            if checked {
                self.selected.insert(handle);
            } else {
                self.selected.remove(&handle);
            }
        }

        ui.separator();
        ui.horizontal(|ui| {
            let bytes: u64 = rows.iter().map(|r| r.size as u64).sum();
            let selected = self.selected.len();
            // The same value/unit split as the rail: figures are what you look for, the
            // words beside them are what you read once.
            stat_inline(ui, &thousands(rows.len() as u64), "shown", TEXT_1);
            ui.add_space(SP_16);
            stat_inline(ui, &theme::human_bytes(bytes), "", TEXT_1);
            ui.add_space(SP_16);
            // Selection is a state, so it gets the accent — but only when there is one.
            stat_inline(
                ui,
                &thousands(selected as u64),
                "selected",
                if selected > 0 { NEON_ORANGE } else { TEXT_3 },
            );
            ui.add_space(SP_24);
            if quiet_button(ui, "Select all shown", TEXT_2).clicked() {
                for row in &rows {
                    self.selected.insert(row.handle);
                }
            }
            if quiet_button(ui, "Select none", TEXT_2).clicked() {
                self.selected.clear();
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let enabled =
                    !self.selected.is_empty() && !self.busy && self.link == Link::Connected;
                // Kept visible while disabled rather than hidden: the action exists, it
                // just needs a selection first.
                let clicked = ui
                    .add_enabled_ui(enabled, |ui| quiet_button(ui, "Delete selected", RED))
                    .inner
                    .clicked();
                if clicked {
                    self.confirm_delete = Some(self.selected.iter().copied().collect());
                }
            });
        });
    }

    fn albums_page(&mut self, ui: &mut egui::Ui, motion: &mut Motion) {
        self.banner(ui);
        if let Some(name) = self.album_open.clone() {
            self.album_detail(ui, motion, &name);
            return;
        }
        let mut diagnose = false;
        let mut diagnose_covers = false;
        let mut refresh = false;
        ui.horizontal(|ui| {
            stat_inline(ui, &thousands(self.albums.len() as u64), "albums", TEXT_1);
            ui.add_space(SP_16);
            // No "sort" caption: three buttons of which one is orange explains itself.
            for key in AlbumSort::ALL {
                if quiet_toggle(ui, key.label(), self.album_sort == key).clicked() {
                    self.album_sort = key;
                }
            }
            ui.add_space(SP_12);
            text_field(ui, &mut self.album_search, "filter albums", 200.0);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                // Deliberately *not* gated on `busy`: pressing this stops the library
                // read early so the answer arrives in seconds instead of minutes.
                let covers = ui
                    .add_enabled_ui(self.link == Link::Connected, |ui| {
                        quiet_button(ui, "Diagnose covers", TEXT_2)
                    })
                    .inner
                    .on_hover_text(
                        "Fast: reads only the image objects and a few albums (seconds, not minutes) and \
                         writes the real association structure to the Log page",
                    );
                if covers.clicked() {
                    diagnose_covers = true;
                }
                let albums_diag = ui
                    .add_enabled_ui(self.link == Link::Connected, |ui| {
                        quiet_button(ui, "Diagnose albums", TEXT_2)
                    })
                    .inner
                    .on_hover_text(
                        "Dump the real reference/property structure of the first albums to the Log page",
                    );
                if albums_diag.clicked() {
                    diagnose = true;
                }
                let refresh_button = ui
                    .add_enabled_ui(!self.busy && self.link == Link::Connected, |ui| {
                        quiet_button(ui, "Refresh", TEXT_2)
                    })
                    .inner
                    .on_hover_text("Re-read the device now and overwrite the cached library");
                if refresh_button.clicked() {
                    refresh = true;
                }
            });
        });
        if diagnose_covers {
            self.worker.clear_cancel();
            // Stop the (possibly minutes-long) library read so this runs now.
            self.worker.interrupt_library_read();
            self.worker.send(Command::DiagnoseCovers { images: 3, albums: 3 });
            self.page = Page::Log;
        }
        if diagnose {
            self.worker.clear_cancel();
            self.worker.interrupt_library_read();
            self.worker.send(Command::DiagnoseAlbums(6));
            self.page = Page::Log;
        }
        if refresh {
            self.worker.clear_cancel();
            let force = ui.input(|i| i.modifiers.shift);
            self.worker.send(Command::Refresh { force });
        }
        ui.add_space(SP_12);

        if self.albums.is_empty() {
            let hint = if self.link != Link::Connected {
                "Not connected. Plug in the Zune; the client reconnects on its own."
            } else if self.busy {
                "Still reading the device — this takes minutes for a large library. \
                 \"Diagnose covers\" above answers immediately and does not wait for it."
            } else {
                "No albums on the device yet — or the last read failed; press Refresh, \
                 or Diagnose covers to see what the device actually holds."
            };
            self.empty_hint(ui, hint);
            return;
        }

        // Assemble the tiles before drawing: the cover state lives in `self`, and the
        // grid must not hold a borrow of it while more covers are requested.
        let rows = self.album_rows();
        if rows.is_empty() {
            ui.label(RichText::new(format!("no album matches \"{}\"", self.album_search)).color(MUTED));
            return;
        }
        struct Tile {
            name: String,
            subtitle: String,
            handle: Option<u32>,
            /// The map key: the device handle, or a synthetic album key.
            key: u32,
            artist: String,
            /// The album's first track, for the embedded-ID3 fallback.
            track_handle: Option<u32>,
            cover: Option<egui::TextureId>,
            badge: CoverBadge,
        }
        let mut tiles: Vec<Tile> = Vec::with_capacity(rows.len());
        for (album, subtitle) in &rows {
            // The device is not the only source: with no art on the device, the tile
            // shows whatever the fallback ladder found (cache → local file → iTunes).
            let (cover, badge) = match self.covers.get(&album.cover_key) {
                Some(CoverState::Ready(texture)) => (Some(texture.id()), CoverBadge::Ready),
                Some(CoverState::Failed(reason)) => (None, CoverBadge::Failed(reason.clone())),
                Some(CoverState::Loading) | None => {
                    if album.cover_handle.is_some() {
                        (None, CoverBadge::Loading)
                    } else if self.cover_origins.contains_key(&album.cover_key) {
                        (None, CoverBadge::Loading)
                    } else {
                        (None, CoverBadge::Missing)
                    }
                }
            };
            tiles.push(Tile {
                name: album.name.clone(),
                subtitle: subtitle.clone(),
                handle: album.cover_handle,
                key: album.cover_key,
                artist: album.artist.clone(),
                track_handle: album.track_handles.first().copied(),
                cover,
                badge,
            });
        }
        // Request what is not cached yet (cheap to ask; the worker de-duplicates).
        for tile in &tiles {
            if tile.cover.is_some() || matches!(tile.badge, CoverBadge::Failed(_)) {
                continue;
            }
            match tile.handle {
                // Art on the device: download that object.
                Some(handle) => self.request_cover(handle),
                // No art on the device: ask the worker to try the fallback sources.
                None if !self.cover_pending.contains(&tile.key) => {
                    self.cover_pending.insert(tile.key);
                    self.worker.send(Command::FetchFallbackCover {
                        key: tile.key,
                        artist: tile.artist.clone(),
                        album: tile.name.clone(),
                        track_handle: tile.track_handle,
                    });
                }
                None => {}
            }
        }

        // Adaptive columns: at least 160px of cover, never wider than 240px.
        const GAP: f32 = CARD_GAP;
        const MIN_TILE: f32 = 160.0;
        const MAX_TILE: f32 = 240.0;
        let available = ui.available_width();
        let columns = (((available + GAP) / (MIN_TILE + GAP)).floor() as usize).max(1);
        let tile_size = (((available - GAP * (columns - 1) as f32) / columns as f32).floor())
            .clamp(MIN_TILE, MAX_TILE);

        let mut clicked: Option<String> = None;
        {
            let clicked = &mut clicked;
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                ui.spacing_mut().item_spacing = Vec2::new(GAP, GAP);
                ui.horizontal_wrapped(|ui| {
                    for (index, tile) in tiles.iter().enumerate() {
                        let response = AlbumTile::new(&tile.name, &tile.subtitle, tile_size)
                            .cover(tile.cover, tile.badge.clone())
                            .show(ui, motion, index as u64);
                        if response.clicked() {
                            *clicked = Some(tile.name.clone());
                        }
                    }
                });
            });
        }
        if let Some(name) = clicked {
            self.album_open = Some(name);
        }
    }

    /// One album: cover, statistics and its track list.
    fn album_detail(&mut self, ui: &mut egui::Ui, motion: &mut Motion, name: &str) {
        let Some(album) = self.albums.iter().find(|a| a.name == name).cloned() else {
            self.album_open = None;
            return;
        };

        ui.horizontal(|ui| {
            if ui.button("← All albums").clicked() {
                self.album_open = None;
            }
            ui.label(RichText::new(tracked("album")).font(label()).color(NEON_ORANGE));
        });
        ui.add_space(SP_12);

        // Cover + headline statistics.
        let cover_side = 180.0;
        let (cover_rect, _) = ui.allocate_exact_size(Vec2::splat(cover_side), Sense::hover());
        let (texture, badge) = match self.covers.get(&album.cover_key) {
            Some(CoverState::Ready(texture)) => (Some(texture.id()), CoverBadge::Ready),
            Some(CoverState::Failed(reason)) => (None, CoverBadge::Failed(reason.clone())),
            Some(CoverState::Loading) | None => (None, CoverBadge::Loading),
        };
        if texture.is_none() && !matches!(badge, CoverBadge::Failed(_)) {
            match album.cover_handle {
                Some(handle) => self.request_cover(handle),
                None if !self.cover_pending.contains(&album.cover_key) => {
                    self.cover_pending.insert(album.cover_key);
                    self.worker.send(Command::FetchFallbackCover {
                        key: album.cover_key,
                        artist: album.artist.clone(),
                        album: album.name.clone(),
                        track_handle: album.track_handles.first().copied(),
                    });
                }
                None => {}
            }
        }
        {
            let painter = ui.painter();
            match texture {
                Some(texture) => {
                    paint::rounded_image(painter, texture, cover_rect, r_md(), CANVAS);
                    paint::hairline(painter, cover_rect, r_md());
                }
                None => {
                    painter.rect_filled(cover_rect, r_md(), CANVAS);
                    let initial = album
                        .name
                        .chars()
                        .next()
                        .map(|c| c.to_uppercase().to_string())
                        .unwrap_or_else(|| "?".to_owned());
                    let galley = painter.layout_no_wrap(
                        initial,
                        egui::FontId::new(cover_side * 0.42, egui::FontFamily::Name(F_XLIGHT.into())),
                        NEON_ORANGE,
                    );
                    let pos = cover_rect.center() - galley.size() / 2.0;
                    paint::glow_galley(painter, pos, &galley, NEON_ORANGE, GLOW_TEXT, GlowResponse::IDLE);
                    painter.galley(pos, galley, NEON_ORANGE);
                    paint::hairline(painter, cover_rect, r_md());
                }
            }
            if let CoverBadge::Failed(reason) = &badge {
                painter.text(
                    cover_rect.center_bottom() - Vec2::new(0.0, 8.0),
                    egui::Align2::CENTER_BOTTOM,
                    "cover unavailable",
                    body_sm(),
                    NEON_ORANGE,
                );
                let _ = reason;
            }
        }

        let info_rect = egui::Rect::from_min_size(
            egui::pos2(cover_rect.right() + SP_24, cover_rect.top()),
            Vec2::new(ui.available_width() - cover_side - SP_24, cover_side),
        );
        {
            let painter = ui.painter();
            let name_galley = painter.layout(
                self.album_open.clone().unwrap_or_default(),
                h1(),
                TEXT_1,
                info_rect.width().max(120.0),
            );
            painter.galley(info_rect.left_top(), name_galley.clone(), TEXT_1);
            let mut y = info_rect.top() + name_galley.size().y + SP_8;
            if !album.artist.is_empty() {
                painter.text(
                    egui::pos2(info_rect.left(), y),
                    egui::Align2::LEFT_TOP,
                    &album.artist,
                    body(),
                    TEXT_2,
                );
                y += 22.0;
            }
            let year = album
                .year
                .map(|y| format!("{y}  ·  "))
                .unwrap_or_default();
            painter.text(
                egui::pos2(info_rect.left(), y),
                egui::Align2::LEFT_TOP,
                format!(
                    "{year}{} 首  ·  {}  ·  {}",
                    album.tracks,
                    duration_label(album.duration),
                    theme::human_bytes(album.bytes)
                ),
                mono(BODY_SM),
                NEON_ORANGE,
            );
            y += 26.0;
            let origin = self
                .cover_origins
                .get(&album.cover_key)
                .cloned()
                .unwrap_or_else(|| album.cover_source.label().to_owned());
            painter.text(
                egui::pos2(info_rect.left(), y),
                egui::Align2::LEFT_TOP,
                format!("cover art: {origin}"),
                body_sm(),
                TEXT_3,
            );
            if let Some(handle) = album.cover_handle {
                painter.text(
                    egui::pos2(info_rect.left(), y + 18.0),
                    egui::Align2::LEFT_TOP,
                    format!("object {}", library::hex(handle)),
                    mono(11.0),
                    TEXT_3,
                );
            }
            if let CoverBadge::Failed(reason) = &badge {
                painter.text(
                    egui::pos2(info_rect.left(), y + 36.0),
                    egui::Align2::LEFT_TOP,
                    format!("download failed: {reason}"),
                    mono(11.0),
                    NEON_ORANGE,
                );
            }
        }
        ui.allocate_space(Vec2::new(0.0, cover_side - ui.min_rect().height().min(0.0)));

        ui.add_space(SP_24);
        ui.horizontal(|ui| {
            ui.label(RichText::new(tracked("tracks")).font(label()).color(TEXT_3));
            ui.label(
                RichText::new(format!("{} tracks on the device", album.tracks)).color(MUTED).size(11.0),
            );
        });
        ui.add_space(SP_8);

        // The album's tracks, in album order (the device's own numbering).
        let mut rows: Vec<&Track> = self
            .tracks
            .iter()
            .filter(|t| album.track_handles.contains(&t.handle))
            .collect();
        rows.sort_by_key(|t| (t.track_no.unwrap_or(u16::MAX), t.title.clone()));

        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            if rows.is_empty() {
                ui.label(
                    RichText::new("this album's objects are not in the track list yet — press Refresh")
                        .color(MUTED),
                );
            }
            for (index, track) in rows.iter().enumerate() {
                let subtitle = if track.artist.is_empty() {
                    track.album.clone()
                } else {
                    track.artist.clone()
                };
                track_row(
                    ui,
                    motion,
                    index as u64,
                    TrackRow::new(track.display_title(), &subtitle)
                        .index(track.track_no.unwrap_or(index as u16 + 1) as u32)
                        .trailing(&track.duration_label())
                        .accent(NEON_ORANGE),
                );
            }
        });
    }

    fn push_page(&mut self, ui: &mut egui::Ui) {
        self.banner(ui);
        ui.label(RichText::new("Drag files or folders onto this window, or type a path.").color(MUTED));
        ui.horizontal(|ui| {
            text_field(ui, &mut self.path_input, "/Users/you/Music/Album", 420.0);
            if ui.button("Add").clicked() {
                let raw = self.path_input.trim().trim_matches('"').to_string();
                if !raw.is_empty() {
                    self.add_to_queue(PathBuf::from(raw));
                    self.path_input.clear();
                }
            }
            if ui.button("Clear queue").clicked() {
                self.queue.clear();
            }
        });
        ui.add_space(6.0);

        egui::Frame::NONE
            .fill(PANEL)
            .stroke(Stroke::new(1.0, LINE))
            .corner_radius(CornerRadius::same(3))
            .inner_margin(egui::Margin::same(10))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("QUEUE").color(MUTED).size(11.0).strong());
                    ui.label(RichText::new(format!("{} entries", self.queue.len())).color(MUTED).size(11.0));
                });
                ui.add_space(4.0);
                if self.queue.is_empty() {
                    ui.label(RichText::new("empty").color(MUTED));
                }
                let mut remove: Option<usize> = None;
                let total = self.queue.len();
                egui::ScrollArea::vertical()
                    .id_salt("queue")
                    .max_height(150.0)
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        for (index, path) in self.queue.iter().enumerate() {
                            ui.horizontal(|ui| {
                                let kind = if path.is_dir() {
                                    let count = crate::core::library::audio_files(path).map(|f| f.len()).unwrap_or(0);
                                    format!("folder · {count} audio files")
                                } else {
                                    let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
                                    format!("file · {}", theme::human_bytes(size))
                                };
                                ui.add_sized(
                                    [430.0, 18.0],
                                    egui::Label::new(RichText::new(path.display().to_string()).monospace().size(12.0)).truncate(),
                                );
                                ui.label(RichText::new(kind).color(MUTED).size(11.0));
                                if ui.small_button("remove").clicked() {
                                    remove = Some(index);
                                }
                            });
                            let _ = total;
                        }
                    });
                if let Some(index) = remove {
                    self.queue.remove(index);
                }
            });

        ui.add_space(10.0);
        ui.horizontal(|ui| {
            ui.checkbox(&mut self.album_mode, "Push as one album");
            ui.label(
                RichText::new("(tracks + .pla playlist + cover art + abstract album)")
                    .color(MUTED)
                    .size(11.0),
            );
        });
        if self.album_mode {
            ui.horizontal(|ui| {
                ui.label(RichText::new("Album title").color(TEXT_2));
                text_field(
                    ui,
                    &mut self.album_name,
                    "(default: folder name / first track's album tag)",
                    300.0,
                );
                ui.checkbox(&mut self.make_playlist, "create playlist");
            });
            ui.horizontal(|ui| {
                ui.label(RichText::new("Cover art").color(TEXT_2));
                text_field(
                    ui,
                    &mut self.cover_path,
                    "(default: largest JPEG in the folder)",
                    380.0,
                );
            });
        }

        ui.add_space(12.0);
        ui.horizontal(|ui| {
            let can_push = !self.queue.is_empty() && !self.busy && self.link == Link::Connected;
            if ui
                .add_enabled(
                    can_push,
                    egui::Button::new(RichText::new("Push to Zune").color(ORANGE).strong())
                        .min_size(Vec2::new(160.0, 32.0)),
                )
                .clicked()
            {
                self.worker.clear_cancel();
                if self.album_mode {
                    match self.queue.iter().find(|p| p.is_dir()) {
                        Some(dir) => {
                            let cover = {
                                let raw = self.cover_path.trim().trim_matches('"').to_string();
                                if raw.is_empty() { None } else { Some(PathBuf::from(raw)) }
                            };
                            self.worker.send(Command::PushAlbum {
                                dir: dir.clone(),
                                name: Some(self.album_name.clone()).filter(|n| !n.trim().is_empty()),
                                cover,
                                playlist: self.make_playlist,
                            });
                        }
                        None => {
                            self.logs.push(LogLine::error(
                                "Album mode needs a folder in the queue",
                            ));
                        }
                    }
                } else {
                    self.worker.send(Command::PushTracks(self.queue.clone()));
                }
            }
            if self.busy && ui.button("Stop after current").clicked() {
                self.worker.request_cancel();
            }
            if !can_push && !self.busy {
                ui.label(
                    RichText::new(if self.link != Link::Connected {
                        "connect a Zune first"
                    } else {
                        "add something to the queue first"
                    })
                    .color(MUTED)
                    .size(11.0),
                );
            }
        });

        if let Some(progress) = &self.progress {
            ui.add_space(10.0);
            ui.add(
                egui::ProgressBar::new(progress.fraction())
                    .desired_width(520.0)
                    .fill(ORANGE)
                    .text(format!("{:.0}%  {}", progress.fraction() * 100.0, progress.step)),
            );
        }
    }

    fn device_page(&mut self, ui: &mut egui::Ui) {
        self.banner(ui);
        let snapshot = self.snapshot.clone();
        egui::Frame::NONE
            .fill(PANEL)
            .corner_radius(CornerRadius::same(3))
            .inner_margin(egui::Margin::same(14))
            .show(ui, |ui| {
                egui::Grid::new("identity")
                    .num_columns(2)
                    .spacing([24.0, 6.0])
                    .show(ui, |ui| {
                        let mut kv = |k: &str, v: String| {
                            ui.label(RichText::new(k).color(MUTED));
                            ui.label(RichText::new(v).monospace());
                            ui.end_row();
                        };
                        let dash = |s: &str| if s.is_empty() { "—".to_string() } else { s.to_string() };
                        kv("Model", dash(&snapshot.name));
                        kv("Manufacturer", dash(&snapshot.manufacturer));
                        kv("Serial", dash(&snapshot.serial));
                        kv("Firmware", dash(&snapshot.firmware));
                        kv("MTP version", dash(&snapshot.mtp_version));
                        kv("USB topology", dash(&snapshot.topology));
                        kv("Track formats", dash(&snapshot.format_summary));
                    });
            });

        ui.add_space(12.0);
        ui.label(RichText::new("STORAGE").color(MUTED).size(11.0).strong());
        match (snapshot.used(), snapshot.capacity, snapshot.free) {
            (Some(used), Some(capacity), Some(free)) if capacity > 0 => {
                let fraction = used as f32 / capacity as f32;
                ui.add(
                    egui::ProgressBar::new(fraction)
                        .desired_width(520.0)
                        .fill(ORANGE)
                        .text(format!(
                            "{} used · {} free of {}",
                            theme::human_bytes(used),
                            theme::human_bytes(free),
                            theme::human_bytes(capacity)
                        )),
                );
                if !snapshot.storage_desc.is_empty() {
                    ui.label(RichText::new(&snapshot.storage_desc).color(MUTED).size(11.0));
                }
            }
            _ => {
                ui.label(
                    RichText::new(
                        "The device did not report storage capacity (GetStorageInfo 0x1005 is optional).",
                    )
                    .color(MUTED),
                );
            }
        }

        ui.add_space(12.0);
        ui.label(RichText::new("MTPZ KEYS").color(MUTED).size(11.0).strong());
        match (&snapshot.key_path, &snapshot.key_error) {
            (Some(path), None) => {
                ui.label(RichText::new(format!("loaded from {path}")).color(GREEN));
            }
            (_, Some(error)) => {
                ui.label(RichText::new("no key file — the library is unavailable").color(RED).strong());
                for candidate in worker::key_candidates() {
                    ui.label(RichText::new(candidate.display().to_string()).monospace().color(MUTED).size(11.0));
                }
                ui.collapsing("error detail", |ui| {
                    ui.label(RichText::new(error).monospace().size(11.0));
                });
            }
            _ => {
                ui.label(RichText::new("not checked yet").color(MUTED));
            }
        }

        ui.add_space(14.0);
        ui.horizontal(|ui| {
            let connected = self.link == Link::Connected;
            if ui.add_enabled(!self.busy, egui::Button::new("Reset USB link")).clicked() {
                self.worker.send(Command::UsbReset);
            }
            if ui.add_enabled(connected && !self.busy, egui::Button::new("Eject")).clicked() {
                self.worker.send(Command::Eject);
            }
            if ui.add_enabled(!self.busy, egui::Button::new("Reconnect")).clicked() {
                self.worker.send(Command::Connect);
            }
        });
        ui.label(
            RichText::new(
                "A Zune left mid-transfer refuses new commands until its stale response is drained; \
                 if transfers time out, Reset USB is the way back.",
            )
            .color(MUTED)
            .size(11.0),
        );
    }

    fn wifi_page(&mut self, ui: &mut egui::Ui) {
        ui.label(
            RichText::new("Vendor opcodes from the Keel 3.30 firmware (MtpSvc).").color(MUTED),
        );
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            let connected = self.link == Link::Connected && !self.busy;
            if ui.add_enabled(connected, egui::Button::new("Scan networks (0x9225)")).clicked() {
                self.worker.send(Command::WifiScan);
            }
            if ui
                .add_enabled(connected, egui::Button::new("Connectivity test (0x9228)"))
                .clicked()
            {
                self.worker.send(Command::WifiTest);
            }
        });
        if !self.wifi_note.is_empty() {
            ui.label(RichText::new(&self.wifi_note).color(YELLOW));
        }

        // The scan answer, read back as networks rather than as bytes. The device sends a
        // fixed 56-byte record per network (see core::wifi); showing the hex for that was
        // showing the packaging instead of the contents.
        if !self.wifi_networks.is_empty() {
            ui.add_space(12.0);
            crate::ui::section::section_rule(ui, "VISIBLE NETWORKS", Some(&format!("{}", self.wifi_networks.len())));
            ui.add_space(4.0);
            for network in &self.wifi_networks {
                ui.horizontal(|ui| {
                    // A four-slot bar, filled to the network's strength. Glyphs rather
                    // than a painted bar: this row is text-height and the font already
                    // has the blocks.
                    let filled = network.bars();
                    let mut bar = String::new();
                    for slot in 0..4 {
                        bar.push(if slot < filled { '\u{2588}' } else { '\u{2591}' });
                    }
                    let tint = match network.strength {
                        0..=24 => TEXT_3,
                        25..=49 => NEON_AMBER,
                        50..=74 => NEON_ORANGE,
                        _ => NEON_LIME,
                    };
                    ui.label(RichText::new(bar).color(tint).monospace());
                    ui.label(RichText::new(format!("{:>3}%", network.strength)).color(TEXT_2).monospace());
                    ui.label(RichText::new(&network.ssid).color(TEXT_1).strong());
                    ui.label(RichText::new(network.quality()).color(TEXT_3).size(11.0));
                });
            }
            ui.add_space(12.0);
        }
        ui.add_space(8.0);
        ui.label(RichText::new("PROFILE BLOB (hex, device-side WZC/NDIS_802_11 layout)").color(MUTED).size(11.0).strong());
        ui.add(
            egui::TextEdit::multiline(&mut self.wifi_profile_input)
                .desired_rows(3)
                .desired_width(520.0)
                .font(egui::TextStyle::Monospace)
                .hint_text("0102ab…"),
        );
        if ui
            .add_enabled(
                self.link == Link::Connected && !self.busy && !self.wifi_profile_input.trim().is_empty(),
                egui::Button::new("Commit profile (0x9227)"),
            )
            .clicked()
        {
            let cleaned: String = self
                .wifi_profile_input
                .chars()
                .filter(|c| !c.is_whitespace())
                .collect();
            match hex::decode(&cleaned) {
                Ok(blob) if !blob.is_empty() && blob.len() <= 0x1000 => {
                    self.worker.send(Command::WifiProfile(blob));
                }
                Ok(blob) => self.logs.push(LogLine::error(format!(
                    "profile blob must be 1..=4096 bytes (got {})",
                    blob.len()
                ))),
                Err(e) => self.logs.push(LogLine::error(format!("not valid hex: {e}"))),
            }
        }

        ui.add_space(12.0);
        ui.label(RichText::new("LAST SCAN RESULT").color(MUTED).size(11.0).strong());
        egui::ScrollArea::vertical()
            .id_salt("wifi")
            .max_height(240.0)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                if self.wifi_hex.is_empty() {
                    ui.label(RichText::new("(nothing scanned yet)").color(MUTED));
                } else {
                    ui.label(RichText::new(&self.wifi_hex).monospace().size(11.0));
                }
            });
    }

    fn log_page(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if ui.button("Clear").clicked() {
                self.logs.clear();
                self.last_log = None;
            }
            if ui.button("Copy to clipboard").clicked() {
                let text: String = self
                    .logs
                    .iter()
                    .map(|l| format!("{}\n", l.text))
                    .collect();
                ui.ctx().copy_text(text);
            }
            ui.label(RichText::new(format!("{} lines", self.logs.len())).color(MUTED));
        });
        ui.separator();
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .stick_to_bottom(true)
            .show(ui, |ui| {
                for line in &self.logs {
                    ui.label(
                        RichText::new(&line.text)
                            .color(level_color(line.level))
                            .size(12.0),
                    );
                }
            });
    }

    fn confirm_dialog(&mut self, ctx: &egui::Context) {
        let Some(handles) = self.confirm_delete.clone() else { return };
        let titles: Vec<String> = handles
            .iter()
            .filter_map(|h| {
                self.tracks
                    .iter()
                    .find(|t| t.handle == *h)
                    .map(|t| t.display_title().to_string())
            })
            .take(8)
            .collect();
        let response = egui::Modal::new(egui::Id::new("confirm-delete")).show(ctx, |ui| {
            ui.set_width(420.0);
            ui.label(RichText::new(format!("Delete {} track(s) from the Zune?", handles.len())).size(18.0));
            ui.add_space(4.0);
            for title in &titles {
                ui.label(RichText::new(format!("  • {title}")).color(MUTED));
            }
            if handles.len() > titles.len() {
                ui.label(RichText::new(format!("  … and {} more", handles.len() - titles.len())).color(MUTED));
            }
            ui.add_space(6.0);
            ui.label(RichText::new("This deletes them from the device; it cannot be undone.").color(RED));
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                if ui.button("Cancel").clicked() {
                    self.confirm_delete = None;
                }
                if ui
                    .add(egui::Button::new(RichText::new("Delete").color(RED).strong()))
                    .clicked()
                {
                    self.confirm_delete = None;
                    self.selected.clear();
                    self.worker.clear_cancel();
                    self.worker.send(Command::Delete(handles.clone()));
                }
            });
        });
        if response.should_close() {
            self.confirm_delete = None;
        }
    }
}

impl eframe::App for ZuneApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.drain_events(ui.ctx());
        self.take_dropped_files(ui.ctx());

        // Dev affordance: one scan, once, as soon as there is a device to ask.
        if self.wifi_autoscan && self.link == Link::Connected && !self.busy {
            self.wifi_autoscan = false;
            self.worker.send(Command::WifiScan);
        }
        // Dev affordance: one refresh, once, so the quick change check can be exercised
        // without racing another window for the pointer.
        if let Some(force) = self.refresh_on_start {
            if self.link == Link::Connected {
                self.refresh_on_start = None;
                self.worker.send(Command::Refresh { force });
            }
        }

        // One motion handle per frame: it is the only thing that may ask for
        // another repaint, which is what keeps an idle window at 0 % CPU.
        let mut motion = Motion::begin(ui.ctx());

        self.top_bar(ui, &mut motion);
        self.status_bar(ui);
        self.side_bar(ui);
        egui::CentralPanel::default()
            // Extra room at the top: the 88 pt title's ascenders start immediately below
            // the hairline otherwise, and the page reads as cramped against the chrome.
            .frame(
                egui::Frame::central_panel(ui.style())
                    .inner_margin(egui::Margin { left: 16, right: 16, top: 24, bottom: 16 }),
            )
            .show(ui, |ui| {
                let (title, rule) = match self.page {
                    // Its rail label is "Design" but its heading is not.
                    Page::Board => ("Design System", true),
                    other => (other.title(), false),
                };
                self.page_header(ui, title, rule);
                match self.page {
                    Page::Home => self.home_page(ui, &mut motion),
                    Page::Board => board::board(ui, &mut motion, &mut self.board),
                    Page::Tracks => self.tracks_page(ui),
                    Page::Albums => self.albums_page(ui, &mut motion),
                    Page::Artists => self.artists_page(ui, &mut motion),
                    Page::Push => self.push_page(ui),
                    Page::Device => self.device_page(ui),
                    Page::Wifi => self.wifi_page(ui),
                    Page::Log => self.log_page(ui),
                }
            });
        self.confirm_dialog(ui.ctx());
        motion.finish(ui.ctx());
        self.screenshot_tick(ui.ctx());
    }
}

/// The Zune mark: a ring with the bar through it, which is what makes it a Zune and not
/// a record. This is the one piece of chrome that keeps a fixed colour — it is the brand,
/// so it stays orange while everything around it takes the page's signature.
/// Write an egui framebuffer out as a PNG.
///
/// `image` is already a dependency for decoding cover art, so the screenshot path costs
/// nothing new — and going through it gets 8-bit RGBA conversion and PNG encoding right
/// for free.
fn save_png(path: &std::path::Path, image: &egui::ColorImage) -> Result<()> {
    let [width, height] = image.size;
    let mut rgba = Vec::with_capacity(width * height * 4);
    for pixel in &image.pixels {
        rgba.extend_from_slice(&pixel.to_array());
    }
    let buffer = image::RgbaImage::from_raw(width as u32, height as u32, rgba)
        .context("the framebuffer did not match its own dimensions")?;
    buffer
        .save(path)
        .with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

/// A navigation entry on the home page: a small tile with its glyph.
///
/// These are the only controls on the page that *go* somewhere, so unlike the row-level
/// `quiet_button`s they get a surface and an edge — and the edge lights orange on hover,
/// the same "this is what you are about to touch" the album tiles use.
fn nav_chip(
    ui: &mut egui::Ui,
    icon: crate::ui::icons::Icon,
    text: &str,
) -> egui::Response {
    let galley = ui.painter().layout_no_wrap(text.to_owned(), body_sm(), TEXT_1);
    let size = Vec2::new(galley.size().x + 56.0, 34.0);
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    let hovered = response.hovered();
    let colour = if hovered { NEON_ORANGE } else { TEXT_1 };
    let painter = ui.painter();
    painter.rect_filled(rect, r_sm(), if hovered { SURFACE_2 } else { SURFACE_1 });
    if hovered {
        paint::hairline_lit(painter, rect, r_sm(), 1.0, NEON_ORANGE);
    } else {
        paint::hairline(painter, rect, r_sm());
    }
    icon.draw(
        painter,
        egui::Rect::from_center_size(
            egui::pos2(rect.left() + 18.0, rect.center().y),
            Vec2::splat(15.0),
        ),
        colour,
    );
    let galley = painter.layout_no_wrap(text.to_owned(), body_sm(), colour);
    painter.galley(
        egui::pos2(rect.left() + 34.0, rect.center().y - galley.size().y / 2.0),
        galley,
        colour,
    );
    response
}

/// A text input: a dark well with a hairline edge, and the accent only while focused.
///
/// egui's default text field is a light-grey box, which on this canvas reads as a hole
/// punched in the page. Every input in the app goes through here.
fn text_field(ui: &mut egui::Ui, value: &mut String, hint: &str, width: f32) {
    ui.scope(|ui| {
        let v = ui.visuals_mut();
        v.extreme_bg_color = SURFACE_1;
        v.widgets.inactive.corner_radius = r_sm();
        v.widgets.hovered.corner_radius = r_sm();
        v.widgets.active.corner_radius = r_sm();
        v.widgets.inactive.bg_stroke = Stroke::new(1.0, HAIRLINE);
        v.widgets.hovered.bg_stroke = Stroke::new(1.0, paint::with_alpha(TEXT_2, 0.55));
        v.widgets.active.bg_stroke = Stroke::new(1.0, NEON_ORANGE);
        ui.add(
            egui::TextEdit::singleline(value)
                .hint_text(RichText::new(hint).color(TEXT_3))
                .desired_width(width)
                .font(body_sm()),
        );
    });
}

/// A button with no chrome until you reach for it.
///
/// Row-level actions — "select all", "clear filter", "diagnose covers" — sit next to
/// content and should not compete with it. A stock button puts a filled box on every one
/// of them; this stays invisible until hovered, and dims rather than disappears when
/// disabled. The `Response` comes back rather than a bool so callers keep `on_hover_text`,
/// which is how the diagnose actions explain themselves.
fn quiet_button(ui: &mut egui::Ui, text: &str, colour: egui::Color32) -> egui::Response {
    quiet_button_styled(ui, text, colour, TEXT_1)
}

/// A quiet button that is one of a set: the active one keeps the accent on hover, because
/// brightening it to white would read as "and now this one is selected".
fn quiet_toggle(ui: &mut egui::Ui, text: &str, active: bool) -> egui::Response {
    if active {
        quiet_button_styled(ui, text, NEON_ORANGE, NEON_ORANGE)
    } else {
        quiet_button_styled(ui, text, TEXT_2, TEXT_1)
    }
}

fn quiet_button_styled(
    ui: &mut egui::Ui,
    text: &str,
    colour: egui::Color32,
    hover_colour: egui::Color32,
) -> egui::Response {
    let enabled = ui.is_enabled();
    let galley = ui.painter().layout_no_wrap(text.to_owned(), body_sm(), colour);
    let size = galley.size() + Vec2::new(18.0, 8.0);
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    let hovered = enabled && response.hovered();
    if hovered {
        ui.painter().rect_filled(rect, r_sm(), SURFACE_2);
    }
    let colour = if !enabled {
        TEXT_3
    } else if hovered {
        hover_colour
    } else {
        colour
    };
    let galley = ui.painter().layout_no_wrap(text.to_owned(), body_sm(), colour);
    ui.painter()
        .galley(rect.center() - galley.size() / 2.0, galley, colour);
    response
}

/// A cell of the track table, aligned to the edge its column is read from.
///
/// `add_sized` centres, which is wrong for every column in a table: a short title floats
/// in the middle of its slot while the one above it starts at the left edge, so nothing
/// lines up. Numbers go right so their digits share a column; text goes left so its
/// first letter does.
fn cell(ui: &mut egui::Ui, width: f32, text: RichText, right: bool) {
    ui.allocate_ui_with_layout(
        Vec2::new(width, 18.0),
        if right {
            Layout::right_to_left(Align::Center)
        } else {
            Layout::left_to_right(Align::Center)
        },
        |ui| {
            // `.truncate()`, not `ui.label()`: without it a long title runs straight over
            // the next column, because nothing is holding it to its own width.
            ui.add(egui::Label::new(text).truncate());
        },
    );
}

/// A right-aligned cell, so digits line up down a numeric column.
fn cell_right(ui: &mut egui::Ui, width: f32, text: RichText) {
    cell(ui, width, text, true);
}

/// A figure with its unit, inline: "2,425 shown". The value carries the row.
fn stat_inline(ui: &mut egui::Ui, value: &str, unit: &str, colour: egui::Color32) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 5.0;
        ui.label(RichText::new(value).size(15.0).color(colour));
        if !unit.is_empty() {
            ui.label(RichText::new(unit).size(11.0).color(MUTED));
        }
    });
}

/// One figure in the left rail: a large value with a small unit after it.
fn rail_stat(ui: &mut egui::Ui, value: &str, unit: &str, colour: egui::Color32) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 5.0;
        ui.label(RichText::new(value).size(19.0).color(colour));
        ui.label(RichText::new(unit).size(11.0).color(MUTED));
    });
    ui.add_space(3.0);
}

/// The Zune mark: a ring with a Z inside it.
///
/// A ring alone reads as a generic record button, which is what this started as. The Z
/// is drawn as three strokes — top bar, diagonal, bottom bar — rather than set as text,
/// so it keeps its proportions at 26 px in the top bar and at any larger size the UI
/// uses later.
fn zune_mark(ui: &mut egui::Ui, size: f32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(size), Sense::hover());
    let painter = ui.painter();
    let center = rect.center();
    let radius = size * 0.46;
    paint::glow_circle(painter, center, radius, NEON_ORANGE, GLOW_SM, GlowResponse::IDLE);
    let stroke = Stroke::new((size * 0.068).max(1.4), NEON_ORANGE);
    painter.circle_stroke(center, radius, stroke);

    // Optically centred inside the ring: a Z sits slightly above the geometric middle.
    let half_w = radius * 0.34;
    let half_h = radius * 0.29;
    let top = center.y - half_h;
    let bottom = center.y + half_h;
    let left = center.x - half_w;
    let right = center.x + half_w;
    painter.line_segment([egui::pos2(left, top), egui::pos2(right, top)], stroke);
    painter.line_segment([egui::pos2(right, top), egui::pos2(left, bottom)], stroke);
    painter.line_segment([egui::pos2(left, bottom), egui::pos2(right, bottom)], stroke);
}

/// Album totals use the same formatting as track rows.
fn duration_label(seconds: u32) -> String {
    format_duration(seconds)
}

/// 2425 → "2,425". The counts here are small enough that a grouping pass is cheaper
/// than a formatting dependency, and the separators are what make a four-digit figure
/// land in a hero slot.
fn thousands(value: u64) -> String {
    let digits = value.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, ch) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index) % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

fn level_color(level: Level) -> Color32 {
    match level {
        Level::Info => TEXT,
        Level::Warn => YELLOW,
        Level::Error => RED,
        Level::Ok => GREEN,
    }
}
