//! `ProgressRing` — the 96px capacity ring.
//!
//! v1.1: one component, one neon. The v1.0 orange→amber gradient is gone (that
//! mixed two neons); the sweep is a single page-accent colour carrying `glow-md`,
//! and the centre figure is a neon number with `text-glow`.

use eframe::egui::{Align2, Color32, Pos2, Response, Sense, Shape, Stroke, Ui, Vec2};

use crate::design::anim::{self, Motion};
use crate::design::paint::{self, GlowResponse, GLOW_MD, GLOW_SM, GLOW_TEXT, GLOW_NONE};
use crate::design::tokens::*;

pub struct ProgressRing<'a> {
    /// 0..=1
    pub fraction: f32,
    pub headline: &'a str,
    pub caption: &'a str,
    pub size: f32,
    pub thickness: f32,
    /// The one neon this ring uses.
    pub accent: Color32,
    /// Glow the centre figure with `text-glow`.
    pub glow_text: bool,
}

impl<'a> ProgressRing<'a> {
    pub fn new(fraction: f32, headline: &'a str, caption: &'a str) -> Self {
        Self {
            fraction,
            headline,
            caption,
            size: 96.0,
            thickness: 6.0,
            accent: NEON_ORANGE,
            glow_text: true,
        }
    }

    pub fn size(mut self, size: f32) -> Self {
        self.size = size;
        self
    }

    pub fn thickness(mut self, thickness: f32) -> Self {
        self.thickness = thickness;
        self
    }

    pub fn accent(mut self, accent: Color32) -> Self {
        self.accent = accent;
        self
    }

    pub fn show(self, ui: &mut Ui, motion: &mut Motion, salt: u64) -> Response {
        let (rect, response) = ui.allocate_exact_size(Vec2::splat(self.size), Sense::hover());
        let id = ui.id().with(("progress-ring", salt, response.id));

        // The sweep animates; once it settles nothing requests a repaint.
        let animated: f32 =
            anim::anim_at::<f32>(ui, id.with("fraction"), self.fraction.clamp(0.0, 1.0), motion);

        let center = rect.center();
        let radius = (self.size - self.thickness) / 2.0;
        let painter = ui.painter();

        // Track: a hairline-bright ring, no colour.
        painter.circle_stroke(center, radius, Stroke::new(self.thickness, SURFACE_2));
        painter.circle_stroke(
            center,
            radius + self.thickness / 2.0,
            Stroke::new(1.0, HAIRLINE),
        );

        let sweep = 360.0 * animated;
        if sweep > 0.1 {
            let start_deg = -90.0_f32;
            let segments = 96;
            // The glow is drawn with the arc (one colour, always glowing).
            for i in 0..segments {
                let t0 = i as f32 / segments as f32 * sweep;
                if t0 >= sweep {
                    break;
                }
                let t1 = ((i + 1) as f32 / segments as f32 * sweep).min(sweep);
                let p0 = point(center, radius, start_deg + t0);
                let p1 = point(center, radius, start_deg + t1);
                let angle = (i as f32 / segments as f32) * std::f32::consts::TAU;
                let glow_alpha = GLOW_MD.near_alpha
                    * (0.55 + 0.45 * (1.0 - i as f32 / segments as f32))
                    * (4.0 / 20.0);
                painter.circle_filled(
                    p0,
                    self.thickness * 1.6,
                    alpha_of(self.accent, glow_alpha * 0.5),
                );
                let _ = angle;
                painter.add(Shape::line(vec![p0, p1], Stroke::new(self.thickness, self.accent)));
            }
        }

        // Centre: tabular figure (neon, text-glow) + tracked caption.
        let headline_galley = painter.layout_no_wrap(self.headline.to_owned(), numeric(), self.accent);
        let headline_pos = center - Vec2::new(headline_galley.size().x / 2.0, 6.0 + headline_galley.size().y / 2.0);
        if self.glow_text {
            paint::glow_galley(
                painter,
                headline_pos,
                &headline_galley,
                self.accent,
                GLOW_TEXT,
                GlowResponse::IDLE,
            );
        }
        painter.galley(headline_pos, headline_galley, self.accent);
        painter.text(
            center + Vec2::new(0.0, 14.0),
            Align2::CENTER_CENTER,
            tracked(self.caption),
            label(),
            TEXT_3,
        );

        let _ = (GLOW_SM, GLOW_NONE);
        response
    }
}

fn point(center: Pos2, radius: f32, degrees: f32) -> Pos2 {
    center + Vec2::angled(degrees.to_radians()) * radius
}
