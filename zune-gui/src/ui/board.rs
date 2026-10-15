//! **The component board** — every token and primitive on one page, for visual review.
//!
//! Updated for **v1.1**: pure black canvas, 1px hairlines, the neon collision palette
//! and its mandatory glows, pure white type at 88pt.
//!
//! Two kinds of section live here, and they follow different rules:
//!
//! * **spec sheets** (colour, neon palette, page signatures, glow tiers) exist to
//!   *define* the tokens, so they necessarily show all six neons. They are framed as
//!   documentation and are not an interface sample.
//! * **component sections** obey the real rules: at most two neons in view, one neon
//!   per component, every neon glowing, no neon fill larger than a bar or a button.

use eframe::egui::{self, Align2, Color32, FontId, Pos2, Rect, RichText, Sense, Ui, Vec2};

use crate::design::anim::{Bezier, Eased, Motion, Spring};
use crate::design::paint::{self, GlowResponse, GLOW_LG, GLOW_MD, GLOW_NONE, GLOW_SM, GLOW_TEXT};
use crate::design::tokens::*;
use crate::ui::device_pill::{device_pill, PillState};
use crate::ui::glass_card::GlassCard;
use crate::ui::gradient_bar::GradientBar;
use crate::ui::icon_button::{IconButton, PrimaryButton};
use crate::ui::icons::Icon;
use crate::ui::progress_ring::ProgressRing;
use crate::ui::section::section_rule;
use crate::ui::track_row::{track_row, TrackRow};

/// Interactive demo state (the rest is animated in egui memory).
pub struct BoardState {
    pub card_selected: bool,
    pub row_selected: bool,
    pub playing: bool,
    pub ring_fraction: f32,
    pub spring_target: f32,
    pub eased: Eased,
    pub curve_spring: Spring,
    pub hairline_on: bool,
}

impl Default for BoardState {
    fn default() -> Self {
        Self {
            card_selected: false,
            row_selected: true,
            playing: true,
            ring_fraction: 0.62,
            spring_target: 0.0,
            eased: Eased::new(0.0, T_SLOW, Bezier::ENTER),
            curve_spring: Spring::new(0.0),
            hairline_on: true,
        }
    }
}

pub fn board(ui: &mut Ui, motion: &mut Motion, state: &mut BoardState) {
    // The canvas is pure black; the dot matrix is the only background texture.
    let panel = ui.available_rect_before_wrap();
    let dots = paint::dot_grid(ui.ctx());
    paint::paint_dot_grid(ui.painter(), panel, &dots);

    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| board_content(ui, motion, state));
}

fn board_content(ui: &mut Ui, motion: &mut Motion, state: &mut BoardState) {
    // The heading comes from the shared page header now, partly so this page cannot drift
    // out of step with the other eight again.

    spec_base_and_text(ui);
    ui.add_space(SP_48);
    spec_neon_palette(ui, motion);
    ui.add_space(SP_48);
    spec_page_signatures(ui, motion);
    ui.add_space(SP_48);
    glow_tiers(ui, motion);
    ui.add_space(SP_48);
    hairline_demo(ui, motion, state);
    ui.add_space(SP_48);
    typography(ui);
    ui.add_space(SP_48);
    spacing_and_radius(ui, motion);
    ui.add_space(SP_48);
    motion_section(ui, motion, state);
    ui.add_space(SP_48);
    components(ui, motion, state);
    ui.add_space(SP_48);
    composition(ui, motion);
    ui.add_space(SP_64);
}

// ── 1. the bleeding title (88pt ExtraLight, clipped rather than wrapped) ────


/// A framed "spec sheet": documentation, not an interface sample.
fn spec_frame<R>(ui: &mut Ui, title: &str, note: &str, add: impl FnOnce(&mut Ui) -> R) -> R {
    egui::Frame::NONE
        .fill(SURFACE_1)
        .corner_radius(r_lg())
        .inner_margin(egui::Margin::same(20))
        .show(ui, |ui| {
            let full = ui.available_rect_before_wrap();
            let _ = full;
            ui.horizontal(|ui| {
                ui.label(RichText::new(tracked(title)).font(label()).color(TEXT_3));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(RichText::new(note).font(mono(10.0)).color(TEXT_3));
                });
            });
            ui.add_space(SP_16);
            let inner = add(ui);
            // Drawn last so the hairline sits on top of the fills.
            let rect = ui.min_rect().expand2(Vec2::new(20.0, 20.0));
            paint::hairline(ui.painter(), rect, r_lg());
            inner
        })
        .inner
}

// ── 2. base layers + text ───────────────────────────────────────────────────

