//! **Legacy compatibility shim.**
//!
//! The temporary theme written during stage 2 is replaced by `design::tokens`
//! (`UI_DESIGN.md` is the contract). The old pages still reference these names;
//! they are aliases, not values — nothing new should use them. Delete this file
//! when the P3/P4 pages are rebuilt on `ui::*` components.

use eframe::egui::{Color32, RichText, Ui};

#[allow(unused_imports)]
pub use crate::design::tokens::{
    ACCENT as ORANGE, ACCENT_LO as ORANGE_DIM, CANVAS as BG, ERR as RED, OK as GREEN, SURFACE_1 as PANEL,
    SURFACE_2 as CARD, SURFACE_2 as CARD_HOVER, TEXT_1 as TEXT, TEXT_2 as MUTED, WARN as YELLOW,
};

pub const FIELD: Color32 = crate::design::tokens::CANVAS;
pub const LINE: Color32 = crate::design::tokens::HAIRLINE;
/// The recorded row pitch is cosmetic. The real row height is whatever the layout
/// produces; this only has to be close enough for `show_rows` to virtualise, so keep
/// it exactly what `tracks_page` allocates per row.
pub const ROW_HEIGHT: f32 = 28.0;
pub const COLS: [(f32, &str); 7] = [
    (26.0, ""),
    (36.0, "#"),
    (250.0, "Title"),
    (180.0, "Artist"),
    (180.0, "Album"),
    (62.0, "Length"),
    (78.0, "Size"),
];

/// Kept so existing call sites compile; the real install is `design::install`.
pub fn apply(ctx: &eframe::egui::Context) {
    crate::design::install(ctx);
}

pub fn wordmark(ui: &mut Ui) {
    use crate::design::tokens::F_XLIGHT;
    // The wordmark is a neon element, so it carries a text glow.
    let (rect, _) = ui.allocate_exact_size(egui::vec2(64.0, 34.0), egui::Sense::hover());
    let painter = ui.painter();
    let galley = painter.layout_no_wrap(
        "zune".to_owned(),
        eframe::egui::FontId::new(30.0, eframe::egui::FontFamily::Name(F_XLIGHT.into())),
        ORANGE,
    );
    let pos = egui::pos2(rect.left(), rect.center().y - galley.size().y / 2.0);
    crate::design::paint::glow_galley(
        painter,
        pos,
        &galley,
        ORANGE,
        crate::design::paint::GLOW_TEXT,
        crate::design::paint::GlowResponse::IDLE,
    );
    painter.galley(pos, galley, ORANGE);
}

pub fn tag(ui: &mut Ui, text: &str, color: Color32) {
    // A tag is a small neon element: hairline in the same neon, no fill block.
    egui::Frame::NONE
        .fill(crate::design::paint::with_alpha(color, 0.10))
        .stroke(eframe::egui::Stroke::new(1.0, crate::design::paint::with_alpha(color, 0.45)))
        .corner_radius(crate::design::tokens::r_sm())
        .inner_margin(egui::Margin::symmetric(6, 2))
        .show(ui, |ui| {
            ui.label(RichText::new(text).color(color).size(12.0));
        });
}

pub fn dot_label(ui: &mut Ui, color: Color32, text: &str) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
    ui.painter().circle_filled(rect.center(), 4.0, color);
    ui.label(RichText::new(text).color(TEXT));
}

pub fn human_bytes(bytes: u64) -> String {
    crate::design::tokens::human_bytes(bytes)
}

pub fn album_tint(name: &str) -> Color32 {
    crate::design::tokens::album_tint(name)
}
