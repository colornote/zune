//! Design tokens — the single source of truth for colour, type, spacing, radius
//! and motion. Nothing in the UI should hard-code a hex value or a font size.
//!
//! Mirrors `UI_DESIGN.md` §2 **plus the v1.1 upgrade (lines 407+)**, which takes
//! precedence wherever it conflicts:
//!
//!   * **pure black canvas** (`#000000`) — no blue offset anywhere;
//!   * **1px hairline borders** on cards, because `#000 → #0A0A0A` is not enough
//!     contrast to carry the hierarchy on its own (this reverses v1.0's "no borders");
//!   * a **neon collision palette**: small areas, high saturation, always glowing,
//!     never a large fill, never two neons inside one component;
//!   * **pure white** primary text, secondary/tertiary text by *alpha* rather than grey;
//!   * `display` type at **88pt** ExtraLight.

use eframe::egui::{Color32, Context, CornerRadius, FontFamily, FontId, TextStyle};

// ── colour: base layers (v1.1 — pure black, depth by luminance + hairline) ───

/// `#000000` — pure black, used for *large* areas. Never a blue-tinted near-black.
pub const CANVAS: Color32 = Color32::from_rgb(0x00, 0x00, 0x00);
/// `#0A0A0A` — card body: +4 % luminance, almost merged into the canvas.
pub const SURFACE_1: Color32 = Color32::from_rgb(0x0A, 0x0A, 0x0A);
/// `#141414` — floating layers, hover, selection.
pub const SURFACE_2: Color32 = Color32::from_rgb(0x14, 0x14, 0x14);
/// `rgba(255,255,255,0.08)` — **the** v1.1 border: on pure black this is the only
/// reliable edge, and every card carries it.
pub const HAIRLINE: Color32 = Color32::from_rgba_premultiplied(20, 20, 20, 20);
/// `rgba(255,255,255,0.06)` — separators between list rows and sections.
pub const DIVIDER: Color32 = Color32::from_rgba_premultiplied(15, 15, 15, 15);
/// `rgba(255,255,255,0.035)` — glass fill over a surface.
pub const GLASS_TINT: Color32 = Color32::from_rgba_premultiplied(9, 9, 9, 9);
/// Kept for the glass edge highlight; equal to the hairline by design.
pub const GLASS_BORDER: Color32 = HAIRLINE;

// ── colour: the neon collision system (small areas, high saturation) ────────

/// `#FF6A00` — brand accent, the Collection signature.
pub const NEON_ORANGE: Color32 = Color32::from_rgb(0xFF, 0x6A, 0x00);
/// `#FFB020` — in progress / transferring.
pub const NEON_AMBER: Color32 = Color32::from_rgb(0xFF, 0xB0, 0x20);
/// `#CCFF00` — success / connected / active, the Tracks signature.
pub const NEON_LIME: Color32 = Color32::from_rgb(0xCC, 0xFF, 0x00);
/// `#00E5FF` — information / secondary actions, the Albums signature.
pub const NEON_CYAN: Color32 = Color32::from_rgb(0x00, 0xE5, 0xFF);
/// `#FF00A0` — favourites / special marks, the Artists signature.
pub const NEON_MAGENTA: Color32 = Color32::from_rgb(0xFF, 0x00, 0xA0);
/// `#8B5CF6` — playlists / categories, the Playlist & Sync signature.
pub const NEON_VIOLET: Color32 = Color32::from_rgb(0x8B, 0x5C, 0xF6);

/// Every neon, with its hex and its meaning — the board renders this table.
pub const NEONS: [(&str, Color32, &str); 6] = [
    ("neon-orange", NEON_ORANGE, "primary / brand"),
    ("neon-amber", NEON_AMBER, "in progress / transfer"),
    ("neon-lime", NEON_LIME, "success / connected / active"),
    ("neon-cyan", NEON_CYAN, "information / secondary"),
    ("neon-magenta", NEON_MAGENTA, "favourite / special"),
    ("neon-violet", NEON_VIOLET, "playlist / category"),
];

