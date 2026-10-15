//! `DevicePill` — the device state capsule in the top bar.
//!
//! 32px tall, pill radius, glass body, an 8px status dot that breathes while a
//! transfer runs. Disconnected collapses to `text-3` and stays clickable.

use eframe::egui::{Color32, Response, Sense, Ui, Vec2};

use crate::design::anim::{self, pulse, Motion};
use crate::design::{paint, tokens::*};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum PillState {
    /// Found, trusted, idle.
    Connected,
    /// A transfer is running (the dot pulses).
    Transferring,
    Connecting,
    Disconnected,
}

impl PillState {
    /// v1.1: a status colour is a neon, so it always carries a glow. Lime reads as
    /// "healthy", amber as "moving", and the disconnected state stays neutral so a
    /// dead device is not the loudest thing on screen.
    fn color(self) -> Color32 {
        match self {
            PillState::Connected => NEON_LIME,
            PillState::Transferring => NEON_AMBER,
            PillState::Connecting => NEON_AMBER,
            PillState::Disconnected => TEXT_2,
        }
    }

    fn glow(self) -> paint::Glow {
        match self {
            PillState::Disconnected => paint::GLOW_NONE,
            _ => paint::GLOW_SM,
        }
    }

    fn label(self) -> &'static str {
        match self {
            PillState::Connected => "CONNECTED",
            PillState::Transferring => "TRANSFERRING",
            PillState::Connecting => "CONNECTING",
            PillState::Disconnected => "NO DEVICE",
        }
    }
}

/// Draw the pill. `detail` is usually "Zune 30 · 12.4 GB free".
pub fn device_pill(
    ui: &mut Ui,
    motion: &mut Motion,
    id_salt: u64,
    state: PillState,
    detail: &str,
) -> Response {
    let padding = Vec2::new(SP_16, 0.0);
    let label_font = label();
    let detail_font = body_sm();

    let label_text = tracked(state.label());
    let detail_galley = ui
        .painter()
        .layout_no_wrap(detail.to_owned(), detail_font, TEXT_2);
    let label_galley = ui
        .painter()
        .layout_no_wrap(label_text.clone(), label_font.clone(), state.color());

    let dot = 8.0;
    let width = padding.x * 2.0
        + dot
        + SP_8
        + label_galley.size().x
        + if detail.is_empty() { 0.0 } else { SP_12 + 2.0 + detail_galley.size().x };
    let size = Vec2::new(width, 32.0);

    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    let id = ui.id().with(("device-pill", id_salt, response.id));
    let hover = anim::spring_at(ui, id.with("hover"), response.hovered() as u8 as f32, motion);

    let painter = ui.painter();
    let body = surface_for(state);
    painter.rect_filled(rect, r_pill(), body);
    if hover > 0.01 {
        paint::glow_rect(
            painter,
            rect,
            r_pill(),
            state.color(),
            paint::GLOW_SM,
            paint::GlowResponse::hovered(hover * 0.6),
        );
    }
    // The hairline is what keeps the pill readable on pure black.
    paint::hairline(painter, rect, r_pill());

    // Status dot — pulsing while transferring.
    let dot_center = eframe::egui::pos2(rect.left() + padding.x + dot / 2.0, rect.center().y);
    let glow_alpha = if state == PillState::Transferring {
        motion.keep_animating();
        0.35 + 0.65 * pulse(motion.time(), PULSE_PERIOD)
    } else {
        1.0
    };
    paint::glow_circle(
        painter,
        dot_center,
        dot / 2.0,
        state.color(),
        state.glow(),
        paint::GlowResponse::hovered(glow_alpha * 0.35),
    );
    painter.circle_filled(dot_center, dot / 2.0, state.color());

    let mut x = dot_center.x + dot / 2.0 + SP_8;
    painter.galley(
        eframe::egui::pos2(x, rect.center().y - label_galley.size().y / 2.0),
        label_galley.clone(),
        state.color(),
    );
    x += label_galley.size().x;
    if !detail.is_empty() {
        x += SP_12;
        painter.line_segment(
            [
                eframe::egui::pos2(x - SP_8, rect.center().y - 8.0),
                eframe::egui::pos2(x - SP_8, rect.center().y + 8.0),
            ],
            eframe::egui::Stroke::new(1.0, DIVIDER),
        );
        let color = if state == PillState::Disconnected { TEXT_3 } else { TEXT_2 };
        painter.galley(
            eframe::egui::pos2(x, rect.center().y - detail_galley.size().y / 2.0),
            detail_galley.clone(),
            color,
        );
    }
    // A tiny chevron/spinner hint on the right, only when there is something to do.
    if state == PillState::Connecting {
        motion.keep_animating();
        let angle = motion.time() * 2.4;
        let center = eframe::egui::pos2(rect.right() - SP_16, rect.center().y);
        let r = 5.5;
        let from = angle;
        let to = angle + 3.6;
        let points: Vec<_> = (0..=10)
            .map(|i| {
                let t = from + (to - from) * (i as f32 / 10.0);
                eframe::egui::pos2(center.x + r * t.cos(), center.y + r * t.sin())
            })
            .collect();
        painter.add(eframe::egui::Shape::line(
            points,
            eframe::egui::Stroke::new(1.5, NEON_AMBER),
        ));
    }

    response.on_hover_text(match state {
        PillState::Disconnected => "No Zune found on USB — plug it in, or press Connect on the Device page.",
        _ => detail,
    })
}

fn surface_for(state: PillState) -> Color32 {
    match state {
        PillState::Disconnected => SURFACE_1,
        _ => SURFACE_2,
    }
}