fn spec_base_and_text(ui: &mut Ui) {
    spec_frame(
        ui,
        "Base · text",
        "v1.1: canvas #000000 · hairline 0.08 · text #FFFFFF",
        |ui| {
            ui.horizontal_wrapped(|ui| {
                swatch(ui, "canvas", CANVAS, "大面积纯黑");
                swatch(ui, "surface-1", SURFACE_1, "卡片 #0A0A0A");
                swatch(ui, "surface-2", SURFACE_2, "悬浮 #141414");
                swatch_with_hairline(ui, "hairline", CANVAS, "卡片描边 0.08");
                swatch(ui, "glass-tint", SURFACE_1, "玻璃层 0.035");
                swatch(ui, "divider", CANVAS, "分隔线 0.06");
            });
            ui.add_space(SP_16);
            ui.horizontal_wrapped(|ui| {
                text_chip(ui, "text-1", TEXT_1, "#FFFFFF");
                text_chip(ui, "text-2", TEXT_2, "white 62%");
                text_chip(ui, "text-3", TEXT_3, "white 38%");
                text_chip(ui, "text-inv", TEXT_INV, "#000000");
            });
        },
    );
}

fn swatch(ui: &mut Ui, name: &str, color: Color32, note: &str) {
    swatch_inner(ui, name, color, note, false);
}

fn swatch_with_hairline(ui: &mut Ui, name: &str, color: Color32, note: &str) {
    swatch_inner(ui, name, color, note, true);
}

fn swatch_inner(ui: &mut Ui, name: &str, color: Color32, note: &str, force_hairline: bool) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(126.0, 84.0), Sense::hover());
    let painter = ui.painter();
    let chip = Rect::from_min_size(rect.min, Vec2::new(rect.width(), 48.0));
    painter.rect_filled(chip, r_sm(), color);
    if force_hairline || color.a() < 200 {
        paint::hairline(painter, chip, r_sm());
    }
    painter.text(Pos2::new(chip.left(), chip.bottom() + SP_4), Align2::LEFT_TOP, name, mono(11.0), TEXT_2);
    let hex = if color.a() < 200 {
        format!("white {:.0}%", color.a() as f32 / 2.55)
    } else {
        format!("#{:02X}{:02X}{:02X}", color.r(), color.g(), color.b())
    };
    painter.text(Pos2::new(chip.left(), chip.bottom() + SP_4 + 14.0), Align2::LEFT_TOP, hex, mono(10.0), TEXT_3);
    painter.text(Pos2::new(chip.left(), chip.bottom() + SP_4 + 27.0), Align2::LEFT_TOP, note, body_sm(), TEXT_3);
}

fn text_chip(ui: &mut Ui, name: &str, color: Color32, note: &str) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(126.0, 52.0), Sense::hover());
    let painter = ui.painter();
    let chip = Rect::from_min_size(rect.min, Vec2::new(rect.width(), 30.0));
    painter.rect_filled(chip, r_sm(), SURFACE_1);
    paint::hairline(painter, chip, r_sm());
    painter.text(chip.center(), Align2::CENTER_CENTER, "Aa 0123", h3(), color);
    painter.text(Pos2::new(chip.left(), chip.bottom() + SP_4), Align2::LEFT_TOP, format!("{name} · {note}"), mono(10.0), TEXT_3);
}

// ── 3. the neon collision palette (spec sheet) ──────────────────────────────

fn spec_neon_palette(ui: &mut Ui, motion: &mut Motion) {
    spec_frame(
        ui,
        "Neon palette",
        "小面积 · 高饱和 · 必带发光 · 每组件只用一色",
        |ui| {
            ui.horizontal_wrapped(|ui| {
                for (index, (name, color, meaning)) in NEONS.into_iter().enumerate() {
                    neon_swatch(ui, motion, index as u64, name, color, meaning);
                }
            });
            ui.add_space(SP_16);
            ui.horizontal(|ui| {
                ui.label(RichText::new("❌ no large fills", ).font(body_sm()).color(TEXT_3));
                ui.label(RichText::new("❌ no two neons in one component").font(body_sm()).color(TEXT_3));
                ui.label(RichText::new("✅ dots · bars · indicators · icons · numbers · labels · edge highlights — each glowing").font(body_sm()).color(TEXT_2));
            });
        },
    );
}

