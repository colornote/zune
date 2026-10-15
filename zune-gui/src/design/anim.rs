//! Animation primitives: springs, eased values and the idle-repaint discipline.
//!
//! **Hard rule from the design spec:** while nothing is animating the app must not
//! repaint. Every animation therefore reports whether it is still moving, and the
//! [`Motion`] frame helper is the only thing that calls `request_repaint`.

use eframe::egui::{Color32, Pos2, Vec2};

use super::tokens::{SPRING_DAMPING, SPRING_STIFFNESS};

// ── easing curves ───────────────────────────────────────────────────────────

/// `cubic-bezier(x1, y1, x2, y2)` evaluated at `t`, à la CSS.
#[derive(Clone, Copy, Debug)]
pub struct Bezier {
    x1: f32,
    y1: f32,
    x2: f32,
    y2: f32,
}

impl Bezier {
    pub const fn new(x1: f32, y1: f32, x2: f32, y2: f32) -> Self {
        Self { x1, y1, x2, y2 }
    }

    /// The `standard` curve — the default for anything without a better idea.
    pub const STANDARD: Self = Self::new(0.2, 0.0, 0.0, 1.0);
    /// Elements entering the screen.
    pub const ENTER: Self = Self::new(0.05, 0.7, 0.1, 1.0);
    /// Elements leaving the screen.
    pub const EXIT: Self = Self::new(0.3, 0.0, 0.8, 0.15);

    fn solve_x(&self, x: f32) -> f32 {
        // Newton with a bisection fallback: cheap and always lands inside [0,1].
        let mut t = x;
        for _ in 0..6 {
            let xt = cubic(self.x1, self.x2, t) - x;
            let d = dcubic(self.x1, self.x2, t);
            if d.abs() < 1e-6 {
                break;
            }
            t -= xt / d;
            t = t.clamp(0.0, 1.0);
        }
        t
    }

    pub fn ease(&self, t: f32) -> f32 {
        let t = t.clamp(0.0, 1.0);
        cubic(self.y1, self.y2, self.solve_x(t))
    }
}

fn cubic(a: f32, b: f32, t: f32) -> f32 {
    let u = 1.0 - t;
    3.0 * u * u * t * a + 3.0 * u * t * t * b + t * t * t
}

fn dcubic(a: f32, b: f32, t: f32) -> f32 {
    let u = 1.0 - t;
    3.0 * u * u * a + 6.0 * u * t * (b - a) + 3.0 * t * t * (1.0 - b)
}

// ── spring ──────────────────────────────────────────────────────────────────

/// A damped harmonic oscillator. `stiffness 180 / damping 22` per the spec gives
/// the small overshoot that reads as "physical".
#[derive(Clone, Copy, Debug)]
pub struct Spring {
    value: f32,
    velocity: f32,
    target: f32,
}

impl Spring {
    pub fn new(value: f32) -> Self {
        Self { value, velocity: 0.0, target: value }
    }

    pub fn set(&mut self, target: f32) {
        self.target = target;
    }

    /// Jump without animating (initial state, after a layout change).
    pub fn snap(&mut self, value: f32) {
        self.value = value;
        self.target = value;
        self.velocity = 0.0;
    }

    pub fn value(&self) -> f32 {
        self.value
    }

    pub fn target(&self) -> f32 {
        self.target
    }

    /// Integrate one frame. Returns `true` while there is still motion.
    pub fn step(&mut self, dt: f32) -> bool {
        let dt = dt.clamp(0.0, 1.0 / 20.0);
        let force = (self.target - self.value) * SPRING_STIFFNESS - self.velocity * SPRING_DAMPING;
        self.velocity += force * dt;
        self.value += self.velocity * dt;
        if (self.value - self.target).abs() < 0.0005 && self.velocity.abs() < 0.001 {
            self.value = self.target;
            self.velocity = 0.0;
            return false;
        }
        true
    }
}

// ── interpolatable values ───────────────────────────────────────────────────

/// Anything an [`Animated`] can move between.
pub trait Lerp: Copy {
    fn lerp(self, other: Self, t: f32) -> Self;
}

impl Lerp for f32 {
    fn lerp(self, other: Self, t: f32) -> Self {
        self + (other - self) * t
    }
}

impl Lerp for Vec2 {
    fn lerp(self, other: Self, t: f32) -> Self {
        self + (other - self) * t
    }
}

impl Lerp for Pos2 {
    fn lerp(self, other: Self, t: f32) -> Self {
        Pos2::new(self.x + (other.x - self.x) * t, self.y + (other.y - self.y) * t)
    }
}