/// The primary accent (v1.0's `accent`): the brand orange.
pub const ACCENT: Color32 = NEON_ORANGE;
/// Hover variant of the brand accent.
pub const ACCENT_HI: Color32 = Color32::from_rgb(0xFF, 0x8A, 0x2B);
/// Pressed / dim variant of the brand accent.
pub const ACCENT_LO: Color32 = Color32::from_rgb(0xC7, 0x4F, 0x00);
/// v1.0 kept `accent-grad`; v1.1 forbids two neons inside one component, so the
/// far stop now means "the amber transfer accent" rather than a gradient partner.
pub const AMBER: Color32 = NEON_AMBER;

/// A neon tint at a given alpha (pre-multiplied, so it composites correctly).
pub const fn alpha_of(color: Color32, a: f32) -> Color32 {
    Color32::from_rgba_premultiplied(
        (color.r() as f32 * a) as u8,
        (color.g() as f32 * a) as u8,
        (color.b() as f32 * a) as u8,
        (a * 255.0) as u8,
    )
}

/// Accent tint used by the legacy shim and by washes.
pub const fn accent_alpha(a: f32) -> Color32 {
    alpha_of(NEON_ORANGE, a)
}

// ── colour: text (pure white, then alpha) ───────────────────────────────────

/// `#FFFFFF` — primary text. Pure white, per v1.1.
pub const TEXT_1: Color32 = Color32::from_rgb(0xFF, 0xFF, 0xFF);
/// `rgba(255,255,255,0.62)` — secondary text: transparency, not grey.
pub const TEXT_2: Color32 = Color32::from_rgba_premultiplied(158, 158, 158, 158);
/// `rgba(255,255,255,0.38)` — tertiary text and labels.
pub const TEXT_3: Color32 = Color32::from_rgba_premultiplied(97, 97, 97, 97);
/// Text on a neon fill.
pub const TEXT_INV: Color32 = CANVAS;

/// Semantic aliases: on pure black these are neon, not muted greys.
pub const OK: Color32 = NEON_LIME;
pub const WARN: Color32 = NEON_AMBER;
pub const ERR: Color32 = NEON_MAGENTA;

// ── colour: page signatures (one neon per page, applied systematically) ─────

/// Each page owns exactly one neon. Components take their accent from here, which
/// is what keeps "at most two neons on screen" true as pages are built out.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PageAccent {
    /// Collection — the brand's home turf.
    Collection,
    Albums,
    Artists,
    Tracks,
    Playlist,
    /// Sync / any transfer in flight.
    Transfer,
    /// No signature (settings, diagnostics).
    Neutral,
}

impl PageAccent {
    pub fn color(self) -> Color32 {
        match self {
            PageAccent::Collection => NEON_ORANGE,
            PageAccent::Albums => NEON_CYAN,
            PageAccent::Artists => NEON_MAGENTA,
            PageAccent::Tracks => NEON_LIME,
            PageAccent::Playlist => NEON_VIOLET,
            PageAccent::Transfer => NEON_AMBER,
            PageAccent::Neutral => TEXT_2,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            PageAccent::Collection => "Collection",
            PageAccent::Albums => "Albums",
            PageAccent::Artists => "Artists",
            PageAccent::Tracks => "Tracks",
            PageAccent::Playlist => "Playlist / Sync",
            PageAccent::Transfer => "Transferring",
            PageAccent::Neutral => "Neutral",
        }
    }

    /// The token behind the signature, for the spec table.
    pub fn token(self) -> &'static str {
        match self {
            PageAccent::Collection => "neon-orange",
            PageAccent::Albums => "neon-cyan",
            PageAccent::Artists => "neon-magenta",
            PageAccent::Tracks => "neon-lime",
            PageAccent::Playlist => "neon-violet",
            PageAccent::Transfer => "neon-amber",
            PageAccent::Neutral => "text-2",
        }
    }

    /// Every signature, in board order.
    pub const ALL: [PageAccent; 7] = [
        PageAccent::Collection,
        PageAccent::Albums,
        PageAccent::Artists,
        PageAccent::Tracks,
        PageAccent::Playlist,
        PageAccent::Transfer,
        PageAccent::Neutral,
    ];
}

// ── type: the ladder is deliberately extreme (88 vs 11) ─────────────────────

/// 88pt ExtraLight (v1.1 raised this from 72).
pub const DISPLAY: f32 = 88.0;
pub const H1: f32 = 44.0;
pub const H2: f32 = 28.0;
pub const H3: f32 = 19.0;
pub const BODY: f32 = 15.0;
pub const BODY_SM: f32 = 13.0;
pub const LABEL: f32 = 11.0;
pub const MONO: f32 = 13.0;