fn neon_swatch(ui: &mut Ui, motion: &mut Motion, salt: u64, name: &str, color: Color32, meaning: &str) {
    let (rect, response) = ui.allocate_exact_size(Vec2::new(196.0, 108.0), Sense::hover());
    let id = ui.id().with(("neon-swatch", salt, response.id));
    let hover = crate::design::anim::spring_at(ui, id.with("hover"), response.hovered() as u8 as f32, motion);
    let painter = ui.painter();

    // A small element only: a dot, a bar, a number. Never a filled block.
    let dot = Pos2::new(rect.left() + 18.0, rect.top() + 20.0);
    paint::glow_circle(painter, dot, 5.0, color, GLOW_SM, GlowResponse::hovered(hover));
    painter.circle_filled(dot, 5.0, color);

    let bar = Rect::from_min_size(Pos2::new(rect.left() + 34.0, rect.top() + 17.5), Vec2::new(48.0, 5.0));
    paint::glow_rect(painter, bar, r_pill(), color, GLOW_MD, GlowResponse::hovered(hover));
    painter.rect_filled(bar, r_pill(), color);

    let number = painter.layout_no_wrap("12.4".to_owned(), numeric(), color);
    let number_pos = Pos2::new(rect.left() + 96.0, rect.top() + 8.0);
    paint::glow_galley(painter, number_pos, &number, color, GLOW_TEXT, GlowResponse::hovered(hover));
    painter.galley(number_pos, number.clone(), color);

    painter.text(
        Pos2::new(rect.left(), rect.top() + 44.0),
        Align2::LEFT_TOP,
        name,
        mono(12.0),
        color,
    );
    let hex = format!("#{:02X}{:02X}{:02X}", color.r(), color.g(), color.b());
    painter.text(Pos2::new(rect.left(), rect.top() + 62.0), Align2::LEFT_TOP, hex, mono(10.0), TEXT_3);
    painter.text(Pos2::new(rect.left(), rect.top() + 78.0), Align2::LEFT_TOP, meaning, body_sm(), TEXT_2);
}

// ── 4. page colour signatures (spec sheet) ──────────────────────────────────

fn spec_page_signatures(ui: &mut Ui, motion: &mut Motion) {
    spec_frame(
        ui,
        "Page signatures",
        "每页只用一个主霓虹色 · 同一屏最多两种",
        |ui| {
            for (index, accent) in PageAccent::ALL.into_iter().enumerate() {
                signature_row(ui, motion, index as u64, accent);
            }
        },
    );
}

fn signature_row(ui: &mut Ui, motion: &mut Motion, salt: u64, accent: PageAccent) {
    let (rect, response) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 34.0), Sense::hover());
    let id = ui.id().with(("signature", salt, response.id));
    let hover = crate::design::anim::spring_at(ui, id.with("hover"), response.hovered() as u8 as f32, motion);
    let color = accent.color();
    let painter = ui.painter();
    let neon = accent != PageAccent::Neutral;
    let glow = if neon { GLOW_SM } else { GLOW_NONE };
    let response_state = GlowResponse::hovered(hover * if neon { 1.0 } else { 0.0 });

    painter.text(
        Pos2::new(rect.left(), rect.center().y),
        Align2::LEFT_CENTER,
        accent.name(),
        body(),
        TEXT_1,
    );
    painter.text(
        Pos2::new(rect.left() + 150.0, rect.center().y),
        Align2::LEFT_CENTER,
        accent.token(),
        mono(11.0),
        TEXT_3,
    );

    // The signature applied the systematic way: dot + selection bar + label.
    let dot = Pos2::new(rect.left() + 300.0, rect.center().y);
    paint::glow_circle(painter, dot, 4.5, color, glow, response_state);
    painter.circle_filled(dot, 4.5, color);

    let bar = Rect::from_min_max(
        Pos2::new(rect.left() + 320.0, rect.center().y - 8.0),
        Pos2::new(rect.left() + 323.5, rect.center().y + 8.0),
    );
    paint::glow_rect(painter, bar, r_pill(), color, if neon { GLOW_MD } else { GLOW_NONE }, response_state);
    painter.rect_filled(bar, r_pill(), color);

    let hex = painter.layout_no_wrap(
        format!("#{:02X}{:02X}{:02X}", color.r(), color.g(), color.b()),
        mono(11.0),
        color,
    );
    let pos = Pos2::new(rect.left() + 340.0, rect.center().y - hex.size().y / 2.0);
    if neon {
        paint::glow_galley(painter, pos, &hex, color, GLOW_TEXT, response_state);
    }
    painter.galley(pos, hex, color);
}

// ── 5. glow tiers (spec sheet) ──────────────────────────────────────────────

