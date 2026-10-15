//! Painting primitives that egui does not give us directly.
//!
//! On v1.1's pure black canvas a black drop shadow is invisible, so **glow is the
//! hierarchy**: every neon element carries one, and every card carries a 1px
//! [`hairline`]. The glow tiers follow `UI_DESIGN.md` §2.1.4:
//!
//! ```text
//! glow-sm   0 0  8px color/0.60                        status dots, small icons
//! glow-md   0 0 16px color/0.50, 0 0 40px color/0.18  progress bars, selection bars
//! glow-lg   0 0 28px color/0.55, 0 0 80px color/0.22  primary CTAs
//! text-glow 0 0 12px color/0.45                        neon numbers / headings
//! ```
//!
//! Hover multiplies the radius by 1.4 and adds 0.15 to the opacity; pressing
//! multiplies it by 0.8.

use eframe::egui::{
    self, Color32, Context, CornerRadius, Mesh, Painter, Pos2, Rect, Shape, Stroke, TextureHandle,
    TextureOptions, Vec2,
};

use super::anim::Lerp;
use super::tokens::{self, alpha_of};

/// Layers per glow ring. More looks smoother and costs a few more quads.
const GLOW_LAYERS: usize = 20;
/// Horizontal slices used to fill a rounded rect with a gradient.
const GRADIENT_SLICES: usize = 20;

// ── glow tiers ──────────────────────────────────────────────────────────────

/// One tier of the glow spec: a tight ring plus an optional wide halo.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Glow {
    pub near_radius: f32,
    pub near_alpha: f32,
    pub far_radius: f32,
    pub far_alpha: f32,
}

/// `glow-sm` — status dots and small icons.
pub const GLOW_SM: Glow = Glow { near_radius: 8.0, near_alpha: 0.60, far_radius: 0.0, far_alpha: 0.0 };
/// `glow-md` — progress bars, selection indicators, rings.
pub const GLOW_MD: Glow = Glow { near_radius: 16.0, near_alpha: 0.50, far_radius: 40.0, far_alpha: 0.18 };
/// `glow-lg` — the primary call to action.
pub const GLOW_LG: Glow = Glow { near_radius: 28.0, near_alpha: 0.55, far_radius: 80.0, far_alpha: 0.22 };
/// `text-glow` — neon numbers and headings.
pub const GLOW_TEXT: Glow = Glow { near_radius: 12.0, near_alpha: 0.45, far_radius: 0.0, far_alpha: 0.0 };
/// No glow (a neutral element that must not read as neon).
pub const GLOW_NONE: Glow = Glow { near_radius: 0.0, near_alpha: 0.0, far_radius: 0.0, far_alpha: 0.0 };

/// How the element is currently being interacted with — drives the v1.1 response
/// rules (hover ×1.4 radius / +0.15 alpha, press ×0.8).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GlowResponse {
    pub hover: f32,
    pub press: f32,
}

impl GlowResponse {
    pub const IDLE: Self = Self { hover: 0.0, press: 0.0 };

    pub fn new(hover: f32, press: f32) -> Self {
        Self { hover: hover.clamp(0.0, 1.0), press: press.clamp(0.0, 1.0) }
    }

    pub fn hovered(hover: f32) -> Self {
        Self::new(hover, 0.0)
    }

    /// The radius multiplier for this state.
    fn radius_scale(self) -> f32 {
        let scale = 1.0
            + (tokens::GLOW_HOVER_RADIUS - 1.0) * self.hover
            - (1.0 - tokens::GLOW_PRESS_RADIUS) * self.press;
        scale.max(0.2)
    }

    /// The opacity delta for this state.
    fn alpha_delta(self) -> f32 {
        tokens::GLOW_HOVER_ALPHA * self.hover - 0.10 * self.press
    }
}

/// Draw a glow around a rounded rect. `glow` may be [`GLOW_NONE`] for neutral
/// elements, in which case nothing is painted.
pub fn glow_rect(painter: &Painter, rect: Rect, corner_radius: CornerRadius, color: Color32, glow: Glow, response: GlowResponse) {
    let scale = response.radius_scale();
    let delta = response.alpha_delta();
    ring(painter, rect, corner_radius, color, glow.far_radius * scale, glow.far_alpha + delta);
    ring(painter, rect, corner_radius, color, glow.near_radius * scale, glow.near_alpha + delta);
}

