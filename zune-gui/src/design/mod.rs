//! The design system: tokens, fonts, motion and painting primitives.
//!
//! `UI_DESIGN.md` is the contract; everything visual must come from here so the
//! components stay consistent and the board can audit them.

pub mod anim;
pub mod fonts;
pub mod paint;
pub mod tokens;

/// Install fonts and style into a fresh context.
pub fn install(ctx: &eframe::egui::Context) {
    fonts::install(ctx);
    tokens::install(ctx);
}
