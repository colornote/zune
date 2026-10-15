//! `IconButton` and `PrimaryButton`.
//!
//! v1.1: a neon element is always glowing, and no component mixes two neons — so
//! each button takes **one** accent and renders it as icon + glow (`glow-sm` for an
//! icon button, `glow-lg` for the primary call to action). Press scale stays 0.97.

use eframe::egui::{Color32, Response, Sense, Ui, Vec2};

use crate::design::anim::{self, Lerp, Motion};
use crate::design::paint::{self, GlowResponse, GLOW_LG, GLOW_SM};
use crate::design::tokens::*;
use crate::ui::icons::Icon;

pub struct IconButton {
    size: f32,
    icon_size: f32,
    active: bool,
    enabled: bool,
    tip: Option<String>,
    ghost: bool,
    accent: Color32,
}

impl IconButton {
    pub fn new() -> Self {
        Self {
            size: 36.0,
            icon_size: 20.0,
            active: false,
            enabled: true,
            tip: None,
            ghost: false,
            accent: ACCENT,
        }
    }

    pub fn size(mut self, size: f32) -> Self {
        self.size = size;
        self
    }

    pub fn icon_size(mut self, icon_size: f32) -> Self {
        self.icon_size = icon_size;
        self
    }

    /// The selected state: neon icon, neon indicator, glow.
    pub fn active(mut self, active: bool) -> Self {
        self.active = active;
        self
    }

    pub fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    pub fn tip(mut self, tip: impl Into<String>) -> Self {
        self.tip = Some(tip.into());
        self
    }

    /// No background at all until hover (used inside list rows).
    pub fn ghost(mut self, ghost: bool) -> Self {
        self.ghost = ghost;
        self
    }

    /// The single neon this button is allowed to use.
    pub fn accent(mut self, accent: Color32) -> Self {
        self.accent = accent;
        self
    }

    pub fn show(self, ui: &mut Ui, motion: &mut Motion, id_salt: u64, icon: Icon) -> Response {
        let (rect, response) = ui.allocate_exact_size(Vec2::splat(self.size), Sense::click());
        let id = ui.id().with(("icon-button", id_salt, response.id));

        let hovered = (response.hovered() || response.has_focus()) && self.enabled;
        let hover = anim::spring_at(ui, id.with("hover"), hovered as u8 as f32, motion);
        let press = anim::spring_at(
            ui,
            id.with("press"),
            (response.is_pointer_button_down_on() && self.enabled) as u8 as f32,
            motion,
        );
        let active = anim::spring_at(ui, id.with("active"), self.active as u8 as f32, motion);

        // Press scales the whole button down (instant, per the spec).
        let scale = 1.0 - 0.03 * press;
        let body = eframe::egui::Rect::from_center_size(rect.center(), rect.size() * scale);
        let painter = ui.painter();

        let lit = (hover * 0.9 + active).min(1.0);
        let neon = self.active || active > 0.5;
        if neon {
            // Small area, high saturation, always glowing.
            paint::glow_rect(
                painter,
                body,
                r_sm(),
                self.accent,
                GLOW_SM,
                GlowResponse::new(hover, press),
            );
            painter.rect_filled(body, r_sm(), paint::with_alpha(self.accent, 0.12 + 0.06 * hover));
            paint::hairline_lit(painter, body, r_sm(), 0.35 + 0.25 * hover, self.accent);
        } else if (!self.ghost || lit > 0.01) && self.enabled {
            painter.rect_filled(body, r_sm(), SURFACE_2);
            if lit > 0.01 {
                paint::hairline_lit(painter, body, r_sm(), 0.5 * lit, Color32::WHITE);
            }
        }

        let color = if !self.enabled {
            TEXT_3
        } else if neon {
            self.accent
        } else {
            TEXT_2.lerp(TEXT_1, hover)
        };
        // The icon itself is the neon element, so it glows (glow-sm, per spec).
        if neon && self.enabled {
            let icon_rect =
                eframe::egui::Rect::from_center_size(body.center(), Vec2::splat(self.icon_size * scale));
            paint::glow_circle(
                painter,
                body.center(),
                self.icon_size * 0.35 * scale,
                self.accent,
                GLOW_SM,
                GlowResponse::new(hover, press),
            );
            icon.draw(painter, icon_rect, color);
        } else {
            let icon_rect =
                eframe::egui::Rect::from_center_size(body.center(), Vec2::splat(self.icon_size * scale));
            icon.draw(painter, icon_rect, color);
        }

        let response = if self.enabled { response } else { response.on_hover_text("unavailable") };
        match self.tip {
            Some(tip) => response.on_hover_text(tip),
            None => response,
        }
    }
}

impl Default for IconButton {
    fn default() -> Self {
        Self::new()
    }
}

/// A pill-shaped text button: the primary call to action, `glow-lg`.
pub struct PrimaryButton {
    label: String,
    width: f32,
    enabled: bool,
    accent: Color32,
}

impl PrimaryButton {
    pub fn new(label: impl Into<String>) -> Self {
        Self { label: label.into(), width: 0.0, enabled: true, accent: ACCENT }
    }

    pub fn width(mut self, width: f32) -> Self {
        self.width = width;
        self
    }

    pub fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    pub fn accent(mut self, accent: Color32) -> Self {
        self.accent = accent;
        self
    }

    pub fn show(self, ui: &mut Ui, motion: &mut Motion, id_salt: u64) -> Response {
        let font = eframe::egui::FontId::new(BODY, eframe::egui::FontFamily::Name(F_SEMI.into()));
        let galley = ui.painter().layout_no_wrap(self.label.clone(), font, TEXT_INV);
        let width = if self.width > 0.0 {
            self.width
        } else {
            galley.size().x + SP_32
        };
        let height = 36.0;
        let (rect, response) = ui.allocate_exact_size(Vec2::new(width, height), Sense::click());
        let id = ui.id().with(("primary-button", id_salt, response.id));

        let hovered = response.hovered() && self.enabled;
        let hover = anim::spring_at(ui, id.with("hover"), hovered as u8 as f32, motion);
        let press = anim::spring_at(
            ui,
            id.with("press"),
            (response.is_pointer_button_down_on() && self.enabled) as u8 as f32,
            motion,
        );

        let scale = 1.0 - 0.03 * press;
        let body =
            eframe::egui::Rect::from_center_size(rect.center(), Vec2::new(width * scale, height * scale));
        let painter = ui.painter();

        if self.enabled {
            // A single neon fill, which is allowed because it is a small area and it
            // carries glow-lg. No gradient: one component, one colour (v1.1).
            paint::neon_fill(
                painter,
                body,
                r_pill(),
                self.accent,
                GLOW_LG,
                GlowResponse::new(hover, press),
            );
        } else {
            painter.rect_filled(body, r_pill(), SURFACE_2);
            paint::hairline(painter, body, r_pill());
        }
        let text_color = if self.enabled { TEXT_INV } else { TEXT_3 };
        painter.galley(body.center() - galley.size() / 2.0, galley, text_color);
        response
    }
}
