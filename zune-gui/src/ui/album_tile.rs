//! `AlbumTile` — one cell of the cover wall.
//!
//! v1.1: the Albums page owns the **cyan** signature, so everything neon in this
//! component is cyan and nothing else. A tile without cover art gets a *deliberate*
//! placeholder (pure black, 1px hairline, the album's initial in display type with a
//! text glow) so a missing cover reads as a design, not as a loading failure.

use eframe::egui::{self, Align2, Color32, FontId, Pos2, Rect, Response, Sense, TextureId, Ui, Vec2};

use crate::design::anim::{self, Lerp, Motion};
use crate::design::paint::{self, GlowResponse, GLOW_MD, GLOW_SM, GLOW_TEXT};
use crate::design::tokens::*;
use crate::ui::icons::Icon;

/// What the tile knows about its cover art. Owned, so a page can assemble its tiles
/// before rendering them.
#[derive(Clone, Debug, PartialEq)]
pub enum CoverBadge {
    /// Downloaded and ready to draw.
    Ready,
    /// Fetch in flight.
    Loading,
    /// The device refused, or the payload would not decode — shown on hover.
    Failed(String),
    /// No cover object exists for this album (the deliberate placeholder).
    Missing,
}

pub struct AlbumTile<'a> {
    pub title: &'a str,
    /// "Radiohead · 2000 · 10 首"
    pub subtitle: &'a str,
    pub cover: Option<TextureId>,
    pub badge: CoverBadge,
    /// The square edge length of the cover.
    pub size: f32,
    pub selected: bool,
}

impl<'a> AlbumTile<'a> {
    pub fn new(title: &'a str, subtitle: &'a str, size: f32) -> Self {
        Self {
            title,
            subtitle,
            cover: None,
            badge: CoverBadge::Missing,
            size,
            selected: false,
        }
    }

    pub fn cover(mut self, cover: Option<TextureId>, badge: CoverBadge) -> Self {
        self.cover = cover;
        self.badge = badge;
        self
    }