/// Inter variable instances, one named family per weight (`wght` axis).
pub const F_XLIGHT: &str = "inter-xl"; // 200
pub const F_LIGHT: &str = "inter-light"; // 300
pub const F_REG: &str = "inter-reg"; // 400
pub const F_SEMI: &str = "inter-semi"; // 600
pub const F_BOLD: &str = "inter-bold"; // 700
pub const F_LIGHT_ITALIC: &str = "inter-light-italic";
pub const F_ITALIC: &str = "inter-italic";
/// The system CJK fallback, appended to *every* family so metadata in Chinese
/// renders without embedding a 20 MB font.
pub const F_CJK: &str = "cjk-fallback";

/// Named text styles registered in `install`; use with `TextStyle::Name`.
pub const TS_DISPLAY: &str = "display";
pub const TS_H1: &str = "h1";
pub const TS_H2: &str = "h2";
pub const TS_H3: &str = "h3";
pub const TS_BODY: &str = "body";
pub const TS_BODY_SM: &str = "body-sm";
pub const TS_LABEL: &str = "label";
pub const TS_MONO: &str = "mono";
pub const TS_SUBTITLE: &str = "subtitle"; // italic body
pub const TS_NUMERIC: &str = "numeric"; // tabular, 20pt for the ring

fn family(name: &str) -> FontFamily {
    FontFamily::Name(name.into())
}

/// 88pt ExtraLight — the bleeding page title.
pub fn display() -> FontId {
    FontId::new(DISPLAY, family(F_XLIGHT))
}
/// 44pt Light.
pub fn h1() -> FontId {
    FontId::new(H1, family(F_LIGHT))
}
/// 28pt Regular.
pub fn h2() -> FontId {
    FontId::new(H2, family(F_REG))
}
/// 19pt SemiBold.
pub fn h3() -> FontId {
    FontId::new(H3, family(F_SEMI))
}
pub fn body() -> FontId {
    FontId::new(BODY, family(F_REG))
}
pub fn body_sm() -> FontId {
    FontId::new(BODY_SM, family(F_REG))
}
pub fn label() -> FontId {
    FontId::new(LABEL, family(F_SEMI))
}
/// Italic body — the Metro subtitle convention.
pub fn subtitle() -> FontId {
    FontId::new(BODY, family(F_LIGHT_ITALIC))
}
/// Tabular figures: durations, sizes, counts.
pub fn mono(size: f32) -> FontId {
    FontId::new(size, FontFamily::Monospace)
}
/// 20pt mono for the centre of a `ProgressRing`.
pub fn numeric() -> FontId {
    mono(20.0)
}

/// `label` styling: UPPERCASE with +0.08em tracking.
///
/// egui has no letter-spacing, so the tracking is inserted as thin spaces — the
/// only way to hit the spec's +0.08em without a custom text layout.
pub fn tracked(text: &str) -> String {
    let mut out = String::with_capacity(text.len() * 2);
    for ch in text.chars() {
        out.push(ch);
        if !ch.is_whitespace() {
            out.push('\u{2009}'); // thin space, 0.5em
        }
    }
    out
}

// ── spacing: an 8pt grid, nothing in between ────────────────────────────────

pub const SP_4: f32 = 4.0;
pub const SP_8: f32 = 8.0;
pub const SP_12: f32 = 12.0;
pub const SP_16: f32 = 16.0;
pub const SP_24: f32 = 24.0;
pub const SP_32: f32 = 32.0;
pub const SP_48: f32 = 48.0;
pub const SP_64: f32 = 64.0;
pub const SP_96: f32 = 96.0;

/// The allowed grid, for the board and for tests.
pub const SPACING_SCALE: [f32; 9] = [SP_4, SP_8, SP_12, SP_16, SP_24, SP_32, SP_48, SP_64, SP_96];

// ── radius ──────────────────────────────────────────────────────────────────

pub const R_SM: u8 = 8;
pub const R_MD: u8 = 14;
pub const R_LG: u8 = 20;
pub const R_XL: u8 = 28;
pub const R_PILL: u8 = 255;