/// Draw a glow around a circle (status dots, knobs, playing indicators).
pub fn glow_circle(painter: &Painter, center: Pos2, radius: f32, color: Color32, glow: Glow, response: GlowResponse) {
    let scale = response.radius_scale();
    let delta = response.alpha_delta();
    halo_circle(painter, center, radius, color, glow.far_radius * scale, glow.far_alpha + delta);
    halo_circle(painter, center, radius, color, glow.near_radius * scale, glow.near_alpha + delta);
}

/// Draw a glow *behind a line/bar*: the shape itself is passed as a rect.
pub fn glow_bar(painter: &Painter, rect: Rect, color: Color32, glow: Glow, response: GlowResponse) {
    glow_rect(painter, rect, CornerRadius::same(255), color, glow, response);
}

/// `text-glow`: a soft halo under neon text.
///
/// egui cannot blur shapes, so the halo is the same galley drawn a few times with a
/// small offset — at 12px/0.45 it reads as light spill, which is what the spec asks
/// for. Draw the crisp galley afterwards, on top.
pub fn glow_galley(painter: &Painter, pos: Pos2, galley: &std::sync::Arc<egui::Galley>, color: Color32, glow: Glow, response: GlowResponse) {
    if glow.near_radius <= 0.0 {
        return;
    }
    let radius = (glow.near_radius * response.radius_scale()).clamp(0.0, 24.0);
    let alpha = (glow.near_alpha + response.alpha_delta()).clamp(0.0, 1.0);
    // The offset is what actually spreads the light; more samples on a small circle
    // keeps the halo smooth without smearing adjacent glyphs.
    let offset = (radius / 7.0).clamp(0.5, 2.2);
    let samples = 16;
    let spill = alpha_of(color, alpha * 0.16);
    for i in 0..samples {
        let angle = i as f32 / samples as f32 * std::f32::consts::TAU;
        let delta = Vec2::new(angle.cos(), angle.sin()) * offset;
        painter.galley(pos + delta, galley.clone(), spill);
    }
}

/// The quadratic falloff weights, pre-divided by their own sum so a ring's layers
/// accumulate to *exactly* `peak_alpha` — the spec's numbers are the opacity at the
/// element's edge, and without this normalisation a stack sums to ~1.4× that and the
/// glow starts reading as a fill.
fn layer_weights() -> [f32; GLOW_LAYERS] {
    let mut weights = [0.0f32; GLOW_LAYERS];
    let mut total = 0.0f32;
    for (i, w) in weights.iter_mut().enumerate() {
        let t = (i + 1) as f32 / GLOW_LAYERS as f32;
        *w = (1.0 - t).powi(2);
        total += *w;
    }
    for w in &mut weights {
        *w /= total;
    }
    weights
}

/// One ring of the glow: concentric rounded rects with a quadratic falloff, laid
/// down from the outside in so the layers sum to `peak_alpha`.
fn ring(painter: &Painter, rect: Rect, corner_radius: CornerRadius, color: Color32, radius: f32, peak_alpha: f32) {
    if radius <= 0.1 || peak_alpha <= 0.002 {
        return;
    }
    let weights = layer_weights();
    for i in (0..GLOW_LAYERS).rev() {
        let t = (i + 1) as f32 / GLOW_LAYERS as f32;
        let alpha = peak_alpha * weights[i];
        if alpha <= 0.0008 {
            continue;
        }
        painter.add(Shape::rect_filled(
            rect.expand(radius * t),
            corner_radius,
            alpha_of(color, alpha),
        ));
    }
}

/// One circular ring of a glow.
fn halo_circle(painter: &Painter, center: Pos2, radius: f32, color: Color32, spread: f32, peak_alpha: f32) {
    if spread <= 0.1 || peak_alpha <= 0.002 {
        return;
    }
    let weights = layer_weights();
    for i in (0..GLOW_LAYERS).rev() {
        let t = (i + 1) as f32 / GLOW_LAYERS as f32;
        let alpha = peak_alpha * weights[i];
        if alpha <= 0.0008 {
            continue;
        }
        painter.add(Shape::circle_filled(center, radius + spread * t, alpha_of(color, alpha)));
    }
}

/// A radial ambient light — a large, very soft wash used behind a focal element.
/// Low alpha by construction: on pure black, anything stronger becomes a colour block.
pub fn ambient_glow(painter: &Painter, center: Pos2, radius: f32, color: Color32, peak: f32) {
    if peak <= 0.0 || radius <= 0.0 {
        return;
    }
    let weights = layer_weights();
    for i in (0..GLOW_LAYERS).rev() {
        let t = (i + 1) as f32 / GLOW_LAYERS as f32;
        let alpha = peak * weights[i];
        if alpha <= 0.0004 {
            continue;
        }
        painter.add(Shape::circle_filled(center, radius * t, alpha_of(color, alpha)));
    }
}

