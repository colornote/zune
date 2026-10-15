//! `GradientBar` — the 4px progress bar.
//!
//! v1.1 kept the name but not the gradient: one component uses one neon, so the fill
//! is a single amber (transfer) or page-accent colour with `glow-md`, plus a breathing
//! head while a transfer is live.

use eframe::egui::{Align2, Color32, Pos2, Response, Sense, Ui, Vec2};

use crate::design::anim::{self, pulse, Motion};
use crate::design::paint::{self, GlowResponse, GLOW_MD};
use crate::design::tokens::*;

pub struct GradientBar<'a> {
    pub fraction: f32,
    pub height: f32,
    pub label: Option<&'a str>,
    /// Keep breathing while true (a transfer is running).
    pub active: bool,
    /// The one neon this bar uses.
    pub accent: Color32,
}

impl<'a> GradientBar<'a> {
    pub fn new(fraction: f32) -> Self {
        Self {
            fraction,
            height: 4.0,
            label: None,
            active: false,
            accent: NEON_AMBER,
        }
    }

    pub fn height(mut self, height: f32) -> Self {
        self.height = height;
        self
    }

    pub fn label(mut self, label: &'a str) -> Self {
        self.label = Some(label);
        self
    }

    pub fn active(mut self, active: bool) -> Self {
        self.active = active;
        self
    }

    pub fn accent(mut self, accent: Color32) -> Self {
        self.accent = accent;
        self
    }

    pub fn show(self, ui: &mut Ui, motion: &mut Motion, salt: u64) -> Response {
        let width = ui.available_width();
        let label_height = if self.label.is_some() { 18.0 } else { 0.0 };
        let (rect, response) =
            ui.allocate_exact_size(Vec2::new(width, self.height.max(label_height)), Sense::hover());
        let id = ui.id().with(("gradient-bar", salt, response.id));
        let animated: f32 =
            anim::anim_at::<f32>(ui, id.with("fraction"), self.fraction.clamp(0.0, 1.0), motion);

        let painter = ui.painter();
        let track = if self.label.is_some() {
            eframe::egui::Rect::from_min_size(
                Pos2::new(rect.left(), rect.bottom() - self.height),
                Vec2::new(width, self.height),
            )
        } else {
            eframe::egui::Rect::from_center_size(rect.center(), Vec2::new(width, self.height))
        };

        painter.rect_filled(track, r_pill(), SURFACE_2);
        let fill_width = width * animated;
        if fill_width > 0.5 {
            let fill =
                eframe::egui::Rect::from_min_size(track.left_top(), Vec2::new(fill_width, self.height));
            // One colour + glow-md: the bar is the neon element.
            paint::neon_fill(painter, fill, r_pill(), self.accent, GLOW_MD, GlowResponse::IDLE);
            if self.active {
                motion.keep_animating();
                // A breathing head, so a stalled transfer still reads as alive.
                let head = pulse(motion.time(), PULSE_PERIOD);
                paint::ambient_glow(
                    painter,
                    Pos2::new(fill.right() - self.height, track.center().y),
                    self.height * 5.0 + 8.0 * head,
                    self.accent,
                    0.30 + 0.20 * head,
                );
            }
        }
        if let Some(label) = self.label {
            painter.text(
                Pos2::new(rect.right(), rect.top()),
                Align2::RIGHT_TOP,
                label,
                mono(BODY_SM),
                TEXT_2,
            );
        }
        response
    }
}
