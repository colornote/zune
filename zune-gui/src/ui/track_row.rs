//! `TrackRow` — the 56px list row: flat, hover-lit, with an accent bar that grows
//! from the middle and an animated playing indicator.

use eframe::egui::{Align2, Color32, FontId, Pos2, Rect, Response, Sense, Ui, Vec2};

use crate::design::anim::{self, pulse, Lerp, Motion};
use crate::design::{paint, tokens::*};
use crate::ui::icons::Icon;

pub struct TrackRow<'a> {
    pub index: Option<u32>,
    pub title: &'a str,
    pub subtitle: &'a str,
    /// Right-aligned, tabular: duration or size.
    pub trailing: &'a str,
    pub selected: bool,
    pub playing: bool,
    pub height: f32,
    /// The page signature: one neon, inherited from the page (v1.1 §2.1.2).
    pub accent: Color32,
}

impl<'a> TrackRow<'a> {
    pub fn new(title: &'a str, subtitle: &'a str) -> Self {
        Self {
            index: None,
            title,
            subtitle,
            trailing: "",
            selected: false,
            playing: false,
            height: 56.0,
            accent: crate::design::tokens::NEON_LIME,
        }
    }

    pub fn index(mut self, index: u32) -> Self {
        self.index = Some(index);
        self
    }

    pub fn trailing(mut self, trailing: &'a str) -> Self {
        self.trailing = trailing;
        self
    }

    pub fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    pub fn playing(mut self, playing: bool) -> Self {
        self.playing = playing;
        self
    }

    /// The single neon this row may use (hover bar, playing bars, selection).
    pub fn accent(mut self, accent: Color32) -> Self {
        self.accent = accent;
        self
    }

    /// Compact variant for dense lists.
    pub fn dense(mut self) -> Self {
        self.height = 44.0;
        self
    }
}

pub fn track_row(ui: &mut Ui, motion: &mut Motion, salt: u64, row: TrackRow<'_>) -> Response {
    let width = ui.available_width();
    let (rect, response) = ui.allocate_exact_size(Vec2::new(width, row.height), Sense::click());
    let id = ui.id().with(("track-row", salt, response.id));

    let hover = anim::spring_at(ui, id.with("hover"), response.hovered() as u8 as f32, motion);
    let select = anim::spring_at(ui, id.with("select"), row.selected as u8 as f32, motion);

    let painter = ui.painter();
    let lit = (hover * 0.75 + select).min(1.0);
    if lit > 0.01 {
        painter.rect_filled(rect, r_md(), SURFACE_2);
    }

    // The accent bar grows out of the vertical centre and glows (glow-md).
    if row.playing || select > 0.01 || hover > 0.01 {
        let bar_h = rect.height() * 0.4 * (0.6 + 0.4 * lit);
        let bar = Rect::from_min_max(
            Pos2::new(rect.left() + 1.0, rect.center().y - bar_h / 2.0),
            Pos2::new(rect.left() + 3.5, rect.center().y + bar_h / 2.0),
        );
        paint::neon_fill(
            painter,
            bar,
            r_pill(),
            row.accent,
            paint::GLOW_MD,
            paint::GlowResponse::new(hover, 0.0),
        );
    }

    let left = rect.left() + SP_16;
    let text_color = if row.selected {
        row.accent.lerp(TEXT_1, 1.0 - select)
    } else {
        TEXT_1
    };
    let title_font = FontId::new(H3, eframe::egui::FontFamily::Name(F_SEMI.into()));
    let sub_font = body_sm();

    // Leading slot: number, or the playing bars.
    let slot = Rect::from_min_size(Pos2::new(left, rect.top()), Vec2::new(28.0, row.height));
    if row.playing {
        motion.keep_animating();
        let bar_w = 3.0;
        for i in 0..3 {
            let phase = motion.time() * 1.6 + i as f32 * 0.28;
            let h = 6.0 + 12.0 * pulse(phase, 1.0);
            let x = slot.center().x - 6.0 + i as f32 * 6.0;
            let bar = Rect::from_min_max(
                Pos2::new(x, slot.center().y - h / 2.0),
                Pos2::new(x + bar_w, slot.center().y + h / 2.0),
            );
            paint::glow_rect(painter, bar, r_pill(), row.accent, paint::GLOW_SM, paint::GlowResponse::IDLE);
            painter.rect_filled(bar, r_pill(), row.accent);
        }
    } else if let Some(index) = row.index {
        painter.text(
            slot.center(),
            Align2::CENTER_CENTER,
            index.to_string(),
            mono(BODY_SM),
            TEXT_3,
        );
    }

    let text_left = left + 36.0;
    let title_y = if row.subtitle.is_empty() {
        rect.center().y - 9.0
    } else {
        rect.center().y - 17.0
    };
    painter.text(
        Pos2::new(text_left, title_y),
        Align2::LEFT_TOP,
        row.title,
        title_font,
        text_color,
    );
    if !row.subtitle.is_empty() {
        painter.text(
            Pos2::new(text_left, rect.center().y + 3.0),
            Align2::LEFT_TOP,
            row.subtitle,
            sub_font,
            TEXT_2,
        );
    }

    let trailing_right = rect.right() - SP_16;
    if !row.trailing.is_empty() {
        painter.text(
            Pos2::new(trailing_right, rect.center().y),
            Align2::RIGHT_CENTER,
            row.trailing,
            mono(BODY_SM),
            TEXT_3,
        );
    }
    if hover > 0.4 {
        let alpha = (hover - 0.4) / 0.6;
        Icon::More.draw(
            painter,
            Rect::from_center_size(
                Pos2::new(trailing_right - 52.0, rect.center().y),
                Vec2::splat(16.0),
            ),
            paint::with_alpha(TEXT_2, alpha),
        );
    }

    response
}