// ── edges ───────────────────────────────────────────────────────────────────

/// Replace a colour's alpha, keeping it pre-multiplied.
pub fn with_alpha(color: Color32, alpha: f32) -> Color32 {
    alpha_of(color, alpha.clamp(0.0, 1.0))
}

/// The 1px hairline that makes a card legible on pure black (v1.1 §2.1.1).
pub fn hairline(painter: &Painter, rect: Rect, corner_radius: CornerRadius) {
    painter.rect_stroke(
        rect,
        corner_radius,
        Stroke::new(1.0, tokens::HAIRLINE),
        egui::StrokeKind::Inside,
    );
}

/// The same, with the top edge brighter — the "grazing light" of a glass surface.
pub fn hairline_lit(painter: &Painter, rect: Rect, corner_radius: CornerRadius, lit: f32, tint: Color32) {
    hairline(painter, rect, corner_radius);
    if lit > 0.01 {
        painter.rect_stroke(
            rect,
            corner_radius,
            Stroke::new(1.0, alpha_of(tint, lit)),
            egui::StrokeKind::Inside,
        );
    }
}

/// The 1px grazing highlight along the top edge of a glass surface.
pub fn top_highlight(painter: &Painter, rect: Rect, color: Color32, inset: f32) {
    let y = rect.top() + 1.0;
    painter.line_segment(
        [
            Pos2::new(rect.left() + inset, y),
            Pos2::new(rect.right() - inset, y),
        ],
        Stroke::new(1.0, color),
    );
}

/// A flat surface with the glass treatment: fill + tint + hairline + top highlight.
pub fn glass_surface(painter: &Painter, rect: Rect, corner_radius: CornerRadius, base: Color32) {
    painter.rect_filled(rect, corner_radius, base);
    painter.rect_filled(rect, corner_radius, tokens::GLASS_TINT);
    hairline(painter, rect, corner_radius);
    top_highlight(painter, rect, tokens::HAIRLINE, corner_radius.nw.max(tokens::R_SM) as f32);
}

/// A filled rounded rect with its own glow — the standard neon element
/// (status dot, selection bar, progress fill, CTA).
pub fn neon_fill(
    painter: &Painter,
    rect: Rect,
    corner_radius: CornerRadius,
    color: Color32,
    glow: Glow,
    response: GlowResponse,
) {
    glow_rect(painter, rect, corner_radius, color, glow, response);
    painter.rect_filled(rect, corner_radius, color);
}

// ── gradients (no longer used for neon: v1.1 forbids two colours per component,
//    kept for large non-neon areas such as an album backdrop) ────────────────

/// Draw a texture with rounded corners.
///
/// egui cannot round a textured quad, so the image is drawn square and the four
/// corner regions *outside* the arc are filled with `behind` — exact as long as the
/// tile really sits on that colour (the cover wall sits on the pure-black canvas).
pub fn rounded_image(
    painter: &Painter,
    texture: egui::TextureId,
    rect: Rect,
    corner_radius: CornerRadius,
    behind: Color32,
) {
    painter.image(
        texture,
        rect,
        Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
        Color32::WHITE,
    );
    let radius = corner_radius
        .nw
        .max(corner_radius.ne)
        .max(corner_radius.sw)
        .max(corner_radius.se) as f32;
    if radius <= 0.5 {
        return;
    }
    let radius = radius.min(rect.width() / 2.0).min(rect.height() / 2.0);
    // sign: -1 = left/top edge, +1 = right/bottom edge
    for (sx, sy) in [(-1.0f32, -1.0f32), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)] {
        let corner = Pos2::new(
            if sx < 0.0 { rect.left() } else { rect.right() },
            if sy < 0.0 { rect.top() } else { rect.bottom() },
        );
        let centre = Pos2::new(
            if sx < 0.0 { rect.left() + radius } else { rect.right() - radius },
            if sy < 0.0 { rect.top() + radius } else { rect.bottom() - radius },
        );
        // The quarter arc, sampled between the two edge tangents: at t = 0 the point
        // is on the horizontal edge, at t = 1 on the vertical one, and the middle
        // bulges towards the corner.
        let steps = 10;
        let mut points = Vec::with_capacity(steps + 3);
        points.push(corner);
        for i in 0..=steps {
            let angle = (i as f32 / steps as f32) * std::f32::consts::FRAC_PI_2;
            let (sin, cos) = angle.sin_cos();
            points.push(Pos2::new(
                centre.x + sx * radius * sin,
                centre.y + sy * radius * cos,
            ));
        }
        painter.add(Shape::convex_polygon(points, behind, Stroke::NONE));
    }
}

