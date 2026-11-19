//! CSS easing curves and transitions between discrete values, as the original component uses them.
use gpui::Rgba;

/// The `linear()` spring from Stepper.jsx: 29 evenly spaced samples.
const SPRING: [f32; 29] = [
    0., 0.0603, 0.2033, 0.3821, 0.5633, 0.7258, 0.8584, 0.9574, 1.0242, 1.0634, 1.0809, 1.0828,
    1.0747, 1.0613, 1.046, 1.0313, 1.0186, 1.0085, 1.0012, 0.9965, 0.994, 0.9931, 0.9933, 0.9941,
    0.9953, 0.9966, 0.9978, 0.9988, 1.,
];

pub fn spring(t: f32) -> f32 {
    let x = t.clamp(0., 1.) * (SPRING.len() - 1) as f32;
    let i = (x as usize).min(SPRING.len() - 2);
    SPRING[i] + (SPRING[i + 1] - SPRING[i]) * (x - i as f32)
}

/// CSS `cubic-bezier(x1, y1, x2, y2)`: solves x(s) = t by bisection, returns y(s).
pub fn cubic_bezier(x1: f32, y1: f32, x2: f32, y2: f32) -> impl Fn(f32) -> f32 + Copy {
    let curve = |a: f32, b: f32, s: f32| {
        3. * a * s * (1. - s) * (1. - s) + 3. * b * s * s * (1. - s) + s * s * s
    };
    move |t| {
        let (mut low, mut high) = (0f32, 1f32);
        for _ in 0..24 {
            let mid = (low + high) / 2.;
            if curve(x1, x2, mid) < t {
                low = mid
            } else {
                high = mid
            }
        }
        curve(y1, y2, (low + high) / 2.)
    }
}

pub fn ease() -> impl Fn(f32) -> f32 + Copy {
    cubic_bezier(0.25, 0.1, 0.25, 1.)
}

pub fn ease_out() -> impl Fn(f32) -> f32 + Copy {
    cubic_bezier(0., 0., 0.58, 1.)
}

pub fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

pub fn mix(a: Rgba, b: Rgba, t: f32) -> Rgba {
    Rgba {
        r: lerp(a.r, b.r, t),
        g: lerp(a.g, b.g, t),
        b: lerp(a.b, b.b, t),
        a: lerp(a.a, b.a, t),
    }
}

/// A discrete value whose changes animate. `generation` names the running animation; 0 means none yet.
#[derive(Clone, Copy)]
pub struct Transition<T> {
    pub from: T,
    pub to: T,
    pub generation: usize,
}

impl<T: Copy + PartialEq> Transition<T> {
    pub fn new(value: T) -> Self {
        Self {
            from: value,
            to: value,
            generation: 0,
        }
    }

    pub fn set(&mut self, value: T) {
        if value != self.to {
            (self.from, self.to) = (self.to, value);
            self.generation += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn curves_match_css() {
        assert_eq!((spring(0.), spring(1.)), (0., 1.));
        assert!(
            (spring(10. / 28.) - 1.0809).abs() < 1e-4,
            "overshoot sample"
        );
        // CSS `ease` at 50 % is ≈ 0.8024.
        assert!((ease()(0.5) - 0.8024).abs() < 1e-3);
        assert!((ease_out()(1.) - 1.).abs() < 1e-4);
    }
}
