//! `GlassCard` — the surface everything sits on.
//!
//! v1.1 changes the rule here: on a pure black canvas the step from `#000000` to
//! `#0A0A0A` is not enough on its own, so **every card carries a 1px hairline**
//! (`rgba(255,255,255,0.08)`). Hover lifts the card ~2px and lights the hairline with
//! the accent plus a glow — never a black shadow, which would be invisible anyway.

use eframe::egui::{Color32, CornerRadius, Rect, Response, Sense, Ui, UiBuilder, Vec2};

use crate::design::anim::{self, Motion};
use crate::design::paint::{self, GlowResponse, GLOW_MD};
use crate::design::tokens::*;

pub struct GlassCard {
    size: Vec2,
    padding: Vec2,
    radius: CornerRadius,
    selected: bool,
    interactive: bool,
    /// Draw the brighter `surface-2` body (floating layers, menus).
    raised: bool,
    /// The accent this card lights up with on hover/selection.
    accent: Color32,
    /// Show the hairline. Off only for the board's "why the hairline exists" demo.
    hairline: bool,
    id_salt: u64,
}

impl GlassCard {
    pub fn new(size: Vec2) -> Self {
        Self {
            size,
            padding: Vec2::new(SP_16, SP_16),
            radius: r_lg(),
            selected: false,
            interactive: true,
            raised: false,
            accent: ACCENT,
            hairline: true,
            id_salt: 0,
        }
    }

    pub fn padding(mut self, padding: Vec2) -> Self {
        self.padding = padding;
        self
    }

    pub fn radius(mut self, radius: CornerRadius) -> Self {
        self.radius = radius;
        self
    }

    pub fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    /// Decorative cards do not react to the pointer.
    pub fn interactive(mut self, interactive: bool) -> Self {
        self.interactive = interactive;
        self
    }

    pub fn raised(mut self, raised: bool) -> Self {
        self.raised = raised;
        self
    }

    /// One neon per component (v1.1 §2.1.2) — usually the page signature.
    pub fn accent(mut self, accent: Color32) -> Self {
        self.accent = accent;
        self
    }

    /// Used by the board to show a card with and without its hairline.
    pub fn hairline(mut self, hairline: bool) -> Self {
        self.hairline = hairline;
        self
    }

    pub fn salt(mut self, salt: u64) -> Self {
        self.id_salt = salt;
        self
    }

    pub fn show(
        self,
        ui: &mut Ui,
        motion: &mut Motion,
        add: impl FnOnce(&mut Ui, Rect, &mut Motion),
    ) -> Response {
        let sense = if self.interactive { Sense::click() } else { Sense::hover() };
        let (rect, response) = ui.allocate_exact_size(self.size, sense);

        let id = ui.id().with(("glass-card", self.id_salt, response.id));
        let hovered = response.hovered() && self.interactive;
        let hover = anim::spring_at(ui, id.with("hover"), hovered as u8 as f32, motion);
        let select = anim::spring_at(ui, id.with("select"), self.selected as u8 as f32, motion);
        let press = anim::spring_at(
            ui,
            id.with("press"),
            (response.is_pointer_button_down_on() && self.interactive) as u8 as f32,
            motion,
        );

        // Press pulls the card back down a touch (spec: scale 0.97 / instant).
        let lift = hover * 2.0 - press * 1.0;
        let body = rect.translate(Vec2::new(0.0, -lift));
        let painter = ui.painter();

        let base = if self.raised { SURFACE_2 } else { SURFACE_1 };
        painter.rect_filled(body, self.radius, base);

        // The accent lights up the edge on hover/selection; the glow is what gives
        // the card its presence on pure black.
        let lit = (hover * 0.5 + select).min(1.0);
        if self.hairline {
            if lit > 0.01 {
                paint::hairline_lit(
                    painter,
                    body,
                    self.radius,
                    lit * 0.55,
                    self.accent,
                );
            } else {
                paint::hairline(painter, body, self.radius);
            }
        }
        if select > 0.01 || hover > 0.01 {
            paint::glow_rect(
                painter,
                body,
                self.radius,
                self.accent,
                GLOW_MD,
                GlowResponse::new(hover * 0.55, press),
            );
        }

        let inner = body.shrink2(self.padding);
        let mut child = ui.new_child(
            UiBuilder::new()
                .max_rect(inner)
                .layout(eframe::egui::Layout::top_down(eframe::egui::Align::Min)),
        );
        add(&mut child, inner, motion);
        response
    }
}

/// Convenience for the common case.
pub fn glass_card(
    ui: &mut Ui,
    motion: &mut Motion,
    salt: u64,
    size: Vec2,
    selected: bool,
    accent: Color32,
    add: impl FnOnce(&mut Ui, Rect, &mut Motion),
) -> Response {
    GlassCard::new(size)
        .salt(salt)
        .selected(selected)
        .accent(accent)
        .show(ui, motion, add)
}