/// Fill a rounded rect with a horizontal or vertical gradient.
///
/// egui cannot fill a rounded path with a mesh, so the fill is sliced: the middle
/// slices are plain rects and only the two ends carry the rounding.
pub fn gradient_rect(
    painter: &Painter,
    rect: Rect,
    corner_radius: CornerRadius,
    from: Color32,
    to: Color32,
    horizontal: bool,
) {
    if rect.width() <= 0.0 || rect.height() <= 0.0 {
        return;
    }
    let r = corner_radius.nw.max(corner_radius.ne).max(corner_radius.sw).max(corner_radius.se);
    let slices = GRADIENT_SLICES.max(1);
    let length = if horizontal { rect.width() } else { rect.height() };
    for i in 0..slices {
        let t0 = i as f32 / slices as f32;
        let t1 = (i + 1) as f32 / slices as f32;
        let color = from.lerp(to, (t0 + t1) * 0.5);
        let (a, b) = (length * t0, length * t1);
        let (slice, corners) = if horizontal {
            (
                Rect::from_min_max(Pos2::new(rect.left() + a, rect.top()), Pos2::new(rect.left() + b, rect.bottom())),
                CornerRadius {
                    nw: if i == 0 { r } else { 0 },
                    sw: if i == 0 { r } else { 0 },
                    ne: if i == slices - 1 { r } else { 0 },
                    se: if i == slices - 1 { r } else { 0 },
                },
            )
        } else {
            (
                Rect::from_min_max(Pos2::new(rect.left(), rect.top() + a), Pos2::new(rect.right(), rect.top() + b)),
                CornerRadius {
                    nw: if i == 0 { r } else { 0 },
                    ne: if i == 0 { r } else { 0 },
                    sw: if i == slices - 1 { r } else { 0 },
                    se: if i == slices - 1 { r } else { 0 },
                },
            )
        };
        painter.rect_filled(slice, corners, color);
    }
}

/// A true mesh gradient, for rects that are not rounded (album backdrops).
pub fn gradient_mesh(painter: &Painter, rect: Rect, from: Color32, to: Color32, horizontal: bool) {
    let mut mesh = Mesh::default();
    let positions = [
        rect.left_top(),
        rect.right_top(),
        rect.right_bottom(),
        rect.left_bottom(),
    ];
    let colors = if horizontal { [from, to, to, from] } else { [from, from, to, to] };
    for (pos, color) in positions.iter().zip(colors.iter()) {
        mesh.colored_vertex(*pos, *color);
    }
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(0, 2, 3);
    painter.add(Shape::mesh(mesh));
}

// ── dot grid ────────────────────────────────────────────────────────────────

/// `Zune`'s dot matrix motif: white dots every 18px, very low alpha.
///
/// A small square tile (an exact number of dot pitches) is repeated as one quad per
/// tile. Two dead ends were measured before this: `TextureWrapMode::Repeat` is not
/// honoured for user textures here (Mesa/GL), and one panel-sized texture came back
/// as vertical streaks from the upload path.
pub struct DotGrid {
    pub texture: TextureHandle,
    /// Tile size in points (an exact multiple of the dot pitch).
    pub tile: f32,
}

const DOT_PITCH: usize = 18;
const DOT_DOTS_PER_TILE: usize = 14;
const DOT_TILE: usize = DOT_PITCH * DOT_DOTS_PER_TILE;