impl Lerp for Color32 {
    fn lerp(self, other: Self, t: f32) -> Self {
        let m = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * t).round() as u8;
        Color32::from_rgba_premultiplied(
            m(self.r(), other.r()),
            m(self.g(), other.g()),
            m(self.b(), other.b()),
            m(self.a(), other.a()),
        )
    }
}

/// What an [`Animated`] value must be: interpolatable, comparable (so a repeated
/// `set` is a no-op) and storable in egui's type map.
pub trait Animatable: Lerp + PartialEq + Send + Sync + 'static {}
impl<T: Lerp + PartialEq + Send + Sync + 'static> Animatable for T {}

/// A value that springs towards a target. The spring runs in 0..1 parameter
/// space, so colours and vectors animate as naturally as scalars.
#[derive(Clone, Copy, Debug)]
pub struct Animated<T: Animatable> {
    from: T,
    to: T,
    progress: Spring,
    current: T,
}

impl<T: Animatable> Animated<T> {
    pub fn new(value: T) -> Self {
        Self { from: value, to: value, progress: Spring::new(1.0), current: value }
    }

    /// Move towards `target`; a new target mid-flight starts from where we are.
    pub fn set(&mut self, target: T) {
        if self.to == target {
            return;
        }
        self.from = self.current;
        self.to = target;
        // Wind the spring: snap it to 0 progress *and* aim it at 1. Forgetting the
        // target (an easy mistake, caught by the test below) leaves the value parked
        // at its old position while everything looks correct in a still frame.
        self.progress.snap(0.0);
        self.progress.set(1.0);
    }

    pub fn snap(&mut self, value: T) {
        self.from = value;
        self.to = value;
        self.current = value;
        self.progress.snap(1.0);
    }

    pub fn value(&self) -> T {
        self.current
    }

    pub fn is_settled(&self) -> bool {
        self.progress.value() >= 1.0 && self.progress.target() >= 1.0
    }

    pub fn step(&mut self, dt: f32) -> bool {
        let moving = self.progress.step(dt);
        self.current = self.from.lerp(self.to, self.progress.value().clamp(0.0, 1.0));
        moving
    }
}

/// A time-based eased value, for transitions that should last exactly N seconds
/// rather than settle physically.
#[derive(Clone, Copy, Debug)]
pub struct Eased {
    from: f32,
    to: f32,
    t: f32,
    duration: f32,
    curve: Bezier,
    value: f32,
}

impl Eased {
    pub fn new(value: f32, duration: f32, curve: Bezier) -> Self {
        Self { from: value, to: value, t: duration, duration, curve, value }
    }

    pub fn set(&mut self, target: f32, duration: f32, curve: Bezier) {
        if (self.to - target).abs() < f32::EPSILON && self.t >= self.duration {
            return;
        }
        self.from = self.value;
        self.to = target;
        self.duration = duration.max(1e-4);
        self.curve = curve;
        self.t = 0.0;
    }

    pub fn value(&self) -> f32 {
        self.value
    }

    pub fn step(&mut self, dt: f32) -> bool {
        if self.t >= self.duration {
            self.value = self.to;
            return false;
        }
        self.t = (self.t + dt).min(self.duration);
        let raw = self.t / self.duration;
        self.value = self.from + (self.to - self.from) * self.curve.ease(raw);
        self.t < self.duration
    }
}

// ── per-frame motion bookkeeping ────────────────────────────────────────────

/// Collects the "still moving?" answers of one frame and issues the single
/// `request_repaint` that keeps the animations alive. Idle frames cost nothing.
pub struct Motion {
    dt: f32,
    time: f32,
    moving: bool,
}

impl Motion {
    pub fn begin(ctx: &eframe::egui::Context) -> Self {
        let (dt, time) = ctx.input(|i| (i.stable_dt.clamp(0.0, 1.0 / 20.0), i.time as f32));
        Self { dt, time, moving: false }
    }

    pub fn dt(&self) -> f32 {
        self.dt
    }

    /// Seconds since app start — for ambient effects (pulses, breathing).
    pub fn time(&self) -> f32 {
        self.time
    }

    /// Register motion so the next frame is scheduled.
    pub fn animate(&mut self, moving: bool) {
        self.moving |= moving;
    }

    /// Step a spring and register whether it is still running.
    pub fn spring(&mut self, spring: &mut Spring) -> f32 {
        self.animate(spring.step(self.dt));
        spring.value()
    }

    /// Step an animated value and register whether it is still running.
    pub fn anim<T: Animatable>(&mut self, value: &mut Animated<T>) -> T {
        self.animate(value.step(self.dt));
        value.value()
    }