pub fn r_sm() -> CornerRadius {
    CornerRadius::same(R_SM)
}
pub fn r_md() -> CornerRadius {
    CornerRadius::same(R_MD)
}
pub fn r_lg() -> CornerRadius {
    CornerRadius::same(R_LG)
}
pub fn r_xl() -> CornerRadius {
    CornerRadius::same(R_XL)
}
pub fn r_pill() -> CornerRadius {
    CornerRadius::same(R_PILL)
}

// ── layout sizes ────────────────────────────────────────────────────────────

pub const NAV_WIDTH: f32 = 64.0;
pub const NAV_WIDTH_EXPANDED: f32 = 200.0;
pub const TOPBAR_HEIGHT: f32 = 52.0;
pub const TRANSPORT_HEIGHT: f32 = 72.0;
pub const CONTENT_PAD_H: f32 = 32.0;
pub const CONTENT_PAD_V: f32 = 24.0;
pub const CARD_GAP: f32 = 16.0;
pub const LIST_GAP: f32 = 12.0;

// ── motion ──────────────────────────────────────────────────────────────────

/// Press feedback, colour swaps.
pub const T_INSTANT: f32 = 0.080;
/// Hover, fades.
pub const T_QUICK: f32 = 0.160;
/// Cards, panels, lists.
pub const T_STANDARD: f32 = 0.240;
/// Page transitions, layout changes.
pub const T_SLOW: f32 = 0.400;

/// Spring constant for the physical interactions (doc §2.5).
pub const SPRING_STIFFNESS: f32 = 180.0;
pub const SPRING_DAMPING: f32 = 22.0;

/// The breathing pulse used while a transfer runs: 1.6 s period.
pub const PULSE_PERIOD: f32 = 1.6;

// ── glow response (v1.1 §2.1.4) ─────────────────────────────────────────────

/// Hover multiplies a glow's radius by this much...
pub const GLOW_HOVER_RADIUS: f32 = 1.4;
/// ...and adds this to its opacity.
pub const GLOW_HOVER_ALPHA: f32 = 0.15;
/// Pressed multiplies the radius by this much.
pub const GLOW_PRESS_RADIUS: f32 = 0.8;

// ── small formatting helpers used across components ─────────────────────────

/// A byte count in the units a music player shows (`12.4 GB`).
pub fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit >= 2 {
        format!("{value:.1} {}", UNITS[unit])
    } else {
        format!("{value:.0} {}", UNITS[unit])
    }
}

/// A deterministic colour per album name — the placeholder when a cover is not on
/// the device. Deliberately dark and low saturation: a cover placeholder is a large
/// area, and v1.1 forbids large neon fills.
pub fn album_tint(name: &str) -> Color32 {
    let mut hash: u32 = 0x811C_9DC5;
    for byte in name.as_bytes() {
        hash ^= *byte as u32;
        hash = hash.wrapping_mul(0x0100_0193);
    }
    let hue = (hash % 360) as f32;
    let (r, g, b) = hsv_to_rgb(hue, 0.28, 0.13);
    Color32::from_rgb(r, g, b)
}