fn glow_tiers(ui: &mut Ui, motion: &mut Motion) {
    spec_frame(
        ui,
        "Glow tiers",
        "glow-sm / md / lg / text — hover ×1.4 +0.15α · press ×0.8",
        |ui| {
            let (rect, response) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 150.0), Sense::hover());
            let id = ui.id().with(("glow-tiers", response.id));
            let hover = crate::design::anim::spring_at(ui, id.with("hover"), response.hovered() as u8 as f32, motion);
            let painter = ui.painter();

            let column = |i: f32| rect.left() + 40.0 + i * 260.0;

            // glow-sm: the status dot.
            let sm = Pos2::new(column(0.0), rect.top() + 46.0);
            paint::glow_circle(painter, sm, 5.0, NEON_LIME, GLOW_SM, GlowResponse::hovered(hover));
            painter.circle_filled(sm, 5.0, NEON_LIME);
            caption(painter, column(0.0), rect.top() + 78.0, "glow-sm", "状态点 · 小图标");

            // glow-md: the progress / selection bar.
            let md = Rect::from_min_size(Pos2::new(column(1.0) - 30.0, rect.top() + 42.0), Vec2::new(90.0, 6.0));
            paint::glow_rect(painter, md, r_pill(), NEON_AMBER, GLOW_MD, GlowResponse::hovered(hover));
            painter.rect_filled(md, r_pill(), NEON_AMBER);
            caption(painter, column(1.0), rect.top() + 78.0, "glow-md", "进度条 · 选中条");

            // glow-lg: the primary CTA.
            let cta = Rect::from_min_size(Pos2::new(column(2.0) - 30.0, rect.top() + 30.0), Vec2::new(110.0, 34.0));
            paint::neon_fill(painter, cta, r_pill(), NEON_ORANGE, GLOW_LG, GlowResponse::hovered(hover));
            let label = painter.layout_no_wrap("Push".to_owned(), FontId::new(BODY, eframe::egui::FontFamily::Name(F_SEMI.into())), TEXT_INV);
            painter.galley(cta.center() - label.size() / 2.0, label, TEXT_INV);
            caption(painter, column(2.0), rect.top() + 78.0, "glow-lg", "主 CTA · 焦点");

            // text-glow: the neon number.
            let number = painter.layout_no_wrap("12.4 GB".to_owned(), numeric(), NEON_CYAN);
            let number_pos = Pos2::new(column(3.0) - 30.0, rect.top() + 34.0);
            paint::glow_galley(painter, number_pos, &number, NEON_CYAN, GLOW_TEXT, GlowResponse::hovered(hover));
            painter.galley(number_pos, number, NEON_CYAN);
            caption(painter, column(3.0), rect.top() + 78.0, "text-glow", "霓虹数字 · 标题");

            // Response states on one dot: idle / hover / press.
            let states = [
                ("idle", GlowResponse::IDLE),
                ("hover", GlowResponse::new(1.0, 0.0)),
                ("press", GlowResponse::new(0.35, 1.0)),
            ];
            for (i, (name, state)) in states.into_iter().enumerate() {
                let center = Pos2::new(column(0.0) + i as f32 * 84.0, rect.bottom() - 24.0);
                paint::glow_circle(painter, center, 5.0, NEON_MAGENTA, GLOW_SM, state);
                painter.circle_filled(center, 5.0, NEON_MAGENTA);
                painter.text(center + Vec2::new(0.0, 18.0), Align2::CENTER_TOP, name, mono(10.0), TEXT_3);
            }
        },
    );
}

fn caption(painter: &egui::Painter, x: f32, y: f32, name: &str, note: &str) {
    painter.text(Pos2::new(x - 30.0, y), Align2::LEFT_TOP, name, mono(11.0), TEXT_2);
    painter.text(Pos2::new(x - 30.0, y + 14.0), Align2::LEFT_TOP, note, body_sm(), TEXT_3);
}

// ── 6. why the hairline exists ──────────────────────────────────────────────

fn hairline_demo(ui: &mut Ui, motion: &mut Motion, state: &mut BoardState) {
    section_rule(ui, "Hairline on pure black", Some("v1.1 reverses v1.0's \"no borders\""));
    ui.label(
        RichText::new("#000000 → #0A0A0A is barely visible on a cheap panel, so every card also carries a 1px rgba(255,255,255,0.08) edge.")
            .font(body_sm())
            .color(TEXT_2),
    );
    ui.add_space(SP_12);
    ui.horizontal(|ui| {
        let on = state.hairline_on;
        GlassCard::new(Vec2::new(260.0, 120.0))
            .salt(90)
            .hairline(false)
            .interactive(false)
            .show(ui, motion, |ui, _, _| {
                ui.label(RichText::new("no hairline").font(h3()).color(TEXT_1));
                ui.label(RichText::new("the card edge dissolves into the black").font(body_sm()).color(TEXT_2));
            });
        GlassCard::new(Vec2::new(260.0, 120.0))
            .salt(91)
            .hairline(true)
            .accent(NEON_ORANGE)
            .selected(on)
            .show(ui, motion, |ui, _, _| {
                ui.label(RichText::new("1px hairline 0.08").font(h3()).color(if on { NEON_ORANGE } else { TEXT_1 }));
                ui.label(RichText::new("the edge reads instantly; hover lights it with the accent").font(body_sm()).color(TEXT_2));
            });
        if ui.button("toggle selected").clicked() {
            state.hairline_on = !state.hairline_on;
        }
    });
}