    /// Step an eased value and register whether it is still running.
    pub fn eased(&mut self, value: &mut Eased) -> f32 {
        self.animate(value.step(self.dt));
        value.value()
    }

    /// Keep animating (an ambient effect such as the transfer pulse).
    pub fn keep_animating(&mut self) {
        self.moving = true;
    }

    pub fn finish(self, ctx: &eframe::egui::Context) {
        if self.moving {
            ctx.request_repaint();
        }
    }
}

/// 0..1 triangle wave with `period` seconds — used for pulses and breathing.
pub fn pulse(time: f32, period: f32) -> f32 {
    let phase = (time % period) / period;
    if phase < 0.5 {
        phase * 2.0
    } else {
        (1.0 - phase) * 2.0
    }
}

// ── state kept in egui memory ───────────────────────────────────────────────
//
// Widgets are plain functions here, so their animation state lives in egui's
// per-id memory instead of in the caller. That keeps components stateless to use
// and still honours "no repaint while idle": the spring settles and stops.

/// Animate a spring stored under `id` towards `target`; returns its value.
pub fn spring_at(ui: &eframe::egui::Ui, id: eframe::egui::Id, target: f32, motion: &mut Motion) -> f32 {
    let mut spring = ui
        .data(|d| d.get_temp::<Spring>(id))
        .unwrap_or_else(|| Spring::new(target));
    spring.set(target);
    let value = motion.spring(&mut spring);
    ui.data_mut(|d| d.insert_temp(id, spring));
    value
}

/// Animate an interpolatable value stored under `id` towards `target`.
pub fn anim_at<T: Animatable>(
    ui: &eframe::egui::Ui,
    id: eframe::egui::Id,
    target: T,
    motion: &mut Motion,
) -> T {
    let mut value = ui
        .data(|d| d.get_temp::<Animated<T>>(id))
        .unwrap_or_else(|| Animated::new(target));
    value.set(target);
    let current = motion.anim(&mut value);
    ui.data_mut(|d| d.insert_temp(id, value));
    current
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bezier_hits_its_endpoints_and_stays_monotonic() {
        for curve in [Bezier::STANDARD, Bezier::ENTER, Bezier::EXIT] {
            assert!((curve.ease(0.0) - 0.0).abs() < 1e-3);
            assert!((curve.ease(1.0) - 1.0).abs() < 1e-3);
            let mut previous = -1.0;
            for i in 0..=100 {
                let value = curve.ease(i as f32 / 100.0);
                // The enter/exit curves overshoot slightly inside [0,1]; what must
                // hold is that progress never goes backwards and stays bounded.
                assert!(value >= previous - 1e-3, "curve went backwards at {i}");
                assert!((-0.01..=1.01).contains(&value), "curve left its range: {value}");
                previous = value;
            }
        }
    }

    #[test]
    fn spring_settles_and_stops_reporting_motion() {
        let mut spring = Spring::new(0.0);
        spring.set(1.0);
        let mut frames = 0;
        while spring.step(1.0 / 60.0) {
            frames += 1;
            assert!(frames < 600, "spring never settled");
        }
        assert_eq!(spring.value(), 1.0);
        // Settled means "no repaint": stepping again must not report motion.
        assert!(!spring.step(1.0 / 60.0));
    }

    #[test]
    fn spring_overshoots_like_a_physical_one() {
        let mut spring = Spring::new(0.0);
        spring.set(1.0);
        let mut peak: f32 = 0.0;
        while spring.step(1.0 / 60.0) {
            peak = peak.max(spring.value());
        }
        assert!(peak > 1.0, "expected a little overshoot, peak was {peak}");
    }

    #[test]
    fn animated_values_are_clamped_between_their_ends() {
        let mut value = Animated::<f32>::new(0.0);
        value.set(1.0);
        let mut seen_big = false;
        while value.step(1.0 / 60.0) {
            let v = value.value();
            assert!((-0.001..=1.001).contains(&v), "left range: {v}");
            seen_big |= v > 0.9;
        }
        assert!(seen_big);
        assert!(value.is_settled());
    }

    #[test]
    fn setting_the_same_target_is_a_no_op() {
        let mut value = Animated::<f32>::new(0.25);
        value.snap(0.5);
        value.set(0.5);
        assert!(!value.step(1.0 / 60.0), "a no-op set must not animate");
    }

    #[test]
    fn pulse_breathes_between_zero_and_one() {
        assert!((pulse(0.0, 1.6) - 0.0).abs() < 1e-6);
        assert!((pulse(0.8, 1.6) - 1.0).abs() < 1e-6);
        assert!(pulse(1.6, 1.6) < 1e-6);
    }
}