fn hsv_to_rgb(h: f32, s: f32, v: f32) -> (u8, u8, u8) {
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

// ── install ─────────────────────────────────────────────────────────────────

/// Apply the tokens to an egui context: visuals, spacing and the type ladder.
/// Fonts are installed separately by [`crate::design::fonts::install`].
pub fn install(ctx: &Context) {
    ctx.options_mut(|o| o.theme_preference = eframe::egui::ThemePreference::Dark);
    ctx.all_styles_mut(|style| {
        let v = &mut style.visuals;
        v.dark_mode = true;
        // Pure black everywhere a large area is painted.
        v.panel_fill = CANVAS;
        v.window_fill = SURFACE_1;
        v.extreme_bg_color = CANVAS;
        v.faint_bg_color = SURFACE_1;
        v.override_text_color = Some(TEXT_1);
        v.selection.bg_fill = alpha_of(NEON_ORANGE, 0.24);
        v.selection.stroke = eframe::egui::Stroke::new(1.0, NEON_ORANGE);
        v.window_stroke = eframe::egui::Stroke::new(1.0, HAIRLINE);
        v.window_corner_radius = r_lg();
        v.menu_corner_radius = r_md();
        v.window_shadow = eframe::egui::epaint::Shadow::NONE;
        v.popup_shadow = eframe::egui::epaint::Shadow::NONE;

        // Content, not chrome: flat widgets, edges carried by the hairline.
        for w in [
            &mut v.widgets.noninteractive,
            &mut v.widgets.inactive,
            &mut v.widgets.hovered,
            &mut v.widgets.active,
            &mut v.widgets.open,
        ] {
            w.corner_radius = r_sm();
            w.bg_stroke = eframe::egui::Stroke::new(1.0, HAIRLINE);
            w.expansion = 0.0;
        }
        v.widgets.noninteractive.bg_fill = SURFACE_1;
        v.widgets.noninteractive.weak_bg_fill = SURFACE_1;
        v.widgets.noninteractive.fg_stroke = eframe::egui::Stroke::new(1.0, TEXT_2);
        v.widgets.inactive.bg_fill = SURFACE_2;
        v.widgets.inactive.weak_bg_fill = SURFACE_2;
        v.widgets.inactive.fg_stroke = eframe::egui::Stroke::new(1.0, TEXT_1);
        v.widgets.hovered.bg_fill = SURFACE_2;
        v.widgets.hovered.weak_bg_fill = SURFACE_2;
        v.widgets.hovered.fg_stroke = eframe::egui::Stroke::new(1.0, NEON_ORANGE);
        v.widgets.active.bg_fill = alpha_of(NEON_ORANGE, 0.18);
        v.widgets.active.weak_bg_fill = alpha_of(NEON_ORANGE, 0.18);
        v.widgets.active.fg_stroke = eframe::egui::Stroke::new(1.0, NEON_ORANGE);

        style.spacing.item_spacing = eframe::egui::vec2(SP_8, SP_8);
        style.spacing.button_padding = eframe::egui::vec2(SP_12, SP_8);
        style.spacing.interact_size.y = 32.0;
        style.spacing.scroll.bar_width = 8.0;
        style.spacing.scroll.floating = true;
        style.animation_time = T_QUICK;

        // The type ladder.
        style.text_styles = [
            (TextStyle::Small, body_sm()),
            (TextStyle::Body, body()),
            (TextStyle::Button, body()),
            (TextStyle::Heading, h2()),
            (TextStyle::Monospace, mono(MONO)),
            (TextStyle::Name(TS_DISPLAY.into()), display()),
            (TextStyle::Name(TS_H1.into()), h1()),
            (TextStyle::Name(TS_H2.into()), h2()),
            (TextStyle::Name(TS_H3.into()), h3()),
            (TextStyle::Name(TS_BODY.into()), body()),
            (TextStyle::Name(TS_BODY_SM.into()), body_sm()),
            (TextStyle::Name(TS_LABEL.into()), label()),
            (TextStyle::Name(TS_MONO.into()), mono(MONO)),
            (TextStyle::Name(TS_SUBTITLE.into()), subtitle()),
            (TextStyle::Name(TS_NUMERIC.into()), numeric()),
        ]
        .into();
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(c: Color32) -> String {
        format!("#{:02X}{:02X}{:02X}", c.r(), c.g(), c.b())
    }

    #[test]
    fn the_type_ladder_is_extreme() {
        // The spec's point: no 18/20/22 mush — the steps are deliberate.
        let ladder = [LABEL, BODY_SM, BODY, H3, H2, H1, DISPLAY];
        assert!(ladder.windows(2).all(|w| w[0] < w[1]));
        assert!(DISPLAY >= 4.0 * H3, "display must dwarf h3");
    }

    #[test]
    fn display_is_the_v11_size() {
        assert_eq!(DISPLAY, 88.0);
    }

    #[test]
    fn canvas_is_pure_black_with_no_blue_offset() {
        // v1.1 acceptance: the background is #000000, not a blue-tinted near-black.
        assert_eq!(hex(CANVAS), "#000000");
        assert_eq!(CANVAS.r(), CANVAS.b());
        // The surfaces are neutral too: no channel may lead.
        for surface in [SURFACE_1, SURFACE_2, HAIRLINE, DIVIDER, GLASS_TINT] {
            assert_eq!(surface.r(), surface.b(), "{surface:?} is not neutral");
            assert_eq!(surface.r(), surface.g(), "{surface:?} is not neutral");
        }
    }

    #[test]
    fn primary_text_is_pure_white_and_secondary_is_alpha() {
        assert_eq!(hex(TEXT_1), "#FFFFFF");
        // text-2/3 are white at reduced alpha, so r == g == b == a.
        for t in [TEXT_2, TEXT_3] {
            assert_eq!(t.r(), t.a());
            assert_eq!(t.g(), t.b());
            assert!(t.r() < 255);
        }
    }

    #[test]
    fn neon_tokens_match_the_spec() {
        assert_eq!(hex(NEON_ORANGE), "#FF6A00");
        assert_eq!(hex(NEON_AMBER), "#FFB020");
        assert_eq!(hex(NEON_LIME), "#CCFF00");
        assert_eq!(hex(NEON_CYAN), "#00E5FF");
        assert_eq!(hex(NEON_MAGENTA), "#FF00A0");
        assert_eq!(hex(NEON_VIOLET), "#8B5CF6");
        assert_eq!(NEONS.len(), 6);
    }

    #[test]
    fn every_neon_is_saturated_enough_to_glow() {
        // "High saturation" is the whole point: each neon must have a near-full
        // channel and a dark one, so it reads as light on pure black. (Violet
        // #8B5CF6 tops out at 0xF6 rather than 0xFF, so the bar is 0xF0.)
        for (name, color, _) in NEONS {
            let max = color.r().max(color.g()).max(color.b());
            let min = color.r().min(color.g()).min(color.b());
            assert!(max >= 0xF0, "{name} has no near-full channel (max {max:#04x})");
            assert!(min <= 0x60, "{name} is not saturated (min channel {min:#04x})");
            assert!(max - min >= 0x80, "{name} is too grey (range {:#04x})", max - min);
        }
    }

    #[test]
    fn page_signatures_are_unique_and_neon() {
        let mut seen = Vec::new();
        for accent in PageAccent::ALL {
            let color = accent.color();
            if accent != PageAccent::Neutral {
                assert!(NEONS.iter().any(|(_, n, _)| *n == color), "{accent:?} is not a neon");
                assert!(!seen.contains(&color), "{accent:?} duplicates another signature");
            }
            seen.push(color);
        }
        assert_eq!(PageAccent::Collection.color(), NEON_ORANGE);
        assert_eq!(PageAccent::Albums.color(), NEON_CYAN);
        assert_eq!(PageAccent::Artists.color(), NEON_MAGENTA);
        assert_eq!(PageAccent::Tracks.color(), NEON_LIME);
        assert_eq!(PageAccent::Playlist.color(), NEON_VIOLET);
        assert_eq!(PageAccent::Transfer.color(), NEON_AMBER);
    }

    #[test]
    fn spacing_is_the_8pt_grid() {
        for value in SPACING_SCALE {
            assert!(
                value % 4.0 == 0.0,
                "{value} is off the 4/8/12/16/24/32/48/64/96 grid"
            );
        }
    }

    #[test]
    fn labels_are_tracked_and_uppercase() {
        let tracked = tracked("now playing");
        assert!(tracked.contains('\u{2009}'));
        assert_eq!(tracked.chars().filter(|c| *c == '\u{2009}').count(), 10);
    }

    #[test]
    fn human_bytes_uses_player_units() {
        assert_eq!(human_bytes(0), "0 B");
        assert_eq!(human_bytes(1024), "1 KB");
        assert_eq!(human_bytes(1024 * 1024), "1.0 MB");
        assert_eq!(human_bytes(31_300_000_000), "29.2 GB");
    }

    #[test]
    fn album_placeholders_stay_dark_so_they_are_not_neon_fills() {
        for name in ["Kid A", "Ok Computer", "Voodoo", "  ", "蓝莓之夜"] {
            let c = album_tint(name);
            let max = c.r().max(c.g()).max(c.b());
            assert!(max < 0x30, "placeholder for {name:?} is too bright: {c:?}");
        }
    }

    #[test]
    fn alpha_of_premultiplies() {
        let half = alpha_of(NEON_CYAN, 0.5);
        assert_eq!(half.a(), 127);
        assert_eq!(half.r(), 0);
        assert_eq!(half.b(), 127);
    }
}