pub fn dot_grid(ctx: &Context) -> DotGrid {
    let id = egui::Id::new("zune-dot-grid");
    if let Some(texture) = ctx.data(|d| d.get_temp::<TextureHandle>(id)) {
        return DotGrid { texture, tile: DOT_TILE as f32 };
    }

    // 2 % averaged over a tile: on pure black this must stay a whisper.
    const ALPHA: u8 = 16;
    let mut rgba = vec![0u8; DOT_TILE * DOT_TILE * 4];
    let mut y = 0;
    while y < DOT_TILE {
        let mut x = 0;
        while x < DOT_TILE {
            for dy in 0..2 {
                for dx in 0..2 {
                    let idx = (((y + dy) * DOT_TILE) + x + dx) * 4;
                    rgba[idx] = 255;
                    rgba[idx + 1] = 255;
                    rgba[idx + 2] = 255;
                    rgba[idx + 3] = ALPHA;
                }
            }
            x += DOT_PITCH;
        }
        y += DOT_PITCH;
    }
    let image = egui::ColorImage::from_rgba_unmultiplied([DOT_TILE, DOT_TILE], &rgba);
    let texture = ctx.load_texture(
        "zune-dot-grid",
        image,
        TextureOptions {
            magnification: egui::TextureFilter::Nearest,
            minification: egui::TextureFilter::Nearest,
            wrap_mode: egui::TextureWrapMode::ClampToEdge,
            ..Default::default()
        },
    );
    ctx.data_mut(|d| d.insert_temp(id, texture.clone()));
    DotGrid { texture, tile: DOT_TILE as f32 }
}

/// Paint the grid over `rect` as one quad per tile (uv is exactly 0..1 each time).
pub fn paint_dot_grid(painter: &Painter, rect: Rect, grid: &DotGrid) {
    let mut mesh = Mesh::default();
    // Mesh::default() points at the font atlas; a user texture has to be named
    // explicitly or the quads sample glyphs (measured: magnified text fragments).
    mesh.texture_id = grid.texture.id();
    let tile = grid.tile;
    let mut y = rect.top();
    while y < rect.bottom() {
        let mut x = rect.left();
        while x < rect.right() {
            let cell = Rect::from_min_max(
                Pos2::new(x, y),
                Pos2::new((x + tile).min(rect.right()), (y + tile).min(rect.bottom())),
            );
            let cell_uv = Rect::from_min_max(
                Pos2::ZERO,
                Pos2::new(cell.width() / tile, cell.height() / tile),
            );
            let base = mesh.vertices.len() as u32;
            for (pos, uv) in [
                (cell.left_top(), cell_uv.left_top()),
                (cell.right_top(), cell_uv.right_top()),
                (cell.right_bottom(), cell_uv.right_bottom()),
                (cell.left_bottom(), cell_uv.left_bottom()),
            ] {
                mesh.vertices.push(egui::epaint::Vertex { pos, uv, color: Color32::WHITE });
            }
            mesh.indices
                .extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
            x += tile;
        }
        y += tile;
    }
    painter.add(Shape::mesh(mesh));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glow_tiers_match_the_spec() {
        assert_eq!(GLOW_SM.near_radius, 8.0);
        assert_eq!(GLOW_SM.near_alpha, 0.60);
        assert_eq!(GLOW_MD.near_radius, 16.0);
        assert_eq!(GLOW_MD.near_alpha, 0.50);
        assert_eq!(GLOW_MD.far_radius, 40.0);
        assert_eq!(GLOW_MD.far_alpha, 0.18);
        assert_eq!(GLOW_LG.near_radius, 28.0);
        assert_eq!(GLOW_LG.near_alpha, 0.55);
        assert_eq!(GLOW_LG.far_radius, 80.0);
        assert_eq!(GLOW_LG.far_alpha, 0.22);
        assert_eq!(GLOW_TEXT.near_radius, 12.0);
        assert_eq!(GLOW_TEXT.near_alpha, 0.45);
    }

    #[test]
    fn hover_enlarges_and_brightens_a_glow() {
        let idle = GlowResponse::IDLE;
        let hover = GlowResponse::hovered(1.0);
        assert!((hover.radius_scale() - 1.4).abs() < 1e-5);
        assert!((hover.alpha_delta() - 0.15).abs() < 1e-5);
        assert!(hover.radius_scale() > idle.radius_scale());
    }

    #[test]
    fn press_shrinks_a_glow() {
        let press = GlowResponse::new(0.0, 1.0);
        assert!((press.radius_scale() - 0.8).abs() < 1e-5);
        assert!(press.radius_scale() < GlowResponse::IDLE.radius_scale());
    }

    #[test]
    fn the_glow_never_becomes_a_colour_block() {
        // Acceptance: neon stays small and glowing, never a filled area. The sum of
        // one ring's layer alphas must stay well under 1.0 of the pure colour.
        let total: f32 = layer_weights().iter().map(|w| GLOW_LG.near_alpha * w).sum();
        assert!(
            (total - GLOW_LG.near_alpha).abs() < 0.01,
            "a ring must accumulate to its spec opacity, got {total}"
        );
        assert!(total < 0.65, "glow sums to {total}, which reads as a fill");
    }
}