// ── 7. type ladder ──────────────────────────────────────────────────────────

fn typography(ui: &mut Ui) {
    section_rule(ui, "Typography", Some("Inter Variable · wght axis · display 88pt"));
    ui.label(
        RichText::new("v1.1: primary text is pure #FFFFFF; secondary/tertiary are white at reduced alpha, not grey.")
            .font(body_sm())
            .color(TEXT_2),
    );
    ui.add_space(SP_12);

    let rows: [(&str, FontId, &str, Color32); 9] = [
        ("display 88 / 200", display(), "Collection", TEXT_1),
        ("h1 44 / 300", h1(), "Recently added", TEXT_1),
        ("h2 28 / 400", h2(), "Kid A", TEXT_1),
        ("h3 19 / 600", h3(), "Everything In Its Right Place", TEXT_1),
        ("body 15 / 400", body(), "The quick brown fox jumps over the lazy dog", TEXT_1),
        ("body-sm 13 / 400", body_sm(), "9 tracks · 74.2 MB · AAC 256", TEXT_2),
        ("label 11 / 600 +8%", label(), "NOW PLAYING", TEXT_3),
        ("italic 15 / 300", subtitle(), "an italic second voice, a Metro habit", TEXT_2),
        ("mono 13 tabular", mono(MONO), "01:23:45   12.4 GB   1 048 576", TEXT_2),
    ];
    for (name, font, sample, color) in rows {
        let sample = if name.starts_with("label") { tracked(sample) } else { sample.to_owned() };
        let galley = ui.painter().layout_no_wrap(sample, font, color);
        let height = (galley.size().y + SP_8).max(22.0);
        let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), height), Sense::hover());
        let painter = ui.painter();
        painter.text(Pos2::new(rect.left(), rect.center().y), Align2::LEFT_CENTER, name, mono(11.0), TEXT_3);
        painter.galley(Pos2::new(rect.left() + 190.0, rect.center().y - galley.size().y / 2.0), galley, color);
    }

    ui.add_space(SP_24);
    ui.label(RichText::new(tracked("font proof")).font(label()).color(TEXT_3));
    font_proof(ui);
}

fn font_proof(ui: &mut Ui) {
    let samples = [
        ("xl 200", FontId::new(20.0, eframe::egui::FontFamily::Name(F_XLIGHT.into()))),
        ("light 300", FontId::new(20.0, eframe::egui::FontFamily::Name(F_LIGHT.into()))),
        ("reg 400", FontId::new(20.0, eframe::egui::FontFamily::Name(F_REG.into()))),
        ("semi 600", FontId::new(20.0, eframe::egui::FontFamily::Name(F_SEMI.into()))),
        ("bold 700", FontId::new(20.0, eframe::egui::FontFamily::Name(F_BOLD.into()))),
        ("italic 300", FontId::new(20.0, eframe::egui::FontFamily::Name(F_LIGHT_ITALIC.into()))),
        ("mono", mono(20.0)),
    ];
    let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 26.0), Sense::hover());
    let painter = ui.painter();
    let mut x = rect.left();
    for (_name, font) in samples {
        let galley = painter.layout_no_wrap("Zune 0123".to_owned(), font, TEXT_1);
        painter.galley(Pos2::new(x, rect.center().y - galley.size().y / 2.0), galley.clone(), TEXT_1);
        x += galley.size().x + SP_12;
    }
}

// ── 8. spacing & radius ─────────────────────────────────────────────────────

