//! A small hand-drawn line-icon set: 1.5 px strokes, rounded geometry, no font
//! dependency and no extra crate.
//!
//! Every icon is authored on a 20×20 grid and scaled to the target rect, so the
//! stroke stays optically constant at 16/20/24 px.

use eframe::egui::{Color32, Painter, Pos2, Rect, Shape, Stroke, Vec2};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Icon {
    Home,
    Note,
    Grid,
    List,
    Person,
    Image,
    Film,
    Sync,
    Gear,
    Wifi,
    Search,
    Play,
    Pause,
    Next,
    Upload,
    Trash,
    Refresh,
    Eject,
    Folder,
    Close,
    Check,
    Chevron,
    More,
    Dot,
}

impl Icon {
    /// Every icon, in board order.
    pub const ALL: [Icon; 24] = [
        Icon::Home,
        Icon::Note,
        Icon::Grid,
        Icon::List,
        Icon::Person,
        Icon::Image,
        Icon::Film,
        Icon::Sync,
        Icon::Gear,
        Icon::Wifi,
        Icon::Search,
        Icon::Play,
        Icon::Pause,
        Icon::Next,
        Icon::Upload,
        Icon::Trash,
        Icon::Refresh,
        Icon::Eject,
        Icon::Folder,
        Icon::Close,
        Icon::Check,
        Icon::Chevron,
        Icon::More,
        Icon::Dot,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Icon::Home => "Home",
            Icon::Note => "Note",
            Icon::Grid => "Grid",
            Icon::List => "List",
            Icon::Person => "Person",
            Icon::Image => "Image",
            Icon::Film => "Film",
            Icon::Sync => "Sync",
            Icon::Gear => "Gear",
            Icon::Wifi => "Wifi",
            Icon::Search => "Search",
            Icon::Play => "Play",
            Icon::Pause => "Pause",
            Icon::Next => "Next",
            Icon::Upload => "Upload",
            Icon::Trash => "Trash",
            Icon::Refresh => "Refresh",
            Icon::Eject => "Eject",
            Icon::Folder => "Folder",
            Icon::Close => "Close",
            Icon::Check => "Check",
            Icon::Chevron => "Chevron",
            Icon::More => "More",
            Icon::Dot => "Dot",
        }
    }

    /// Draw centred in `rect` with a 1.5 px stroke scaled to the icon size.
    pub fn draw(self, painter: &Painter, rect: Rect, color: Color32) {
        let size = rect.width().min(rect.height());
        let stroke = Stroke::new((1.5 * size / 20.0).max(1.0), color);
        self.draw_with(painter, rect, stroke);
    }

    pub fn draw_with(self, painter: &Painter, rect: Rect, stroke: Stroke) {
        let size = rect.width().min(rect.height());
        let scale = size / 20.0;
        let origin = rect.center() - Vec2::splat(size / 2.0);
        let p = |x: f32, y: f32| Pos2::new(origin.x + x * scale, origin.y + y * scale);
        let line = |points: Vec<Pos2>| painter.add(Shape::line(points, stroke));
        let seg = |a: (f32, f32), b: (f32, f32)| painter.line_segment([p(a.0, a.1), p(b.0, b.1)], stroke);
        let rect_at = |x: f32, y: f32, w: f32, h: f32, r: f32| {
            painter.rect_stroke(
                Rect::from_min_size(p(x, y), Vec2::new(w * scale, h * scale)),
                eframe::egui::CornerRadius::same((r * scale) as u8),
                stroke,
                eframe::egui::StrokeKind::Middle,
            )
        };
        // A circular arc as a polyline: cheaper and more predictable than a bezier.
        let arc = |cx: f32, cy: f32, radius: f32, from_deg: f32, to_deg: f32| {
            let steps = 12;
            let points: Vec<Pos2> = (0..=steps)
                .map(|i| {
                    let t = i as f32 / steps as f32;
                    let a = (from_deg + (to_deg - from_deg) * t).to_radians();
                    p(cx + radius * a.cos(), cy + radius * a.sin())
                })
                .collect();
            painter.add(Shape::line(points, stroke));
        };

        match self {
            Icon::Home => {
                line(vec![p(3.0, 9.0), p(10.0, 3.0), p(17.0, 9.0)]);
                line(vec![p(5.0, 8.5), p(5.0, 17.0), p(15.0, 17.0), p(15.0, 8.5)]);
            }
            Icon::Note => {
                painter.circle_stroke(p(7.0, 15.0), 2.6 * scale, stroke);
                seg((9.6, 15.0), (9.6, 5.0));
                line(vec![p(9.6, 5.0), p(16.0, 6.6), p(16.0, 11.0)]);
                painter.circle_stroke(p(13.4, 13.0), 2.6 * scale, stroke);
            }
            Icon::Grid => {
                rect_at(3.0, 3.0, 6.0, 6.0, 1.0);
                rect_at(11.0, 3.0, 6.0, 6.0, 1.0);
                rect_at(3.0, 11.0, 6.0, 6.0, 1.0);
                rect_at(11.0, 11.0, 6.0, 6.0, 1.0);
            }
            Icon::List => {
                for y in [5.0, 10.0, 15.0] {
                    painter.circle_filled(p(4.0, y), 1.1 * scale, stroke.color);
                    seg((7.5, y), (17.0, y));
                }
            }
            Icon::Person => {
                painter.circle_stroke(p(10.0, 7.0), 3.4 * scale, stroke);
                arc(10.0, 18.0, 6.4, 200.0, 340.0);
            }
            Icon::Image => {
                rect_at(3.0, 4.0, 14.0, 12.0, 2.0);
                painter.circle_stroke(p(7.6, 8.2), 1.6 * scale, stroke);
                line(vec![p(4.5, 15.5), p(9.0, 10.5), p(12.0, 13.5), p(14.0, 11.5), p(16.0, 14.0)]);
            }
            Icon::Film => {
                rect_at(3.0, 4.0, 14.0, 12.0, 2.0);
                seg((7.5, 4.0), (7.5, 16.0));
                seg((12.5, 4.0), (12.5, 16.0));
                seg((3.0, 10.0), (17.0, 10.0));
            }
            Icon::Sync => {
                arc(10.0, 10.0, 6.5, 30.0, 200.0);
                arc(10.0, 10.0, 6.5, 210.0, 380.0);
                line(vec![p(16.5, 8.0), p(16.6, 12.4), p(13.0, 11.0)]);
                line(vec![p(3.5, 12.0), p(3.4, 7.6), p(7.0, 9.0)]);
            }
            Icon::Gear => {
                painter.circle_stroke(p(10.0, 10.0), 3.0 * scale, stroke);
                for i in 0..6 {
                    let a = (i as f32 * 60.0).to_radians();
                    let (s, c) = a.sin_cos();
                    seg((10.0 + 4.6 * c, 10.0 + 4.6 * s), (10.0 + 7.0 * c, 10.0 + 7.0 * s));
                }
            }
            Icon::Wifi => {
                arc(10.0, 15.5, 9.0, 200.0, 340.0);
                arc(10.0, 15.5, 5.8, 200.0, 340.0);
                arc(10.0, 15.5, 2.6, 200.0, 340.0);
                painter.circle_filled(p(10.0, 16.4), 1.0 * scale, stroke.color);
            }
            Icon::Search => {
                painter.circle_stroke(p(8.8, 8.8), 4.8 * scale, stroke);
                seg((12.3, 12.3), (17.0, 17.0));
            }
            Icon::Play => {
                painter.add(Shape::convex_polygon(
                    vec![p(7.0, 4.5), p(16.0, 10.0), p(7.0, 15.5)],
                    stroke.color,
                    Stroke::NONE,
                ));
            }
            Icon::Pause => {
                seg((8.0, 5.0), (8.0, 15.0));
                seg((13.0, 5.0), (13.0, 15.0));
            }
            Icon::Next => {
                painter.add(Shape::convex_polygon(
                    vec![p(5.0, 5.0), p(13.0, 10.0), p(5.0, 15.0)],
                    stroke.color,
                    Stroke::NONE,
                ));
                seg((15.5, 5.0), (15.5, 15.0));
            }
            Icon::Upload => {
                seg((10.0, 15.0), (10.0, 4.5));
                line(vec![p(6.5, 8.0), p(10.0, 4.5), p(13.5, 8.0)]);
                line(vec![p(4.0, 12.0), p(4.0, 16.5), p(16.0, 16.5), p(16.0, 12.0)]);
            }
            Icon::Trash => {
                line(vec![p(3.5, 6.0), p(16.5, 6.0)]);
                line(vec![p(6.0, 6.0), p(7.0, 17.0), p(13.0, 17.0), p(14.0, 6.0)]);
                line(vec![p(8.0, 6.0), p(8.4, 3.5), p(11.6, 3.5), p(12.0, 6.0)]);
                seg((9.0, 9.0), (9.3, 14.0));
                seg((11.0, 9.0), (10.7, 14.0));
            }
            Icon::Refresh => {
                arc(10.0, 10.0, 6.5, -60.0, 240.0);
                line(vec![p(14.0, 3.0), p(17.0, 6.6), p(12.4, 7.4)]);
            }
            Icon::Eject => {
                painter.add(Shape::convex_polygon(
                    vec![p(10.0, 4.0), p(16.0, 11.0), p(4.0, 11.0)],
                    stroke.color,
                    Stroke::NONE,
                ));
                seg((4.0, 15.0), (16.0, 15.0));
            }
            Icon::Folder => {
                line(vec![p(3.0, 6.0), p(8.0, 6.0), p(9.5, 8.0), p(17.0, 8.0), p(17.0, 16.5), p(3.0, 16.5), p(3.0, 6.0)]);
            }
            Icon::Close => {
                seg((5.5, 5.5), (14.5, 14.5));
                seg((14.5, 5.5), (5.5, 14.5));
            }
            Icon::Check => {
                line(vec![p(4.5, 10.5), p(8.5, 14.5), p(15.5, 5.5)]);
            }
            Icon::Chevron => {
                line(vec![p(8.5, 5.0), p(13.5, 10.0), p(8.5, 15.0)]);
            }
            Icon::More => {
                for x in [5.0, 10.0, 15.0] {
                    painter.circle_filled(p(x, 10.0), 1.4 * scale, stroke.color);
                }
            }
            Icon::Dot => {
                painter.circle_filled(p(10.0, 10.0), 3.0 * scale, stroke.color);
            }
        }
    }
}