    pub fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    pub fn show(self, ui: &mut Ui, motion: &mut Motion, salt: u64) -> Response {
        const LABEL_HEIGHT: f32 = 54.0;
        let size = self.size;
        let (rect, response) =
            ui.allocate_exact_size(Vec2::new(size, size + LABEL_HEIGHT), Sense::click());
        let id = ui.id().with(("album-tile", salt, response.id));

        let hover = anim::spring_at(ui, id.with("hover"), response.hovered() as u8 as f32, motion);
        let press = anim::spring_at(
            ui,
            id.with("press"),
            response.is_pointer_button_down_on() as u8 as f32,
            motion,
        );
        // The cover lifts slightly out of its cell, and the play button fades in.
        let scale = 1.0 + 0.02 * hover;
        let play_alpha = (hover * 1.4 - 0.4).clamp(0.0, 1.0);

        let cover = Rect::from_min_size(rect.min, Vec2::splat(size));
        let lifted = Rect::from_center_size(cover.center(), cover.size() * scale);
        let painter = ui.painter();

        match self.cover {
            Some(texture) => {
                paint::rounded_image(painter, texture, lifted, r_md(), CANVAS);
            }
            None => {
                // The deliberate placeholder: black, hairline, one big initial.
                painter.rect_filled(lifted, r_md(), CANVAS);
                let initial = self
                    .title
                    .chars()
                    .next()
                    .map(|c| c.to_uppercase().to_string())
                    .unwrap_or_else(|| "?".to_owned());
                let galley = painter.layout_no_wrap(initial, FontId::new(size * 0.42, egui::FontFamily::Name(F_XLIGHT.into())), NEON_CYAN);
                let pos = lifted.center() - galley.size() / 2.0;
                paint::glow_galley(
                    painter,
                    pos,
                    &galley,
                    NEON_CYAN,
                    GLOW_TEXT,
                    GlowResponse::new(hover, press),
                );
                painter.galley(pos, galley, NEON_CYAN);
            }
        }

        // The page signature lights the tile: cyan hairline + cyan glow.
        if hover > 0.01 || self.selected {
            paint::glow_rect(
                painter,
                lifted,
                r_md(),
                NEON_CYAN,
                GLOW_MD,
                GlowResponse::new(hover, press),
            );
            paint::hairline_lit(painter, lifted, r_md(), 0.55 + 0.35 * hover, NEON_CYAN);
        } else {
            paint::hairline(painter, lifted, r_md());
        }
        if self.selected {
            paint::hairline_lit(painter, lifted, r_md(), 1.0, NEON_CYAN);
        }

        // The play affordance fades in from the bottom of the cover.
        if play_alpha > 0.01 {
            let center = Pos2::new(lifted.center().x, lifted.bottom() - 30.0);
            let button = Rect::from_center_size(center, Vec2::splat(44.0));
            painter.rect_filled(button, r_pill(), paint::with_alpha(SURFACE_2, play_alpha));
            paint::hairline_lit(painter, button, r_pill(), 0.8 * play_alpha, NEON_CYAN);
            Icon::Play.draw(
                painter,
                Rect::from_center_size(center, Vec2::splat(20.0)),
                paint::with_alpha(TEXT_1, play_alpha),
            );
        }

        // Badge: the only place a failure is allowed to be visible.
        match &self.badge {
            CoverBadge::Loading => {
                motion.keep_animating();
                let phase = crate::design::anim::pulse(motion.time(), PULSE_PERIOD);
                let dot = Pos2::new(lifted.right() - 14.0, lifted.top() + 14.0);
                paint::glow_circle(painter, dot, 4.0, NEON_AMBER, GLOW_SM, GlowResponse::hovered(phase));
                painter.circle_filled(dot, 3.5, paint::with_alpha(NEON_AMBER, 0.5 + 0.5 * phase));
            }
            CoverBadge::Failed(_) => {
                let dot = Pos2::new(lifted.right() - 14.0, lifted.top() + 14.0);
                paint::glow_circle(painter, dot, 4.0, NEON_MAGENTA, GLOW_SM, GlowResponse::IDLE);
                painter.circle_filled(dot, 4.0, NEON_MAGENTA);
                painter.text(
                    Pos2::new(dot.x, dot.y),
                    Align2::CENTER_CENTER,
                    "!",
                    FontId::new(9.0, egui::FontFamily::Name(F_BOLD.into())),
                    TEXT_INV,
                );
            }
            CoverBadge::Ready | CoverBadge::Missing => {}
        }

        // Labels: title (pure white) + "artist · year · N tracks".
        let text_left = rect.left();
        let title_top = cover.bottom() + SP_8;
        let title = truncate(painter, self.title, h3(), TEXT_1, size);
        painter.galley(Pos2::new(text_left, title_top), title, TEXT_1);
        let subtitle = truncate(painter, self.subtitle, body_sm(), TEXT_2, size);
        painter.galley(Pos2::new(text_left, title_top + 22.0), subtitle, TEXT_2);

        match &self.badge {
            CoverBadge::Failed(reason) => response.on_hover_text(format!("cover art unavailable: {reason}")),
            _ => {
                let _ = TEXT_1.lerp(TEXT_2, 0.0);
                response.on_hover_text(format!("{} · {}", self.title, self.subtitle))
            }
        }
    }
}

/// One-line elision: lay the text out once, then re-lay a shortened string if it
/// overflows. Cheap, and it keeps every tile's rows aligned.
fn truncate(
    painter: &egui::Painter,
    text: &str,
    font: FontId,
    color: Color32,
    max_width: f32,
) -> std::sync::Arc<egui::Galley> {
    let galley = painter.layout_no_wrap(text.to_owned(), font.clone(), color);
    if galley.size().x <= max_width {
        return galley;
    }
    let mut job = egui::text::LayoutJob::single_section(
        text.to_owned(),
        egui::TextFormat { font_id: font, color, ..Default::default() },
    );
    job.wrap.max_width = max_width;
    job.wrap.max_rows = 1;
    job.wrap.break_anywhere = true;
    job.wrap.overflow_character = Some('…');
    painter.layout_job(job)
}