fn spacing_and_radius(ui: &mut Ui, motion: &mut Motion) {
    section_rule(ui, "Spacing & Radius", Some("8pt grid · 4 8 12 16 24 32 48 64 96"));
    for value in SPACING_SCALE {
        let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 16.0), Sense::hover());
        let painter = ui.painter();
        let bar = Rect::from_min_size(Pos2::new(rect.left(), rect.center().y - 4.0), Vec2::new(value * 2.0, 8.0));
        paint::glow_rect(painter, bar, r_pill(), NEON_ORANGE, GLOW_SM, GlowResponse::IDLE);
        painter.rect_filled(bar, r_pill(), NEON_ORANGE);
        painter.text(Pos2::new(bar.right() + SP_12, rect.center().y), Align2::LEFT_CENTER, format!("{value:.0}"), mono(11.0), TEXT_3);
    }
    ui.add_space(SP_16);
    ui.horizontal(|ui| {
        for (label, radius) in [("8", r_sm()), ("14", r_md()), ("20", r_lg()), ("28", r_xl()), ("pill", r_pill())] {
            let (rect, _) = ui.allocate_exact_size(Vec2::new(84.0, 56.0), Sense::hover());
            let painter = ui.painter();
            painter.rect_filled(rect, radius, SURFACE_1);
            paint::hairline(painter, rect, radius);
            painter.text(rect.center(), Align2::CENTER_CENTER, label, mono(11.0), TEXT_2);
        }
    });
    let _ = motion;
}

// ── 9. motion ───────────────────────────────────────────────────────────────

fn motion_section(ui: &mut Ui, motion: &mut Motion, state: &mut BoardState) {
    section_rule(ui, "Motion", Some("80 / 160 / 240 / 400 ms · spring 180 / 22"));

    let target = state.spring_target;
    let spring_value = motion.spring(&mut state.curve_spring);
    let eased_value = motion.eased(&mut state.eased);

    let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 78.0), Sense::hover());
    let painter = ui.painter();
    for (i, (label, value, color)) in [("spring 180/22", spring_value, NEON_ORANGE), ("enter 400ms", eased_value, NEON_CYAN)]
        .into_iter()
        .enumerate()
    {
        let y = rect.top() + 14.0 + i as f32 * 34.0;
        let track = Rect::from_min_max(Pos2::new(rect.left() + 150.0, y - 5.0), Pos2::new(rect.right() - SP_16, y + 5.0));
        painter.rect_filled(track, r_pill(), SURFACE_1);
        paint::hairline(painter, track, r_pill());
        let x = track.left() + (track.width() - 12.0) * value.clamp(0.0, 1.0);
        let knob = Rect::from_center_size(Pos2::new(x + 6.0, y), Vec2::new(12.0, 12.0));
        paint::glow_circle(painter, knob.center(), 6.0, color, GLOW_MD, GlowResponse::IDLE);
        painter.rect_filled(knob, r_pill(), color);
        painter.text(Pos2::new(rect.left(), y), Align2::LEFT_CENTER, label, mono(11.0), TEXT_3);
    }

    ui.horizontal(|ui| {
        if ui.button("Trigger").clicked() {
            state.spring_target = if state.spring_target > 0.5 { 0.0 } else { 1.0 };
            state.eased.set(state.spring_target, T_SLOW, Bezier::ENTER);
        }
        ui.label(
            RichText::new("the spring overshoots, the curve does not — both stop repainting when settled")
                .font(body_sm())
                .color(TEXT_3),
        );
    });

    ui.add_space(SP_16);
    let (rect, _) = ui.allocate_exact_size(Vec2::new(260.0, 170.0), Sense::hover());
    let painter = ui.painter();
    painter.rect_filled(rect, r_md(), SURFACE_1);
    paint::hairline(painter, rect, r_md());
    let plot = rect.shrink2(Vec2::new(SP_16, SP_12));
    painter.rect_stroke(plot, r_sm(), eframe::egui::Stroke::new(1.0, DIVIDER), eframe::egui::StrokeKind::Inside);
    // Two neons only in this section: the spring (orange) and the eased value (cyan).
    // The exit curve stays neutral so the section does not become a colour sampler.
    for (curve, color) in [(Bezier::STANDARD, TEXT_2), (Bezier::ENTER, NEON_ORANGE), (Bezier::EXIT, TEXT_3)] {
        let points: Vec<Pos2> = (0..=40)
            .map(|i| {
                let t = i as f32 / 40.0;
                Pos2::new(plot.left() + plot.width() * t, plot.bottom() - plot.height() * curve.ease(t))
            })
            .collect();
        painter.add(egui::Shape::line(points, eframe::egui::Stroke::new(1.5, color)));
    }
    let mut x = rect.left();
    let legend_y = rect.bottom() - SP_4;
    for (color, label) in [(TEXT_2, "standard"), (NEON_ORANGE, "enter"), (TEXT_3, "exit")] {
        painter.circle_filled(Pos2::new(x + 3.0, legend_y - 4.0), 3.0, color);
        let galley = painter.layout_no_wrap(label.to_owned(), mono(10.0), color);
        painter.galley(Pos2::new(x + 10.0, legend_y - 4.0 - galley.size().y / 2.0), galley.clone(), color);
        x += 10.0 + galley.size().x + SP_12;
    }
    painter.text(Pos2::new(rect.right() - SP_8, rect.bottom() - SP_8), Align2::RIGHT_BOTTOM, "cubic-bezier", mono(10.0), TEXT_3);
    let _ = target;
}

