//! The egui front end.
//!
//! The UI thread only ever reads the snapshots the worker hands it and sends
//! [`Command`]s back; it never touches USB.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use eframe::egui::{
    self, Align, Color32, CornerRadius, FontId, Layout, Rect, RichText, Sense, Stroke, Vec2,
};

use crate::cache;
use crate::design::anim::Motion;
use crate::design::paint::{self, GlowResponse, GLOW_TEXT};
use crate::design::tokens::{
    body, body_sm, h1, label, mono, r_md, tracked, BODY_SM, CARD_GAP, CANVAS, F_XLIGHT,
    NEON_CYAN, NEON_MAGENTA, SP_12, SP_24, SP_8, TEXT_1, TEXT_2, TEXT_3,
};
use crate::ui::track_row::{track_row, TrackRow};
use crate::core::library;
use crate::model::*;
use crate::theme::{self, *};
use crate::ui::album_tile::{AlbumTile, CoverBadge};
use crate::ui::board::{self, BoardState};
use crate::worker::{self, Worker};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Page {
    /// P0: the design-system component board (visual acceptance surface).
    Board,
    Tracks,
    Albums,
    Push,
    Device,
    Wifi,
    Log,
}

impl Page {
    const ALL: [Page; 7] = [
        Page::Board,
        Page::Tracks,
        Page::Albums,
        Page::Push,
        Page::Device,
        Page::Wifi,
        Page::Log,
    ];