// ── 10. components (≤2 neons in view, one per component) ────────────────────

fn components(ui: &mut Ui, motion: &mut Motion, state: &mut BoardState) {
    // ── GlassCard: brand orange ──
    section_rule(ui, "GlassCard", Some("hairline 0.08 · hover lift 2px · accent glow"));
    ui.horizontal(|ui| {
        GlassCard::new(Vec2::new(190.0, 116.0)).salt(1).accent(NEON_ORANGE).show(ui, motion, |ui, _, _| {
            ui.label(RichText::new("Idle").font(h3()).color(TEXT_1));
            ui.label(RichText::new("surface-1 + hairline").font(body_sm()).color(TEXT_2));
        });
        GlassCard::new(Vec2::new(190.0, 116.0)).salt(2).accent(NEON_ORANGE).show(ui, motion, |ui, _, _| {
            ui.label(RichText::new("Hover me").font(h3()).color(TEXT_1));
            ui.label(RichText::new("lift + edge lights up").font(body_sm()).color(TEXT_2));
        });
        let selected = state.card_selected;
        let response = GlassCard::new(Vec2::new(190.0, 116.0))
            .salt(3)
            .accent(NEON_ORANGE)
            .selected(selected)
            .show(ui, motion, |ui, _, _| {
                ui.label(RichText::new(if selected { "Selected" } else { "Click me" }).font(h3()).color(if selected { NEON_ORANGE } else { TEXT_1 }));
                ui.label(RichText::new("neon edge + glow-md").font(body_sm()).color(TEXT_2));
            });
        if response.clicked() {
            state.card_selected = !state.card_selected;
        }
        GlassCard::new(Vec2::new(190.0, 116.0)).salt(4).raised(true).accent(NEON_ORANGE).interactive(false).show(ui, motion, |ui, _, _| {
            ui.label(RichText::new("Raised").font(h3()).color(TEXT_1));
            ui.label(RichText::new("surface-2 #141414").font(body_sm()).color(TEXT_2));
        });
    });

    ui.add_space(SP_32);
    // ── DevicePill: the status palette (lime + amber) ──
    section_rule(ui, "DevicePill", Some("status colours are neon and glow · lime / amber"));
    ui.horizontal_wrapped(|ui| {
        device_pill(ui, motion, 1, PillState::Connected, "Zune 30 · 12.4 GB free");
        device_pill(ui, motion, 2, PillState::Transferring, "Kid A · 3 of 9 tracks");
        device_pill(ui, motion, 3, PillState::Connecting, "opening MTP session");
        device_pill(ui, motion, 4, PillState::Disconnected, "");
    });

    ui.add_space(SP_32);
    // ── IconButton / CTA: brand orange ──
    section_rule(ui, "IconButton", Some("1.5px strokes · neon icon + glow-sm · CTA glow-lg"));
    ui.horizontal(|ui| {
        IconButton::new().tip("Idle").show(ui, motion, 1, Icon::Note);
        IconButton::new().active(true).accent(NEON_ORANGE).tip("Selected").show(ui, motion, 2, Icon::Note);
        IconButton::new().ghost(true).tip("Ghost").show(ui, motion, 3, Icon::Upload);
        IconButton::new().enabled(false).tip("Disabled").show(ui, motion, 4, Icon::Trash);
        IconButton::new().size(44.0).icon_size(24.0).tip("Large").show(ui, motion, 5, Icon::Play);
        ui.add_space(SP_16);
        PrimaryButton::new("Push to Zune").accent(NEON_ORANGE).show(ui, motion, 6);
        PrimaryButton::new("Disabled").enabled(false).show(ui, motion, 7);
    });
    ui.add_space(SP_12);
    ui.horizontal_wrapped(|ui| {
        for (index, icon) in Icon::ALL.into_iter().enumerate() {
            IconButton::new()
                .size(34.0)
                .icon_size(18.0)
                .ghost(true)
                .tip(icon.name())
                .show(ui, motion, 100 + index as u64, icon);
        }
    });

    ui.add_space(SP_32);
    // ── ProgressRing: brand orange ──
    section_rule(ui, "ProgressRing", Some("one neon per component · sweep + glow-md · text-glow"));
    ui.horizontal(|ui| {
        ProgressRing::new(0.24, "12.4", "GB free").accent(NEON_ORANGE).show(ui, motion, 1);
        ProgressRing::new(state.ring_fraction, "18.9", "GB used").accent(NEON_ORANGE).show(ui, motion, 2);
        ProgressRing::new(1.0, "100", "% full").size(120.0).accent(NEON_ORANGE).show(ui, motion, 3);
        ui.vertical(|ui| {
            ui.add_space(SP_12);
            ui.label(RichText::new("Drag the sweep").font(body_sm()).color(TEXT_2));
            let mut fraction = state.ring_fraction;
            if ui.add(egui::Slider::new(&mut fraction, 0.0..=1.0).show_value(false)).changed() {
                state.ring_fraction = fraction;
            }
        });
    });

    ui.add_space(SP_32);
    // ── Transfer (amber) + Tracks (lime): the two signatures in view ──
    section_rule(ui, "Progress bar & TrackRow", Some("transfer = amber · tracks = lime"));
    ui.vertical(|ui| {
        ui.set_max_width(560.0);
        GradientBar::new(state.ring_fraction).accent(NEON_AMBER).label("62%").show(ui, motion, 1);
        ui.add_space(SP_16);
        GradientBar::new(0.35).height(4.0).active(true).accent(NEON_AMBER).label("transferring").show(ui, motion, 2);
        ui.add_space(SP_24);
        track_row(ui, motion, 1, TrackRow::new("Everything In Its Right Place", "Radiohead · Kid A").index(1).trailing("4:11").accent(NEON_LIME));
        let selected = state.row_selected;
        let response = track_row(
            ui,
            motion,
            2,
            TrackRow::new("Kid A", "Radiohead · Kid A").index(2).trailing("4:44").selected(selected).accent(NEON_LIME),
        );
        if response.clicked() {
            state.row_selected = !state.row_selected;
        }
        track_row(
            ui,
            motion,
            3,
            TrackRow::new("The National Anthem", "Radiohead · Kid A").index(3).trailing("5:51").playing(state.playing).accent(NEON_LIME),
        );
        if ui.button(if state.playing { "stop the indicator" } else { "animate the indicator" }).clicked() {
            state.playing = !state.playing;
        }
        track_row(ui, motion, 4, TrackRow::new("Treefingers", "Radiohead · Kid A").trailing("3:42").dense().accent(NEON_LIME));
    });
}