    fn title(self) -> &'static str {
        match self {
            Page::Board => "Design",
            Page::Tracks => "Collection",
            Page::Albums => "Albums",
            Page::Push => "Push",
            Page::Device => "Device",
            Page::Wifi => "Wi-Fi",
            Page::Log => "Log",
        }
    }

    fn subtitle(self) -> &'static str {
        match self {
            Page::Board => "the design system, every token and component on one page",
            Page::Tracks => "every track on the device",
            Page::Albums => "albums and their cover art",
            Page::Push => "copy music to the Zune",
            Page::Device => "identity, storage and the MTPZ session",
            Page::Wifi => "vendor WiFi opcodes 0x9225 / 0x9227 / 0x9228",
            Page::Log => "everything the device thread reported",
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
    /// Set once the first frame has run (demo mode uses it to wait for the worker).
    warmed_up: bool,
    /// P0 component board demo state.
    board: BoardState,
    confirm_delete: Option<Vec<u32>>,
    wifi_hex: String,
    wifi_profile_input: String,
    wifi_note: String,
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
            // Dev affordance: ZUNE_PAGE=device|tracks|albums|push|wifi|log lets the
            // screenshot tooling park on a page without a click. Default: the board.
            page: match std::env::var("ZUNE_PAGE").ok().as_deref() {
                Some("tracks") => Page::Tracks,
                Some("albums") => Page::Albums,
                Some("push") => Page::Push,
                Some("device") => Page::Device,
                Some("wifi") => Page::Wifi,
                Some("log") => Page::Log,
                _ => Page::Board,
            },
            search: String::new(),
            sort: SortKey::Album,
            sort_desc: false,
            selected: HashSet::new(),
            album_filter: None,
            covers: HashMap::new(),
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
            warmed_up: false,
            board: BoardState::default(),
            confirm_delete: None,
            wifi_hex: String::new(),
            wifi_profile_input: String::new(),
            wifi_note: String::new(),
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
                Event::Cover { handle, image } => {
                    let texture = ctx.load_texture(
                        format!("cover-{handle}"),
                        egui::ColorImage::from_rgba_unmultiplied(
                            [image.width, image.height],
                            &image.rgba,
                        ),
                        egui::TextureOptions::LINEAR,
                    );
                    self.covers.insert(handle, CoverState::Ready(texture));
                    self.cover_pending.remove(&handle);
                }
                Event::CoverFailed { handle, reason } => {
                    // The device refused it or the payload would not decode: say so in
                    // the Log (warn), and let the tile carry the reason on hover.
                    self.logs.push(LogLine::warn(format!(
                        "cover art {handle:#010x} unavailable: {reason}"
                    )));
                    self.last_log = self.logs.last().cloned();
                    self.covers.insert(handle, CoverState::Failed(reason));
                    self.cover_pending.remove(&handle);
                }
                Event::WifiScan { bytes, hex } => {
                    self.wifi_note = format!("scan returned {} bytes", bytes.len());
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

    fn request_cover(&mut self, handle: u32) {
        if self.covers.contains_key(&handle) || self.cover_pending.contains(&handle) {
            return;
        }
        self.cover_pending.insert(handle);
        self.worker.send(Command::FetchCover(handle));
    }

    // ── panels ───────────────────────────────────────────────────────────────

    /// A quiet statement of where the library on screen came from: `cached · 3 min
    /// ago`, or `live`. text-2, because it is a fact about the data, not an alarm.
    fn data_origin_label(&self, ui: &mut egui::Ui) {
        // Nothing to qualify until there is a library (or a device) to describe.
        if self.tracks.is_empty() && self.link != Link::Connected && self.origin.read_at.is_none() {
            return;
        }
        let text = match (self.origin.source, self.origin.read_at) {
            (Source::Cached, Some(at)) => {
                format!("cached · {}", cache::relative_age(at, cache::now_unix()))
            }
            (Source::Cached, None) => "cached".to_owned(),
            (Source::Live, _) => "live".to_owned(),
        };
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

    fn top_bar(&mut self, ui: &mut egui::Ui) {
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
                    theme::wordmark(ui);
                    ui.add_space(6.0);
                    let (color, label) = match self.link {
                        Link::Connected => (GREEN, "connected"),
                        Link::Connecting => (YELLOW, "connecting…"),
                        Link::Disconnected => (RED, "no device"),
                    };
                    theme::dot_label(ui, color, label);

                    if self.link == Link::Connected {
                        ui.label(RichText::new(&self.snapshot.name).strong());
                        if !self.snapshot.serial.is_empty() {
                            ui.label(
                                RichText::new(format!("· {}", self.snapshot.serial))
                                    .color(MUTED)
                                    .monospace(),
                            );
                        }
                    }
                    if self.snapshot.key_error.is_some() {
                        theme::tag(ui, "MTPZ key missing", RED);
                    } else if self.snapshot.key_path.is_some() {
                        theme::tag(ui, "trusted", GREEN);
                    }
                    self.data_origin_label(ui);

                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let connected = self.link == Link::Connected;
                        if ui
                            .add_enabled(!self.busy, egui::Button::new("Reset USB"))
                            .on_hover_text(
                                "Reset and re-enumerate the device — the recovery for a stalled bulk pipe",
                            )
                            .clicked()
                        {
                            self.worker.send(Command::UsbReset);
                        }
                        if ui
                            .add_enabled(connected && !self.busy, egui::Button::new("Eject"))
                            .on_hover_text("Close the MTP session so the Zune can be unplugged")
                            .clicked()
                        {
                            self.worker.send(Command::Eject);
                        }
                        if ui
                            .add_enabled(connected && !self.busy, egui::Button::new("Refresh"))
                            .on_hover_text("Re-read the device now and overwrite the cached library")
                            .clicked()
                        {
                            self.worker.clear_cancel();
                            self.worker.send(Command::Refresh);
                        }
                        if !connected
                            && ui
                                .add_enabled(!self.busy, egui::Button::new("Connect"))
                                .clicked()
                        {
                            self.worker.send(Command::Connect);
                        }
                        if connected {
                            match (self.snapshot.used(), self.snapshot.capacity) {
                                (Some(used), Some(cap)) if cap > 0 => {
                                    ui.label(
                                        RichText::new(format!(
                                            "{} free of {}",
                                            theme::human_bytes(cap - used),
                                            theme::human_bytes(cap)
                                        ))
                                        .color(MUTED),
                                    );
                                }
                                _ => {}
                            }
                        }
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
                ui.add_space(2.0);
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
                            ORANGE,
                            crate::design::paint::GLOW_MD,
                            crate::design::paint::GlowResponse::IDLE,
                        );
                    }
                    let color = if selected { ORANGE } else if response.hovered() { TEXT } else { MUTED };
                    painter.text(
                        egui::pos2(rect.left() + 18.0, rect.center().y),
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
                ui.add_space(8.0);
                ui.label(RichText::new("LIBRARY").color(MUTED).size(11.0).strong());
                ui.add_space(4.0);
                ui.label(format!("{} tracks", self.snapshot.tracks));
                ui.label(format!("{} albums", self.snapshot.albums));
                ui.label(format!("{} playlists", self.snapshot.playlists));
                ui.label(theme::human_bytes(self.snapshot.library_bytes));
                ui.add_space(10.0);
                if self.queue.len() > 0 {
                    theme::tag(ui, &format!("{} queued", self.queue.len()), ORANGE);
                }
                if !self.selected.is_empty() {
                    theme::tag(ui, &format!("{} selected", self.selected.len()), YELLOW);
                }
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
                                .fill(ORANGE)
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
                                let color = if *ok { GREEN } else { RED };
                                ui.label(RichText::new(summary).color(color));
                            }
                            (None, Some(line)) => {
                                let width = (ui.available_width() - 260.0).max(120.0);
                                ui.allocate_ui_with_layout(
                                    egui::vec2(width, 20.0),
                                    Layout::left_to_right(Align::Center),
                                    |ui| {
                                        ui.add(
                                            egui::Label::new(
                                                RichText::new(line.text.replace('\n', " "))
                                                    .color(level_color(line.level)),
                                            )
                                            .truncate(),
                                        );
                                    },
                                );
                            }
                            (None, None) => {
                                ui.label(RichText::new("Ready").color(MUTED));
                            }
                        },
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if self.busy {
                            let (rect, _) = ui.allocate_exact_size(Vec2::new(10.0, 10.0), Sense::hover());
                            ui.painter().circle_filled(rect.center(), 4.0, ORANGE);
                            ui.label(RichText::new("working…").color(ORANGE));
                        }
                        if self.link == Link::Connected && !self.snapshot.topology.is_empty() {
                            ui.label(RichText::new(&self.snapshot.topology).color(MUTED).size(11.0));
                        }
                    });
                });
            });
    }

    // ── pages ────────────────────────────────────────────────────────────────

    fn page_header(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(RichText::new(self.page.title()).size(26.0));
            ui.label(RichText::new(self.page.subtitle()).color(MUTED));
        });
        ui.add_space(4.0);
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
                            .color(NEON_MAGENTA)
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
            ui.label("Search");
            ui.add(
                egui::TextEdit::singleline(&mut self.search)
                    .hint_text("title, artist, album or file name")
                    .desired_width(280.0),
            );
            if filtered && ui.button("Clear filter").clicked() {
                self.search.clear();
                self.album_filter = None;
            }
            if let Some(album) = self.album_filter.clone() {
                theme::tag(ui, &format!("album: {album}"), ORANGE);
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let label = if self.sort_desc { "▼" } else { "▲" };
                ui.label(RichText::new(format!("sorted by {} {label}", self.sort.label())).color(MUTED));
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
                match key {
                    Some(key) => {
                        let active = self.sort == key;
                        let text = RichText::new(*label)
                            .color(if active { ORANGE } else { MUTED })
                            .size(12.0)
                            .strong();
                        if ui
                            .add_sized([*width, 18.0], egui::Button::new(text).frame(false))
                            .clicked()
                        {
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
                    ui.horizontal(|ui| {
                        let mut checked = selected;
                        if ui
                            .add_sized([18.0, 18.0], egui::Checkbox::without_text(&mut checked))
                            .changed()
                        {
                            toggle = Some((row.handle, checked));
                        }
                        let number = row
                            .track_no
                            .map(|n| n.to_string())
                            .unwrap_or_else(|| "—".to_string());
                        ui.add_sized([36.0, 18.0], egui::Label::new(RichText::new(number).color(MUTED).size(12.0)));
                        let title = if selected {
                            RichText::new(&row.title).color(ORANGE)
                        } else {
                            RichText::new(&row.title)
                        };
                        ui.add_sized([250.0, 18.0], egui::Label::new(title).truncate());
                        ui.add_sized(
                            [180.0, 18.0],
                            egui::Label::new(RichText::new(if row.artist.is_empty() { "—" } else { &row.artist }).color(MUTED)).truncate(),
                        );
                        ui.add_sized(
                            [180.0, 18.0],
                            egui::Label::new(RichText::new(if row.album.is_empty() { "—" } else { &row.album }).color(MUTED)).truncate(),
                        );
                        ui.add_sized([62.0, 18.0], egui::Label::new(RichText::new(&row.length).color(MUTED).size(12.0)));
                        ui.add_sized(
                            [78.0, 18.0],
                            egui::Label::new(RichText::new(theme::human_bytes(row.size as u64)).color(MUTED).size(12.0)),
                        );
                    });
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
            ui.label(format!(
                "{} shown · {} MB · {} selected",
                rows.len(),
                bytes / 1_048_576,
                self.selected.len()
            ));
            if ui.button("Select all shown").clicked() {
                for row in &rows {
                    self.selected.insert(row.handle);
                }
            }
            if ui.button("Select none").clicked() {
                self.selected.clear();
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let enabled = !self.selected.is_empty() && !self.busy && self.link == Link::Connected;
                if ui
                    .add_enabled(enabled, egui::Button::new(RichText::new("Delete selected").color(RED)))
                    .clicked()
                {
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
            ui.label(RichText::new(format!("{} albums", self.albums.len())).strong());
            ui.add_space(SP_8);
            ui.label(RichText::new("sort").color(MUTED).size(12.0));
            for key in AlbumSort::ALL {
                let active = self.album_sort == key;
                if ui
                    .add(
                        egui::Button::new(RichText::new(key.label()).color(if active { NEON_CYAN } else { MUTED }))
                            .frame(false),
                    )
                    .clicked()
                {
                    self.album_sort = key;
                }
            }
            ui.add(
                egui::TextEdit::singleline(&mut self.album_search)
                    .hint_text("filter albums")
                    .desired_width(200.0),
            );
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                // Deliberately *not* gated on `busy`: pressing this stops the library
                // read early so the answer arrives in seconds instead of minutes.
                if ui
                    .add_enabled(self.link == Link::Connected, egui::Button::new("Diagnose covers"))
                    .on_hover_text(
                        "Fast: reads only the image objects and a few albums (seconds, not minutes) and \
                         writes the real association structure to the Log page",
                    )
                    .clicked()
                {
                    diagnose_covers = true;
                }
                if ui
                    .add_enabled(self.link == Link::Connected, egui::Button::new("Diagnose albums"))
                    .on_hover_text(
                        "Dump the real reference/property structure of the first albums to the Log page",
                    )
                    .clicked()
                {
                    diagnose = true;
                }
                if ui
                    .add_enabled(!self.busy && self.link == Link::Connected, egui::Button::new("Refresh"))
                    .on_hover_text("Re-read the device now and overwrite the cached library")
                    .clicked()
                {
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
            self.worker.send(Command::Refresh);
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
            cover: Option<egui::TextureId>,
            badge: CoverBadge,
        }
        let mut tiles: Vec<Tile> = Vec::with_capacity(rows.len());
        for (album, subtitle) in &rows {
            let (cover, badge) = match album.cover_handle {
                Some(handle) => match self.covers.get(&handle) {
                    Some(CoverState::Ready(texture)) => (Some(texture.id()), CoverBadge::Ready),
                    Some(CoverState::Failed(reason)) => (None, CoverBadge::Failed(reason.clone())),
                    Some(CoverState::Loading) | None => (None, CoverBadge::Loading),
                },
                None => (None, CoverBadge::Missing),
            };
            tiles.push(Tile {
                name: album.name.clone(),
                subtitle: subtitle.clone(),
                handle: album.cover_handle,
                cover,
                badge,
            });
        }
        // Request what is not cached yet (cheap to ask; the worker de-duplicates).
        for tile in &tiles {
            if let Some(handle) = tile.handle {
                if tile.cover.is_none() && !matches!(tile.badge, CoverBadge::Failed(_)) {
                    self.request_cover(handle);
                }
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
            ui.label(RichText::new(tracked("album")).font(label()).color(NEON_CYAN));
        });
        ui.add_space(SP_12);

        // Cover + headline statistics.
        let cover_side = 180.0;
        let (cover_rect, _) = ui.allocate_exact_size(Vec2::splat(cover_side), Sense::hover());
        let (texture, badge) = match album.cover_handle {
            Some(handle) => match self.covers.get(&handle) {
                Some(CoverState::Ready(texture)) => (Some(texture.id()), CoverBadge::Ready),
                Some(CoverState::Failed(reason)) => (None, CoverBadge::Failed(reason.clone())),
                Some(CoverState::Loading) | None => (None, CoverBadge::Loading),
            },
            None => (None, CoverBadge::Missing),
        };
        if let Some(handle) = album.cover_handle {
            if texture.is_none() && !matches!(badge, CoverBadge::Failed(_)) {
                self.request_cover(handle);
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
                        NEON_CYAN,
                    );
                    let pos = cover_rect.center() - galley.size() / 2.0;
                    paint::glow_galley(painter, pos, &galley, NEON_CYAN, GLOW_TEXT, GlowResponse::IDLE);
                    painter.galley(pos, galley, NEON_CYAN);
                    paint::hairline(painter, cover_rect, r_md());
                }
            }
            if let CoverBadge::Failed(reason) = &badge {
                painter.text(
                    cover_rect.center_bottom() - Vec2::new(0.0, 8.0),
                    egui::Align2::CENTER_BOTTOM,
                    "cover unavailable",
                    body_sm(),
                    NEON_MAGENTA,
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
                NEON_CYAN,
            );
            y += 26.0;
            painter.text(
                egui::pos2(info_rect.left(), y),
                egui::Align2::LEFT_TOP,
                format!("cover art: {}", album.cover_source.label()),
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
                    NEON_MAGENTA,
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
                        .accent(NEON_CYAN),
                );
            }
        });
    }

    fn push_page(&mut self, ui: &mut egui::Ui) {
        self.banner(ui);
        ui.label(RichText::new("Drag files or folders onto this window, or type a path.").color(MUTED));
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.path_input)
                    .hint_text("/Users/you/Music/Album")
                    .desired_width(420.0),
            );
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
                ui.label("Album title");
                ui.add(
                    egui::TextEdit::singleline(&mut self.album_name)
                        .hint_text("(default: folder name / first track's album tag)")
                        .desired_width(300.0),
                );
                ui.checkbox(&mut self.make_playlist, "create playlist");
            });
            ui.horizontal(|ui| {
                ui.label("Cover art");
                ui.add(
                    egui::TextEdit::singleline(&mut self.cover_path)
                        .hint_text("(default: largest JPEG in the folder)")
                        .desired_width(380.0),
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

        // One motion handle per frame: it is the only thing that may ask for
        // another repaint, which is what keeps an idle window at 0 % CPU.
        let mut motion = Motion::begin(ui.ctx());

        self.top_bar(ui);
        self.status_bar(ui);
        self.side_bar(ui);
        egui::CentralPanel::default()
            .frame(egui::Frame::central_panel(ui.style()).inner_margin(egui::Margin::same(16)))
            .show(ui, |ui| {
                if self.page != Page::Board {
                    self.page_header(ui);
                }
                match self.page {
                    Page::Board => board::board(ui, &mut motion, &mut self.board),
                    Page::Tracks => self.tracks_page(ui),
                    Page::Albums => self.albums_page(ui, &mut motion),
                    Page::Push => self.push_page(ui),
                    Page::Device => self.device_page(ui),
                    Page::Wifi => self.wifi_page(ui),
                    Page::Log => self.log_page(ui),
                }
            });
        self.confirm_dialog(ui.ctx());
        motion.finish(ui.ctx());
    }
}

/// Album totals use the same formatting as track rows.
fn duration_label(seconds: u32) -> String {
    format_duration(seconds)
}

fn level_color(level: Level) -> Color32 {
    match level {
        Level::Info => TEXT,
        Level::Warn => YELLOW,
        Level::Error => RED,
        Level::Ok => GREEN,
    }
}