// ── 11. composition ─────────────────────────────────────────────────────────

fn composition(ui: &mut Ui, motion: &mut Motion) {
    section_rule(ui, "Composition", Some("canvas #000 → surface-1 → surface-2, one signature colour"));
    ui.horizontal(|ui| {
        GlassCard::new(Vec2::new(370.0, 226.0))
            .salt(20)
            .accent(NEON_CYAN)
            .interactive(false)
            .show(ui, motion, |ui, _, motion| {
                ui.label(RichText::new(tracked("Albums")).font(label()).color(NEON_CYAN));
                ui.add_space(SP_8);
                ui.label(RichText::new("Recently added").font(h2()).color(TEXT_1));
                ui.add_space(SP_12);
                track_row(ui, motion, 21, TrackRow::new("Idioteque", "Radiohead").index(8).trailing("5:09").accent(NEON_CYAN));
                track_row(ui, motion, 22, TrackRow::new("Optimistic", "Radiohead").index(6).trailing("5:16").accent(NEON_CYAN));
            });

        GlassCard::new(Vec2::new(310.0, 226.0))
            .salt(23)
            .raised(true)
            .accent(NEON_CYAN)
            .interactive(false)
            .show(ui, motion, |ui, _, motion| {
                ui.label(RichText::new(tracked("Storage")).font(label()).color(TEXT_3));
                ui.add_space(SP_8);
                ui.horizontal(|ui| {
                    ProgressRing::new(0.62, "12.4", "GB free").accent(NEON_CYAN).show(ui, motion, 24);
                    ui.vertical(|ui| {
                        ui.add_space(SP_16);
                        ui.label(RichText::new("Zune 30").font(h3()).color(TEXT_1));
                        ui.label(RichText::new("18.9 GB used of 31.3 GB").font(body_sm()).color(TEXT_2));
                        ui.add_space(SP_8);
                        device_pill(ui, motion, 25, PillState::Connected, "trusted");
                    });
                });
            });
    });
    ui.add_space(SP_16);
    ui.label(
        RichText::new("The page owns one neon (Albums → cyan here); the pill's status lime/amber is the only other colour allowed on screen.")
            .font(subtitle())
            .color(TEXT_3),
    );
}
