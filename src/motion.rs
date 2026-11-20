//! Transitions run on the UI thread: JS declares them once in a node's paint, and Rust animates every
//! frame without calling JS. A property animates from what is on screen to its new target, as a CSS
//! transition does, so a change in the middle of an animation turns around smoothly.
use crate::tree::{Easing, Property, Transition};
use std::collections::HashMap;

/// The React Bits stepper spring: CSS `linear()` with 29 evenly spaced samples.
const SPRING: [f32; 29] = [
    0., 0.0603, 0.2033, 0.3821, 0.5633, 0.7258, 0.8584, 0.9574, 1.0242, 1.0634, 1.0809, 1.0828,
    1.0747, 1.0613, 1.046, 1.0313, 1.0186, 1.0085, 1.0012, 0.9965, 0.994, 0.9931, 0.9933, 0.9941,
    0.9953, 0.9966, 0.9978, 0.9988, 1.,
];

/// An animatable value: a number, or a straight RGBA color with channels in 0–255.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Value {
    Number(f32),
    Color([f32; 4]),
}
impl Value {
    pub fn color(rgba: u32) -> Self {
        Value::Color(rgba.to_be_bytes().map(f32::from))
    }
    pub fn number(self) -> f32 {
        match self {
            Value::Number(v) => v,
            Value::Color(_) => 0.,
        }
    }
    pub fn rgba(self) -> [f32; 4] {
        match self {
            Value::Color(c) => c,
            Value::Number(_) => [0.; 4],
        }
    }
    fn mix(self, to: Value, t: f32) -> Value {
        match (self, to) {
            (Value::Number(a), Value::Number(b)) => Value::Number(a + (b - a) * t),
            // Colors overshoot with the spring too, but a channel never leaves its range.
            (Value::Color(a), Value::Color(b)) => Value::Color(std::array::from_fn(|i| {
                (a[i] + (b[i] - a[i]) * t).clamp(0., 255.)
            })),
            _ => to,
        }
    }
}

/// Eased progress for a time fraction `t` in 0–1.
pub fn ease(transition: &Transition, t: f32) -> f32 {
    let t = t.clamp(0., 1.);
    let eased = match transition.easing {
        Easing::Linear => t,
        Easing::Ease => bezier([0.25, 0.1, 0.25, 1.], t),
        Easing::EaseOut => bezier([0., 0., 0.58, 1.], t),
        Easing::Bezier => bezier(transition.bezier.unwrap_or([0., 0., 1., 1.]), t),
        Easing::Spring => {
            let at = t * (SPRING.len() - 1) as f32;
            let i = (at as usize).min(SPRING.len() - 2);
            SPRING[i] + (SPRING[i + 1] - SPRING[i]) * (at - i as f32)
        }
    };
    // A keyframe at `hold`: the value stays at its start until the eased progress reaches it.
    if transition.hold > 0. && transition.hold < 1. {
        ((eased - transition.hold) / (1. - transition.hold)).max(0.)
    } else {
        eased
    }
}

/// CSS `cubic-bezier(x1, y1, x2, y2)` at time `t`: solve x(s) = t, then return y(s).
fn bezier([x1, y1, x2, y2]: [f32; 4], t: f32) -> f32 {
    let curve = |a: f32, b: f32, s: f32| {
        let u = 1. - s;
        3. * u * u * s * a + 3. * u * s * s * b + s * s * s
    };
    let (mut low, mut high) = (0_f32, 1_f32);
    for _ in 0..32 {
        let mid = (low + high) / 2.;
        if curve(x1, x2, mid) < t {
            low = mid;
        } else {
            high = mid;
        }
    }
    curve(y1, y2, (low + high) / 2.)
}

struct Animation {
    from: Value,
    to: Value,
    start: f64,
    transition: Transition,
}
impl Animation {
    fn at(&self, now: f64) -> Option<Value> {
        let elapsed = (now - self.start) as f32 - self.transition.delay;
        if elapsed >= self.transition.duration {
            return None;
        }
        let t = if self.transition.duration > 0. {
            elapsed.max(0.) / self.transition.duration
        } else {
            1.
        };
        Some(self.from.mix(self.to, ease(&self.transition, t)))
    }
}

/// What a node animates: one of its properties, or a value the host keeps for it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Channel {
    Property(Property),
    /// Opacity of the scrollbar thumb of a `scroll` node.
    Thumb,
    /// Offset of a `scroll` node while a wheel eases it to its target.
    Scroll,
}
impl From<Property> for Channel {
    fn from(property: Property) -> Self {
        Channel::Property(property)
    }
}

/// What each animated channel shows, per node. Times are milliseconds on any monotonic clock.
#[derive(Default)]
pub struct Motion {
    targets: HashMap<(u16, Channel), Value>,
    running: HashMap<(u16, Channel), Animation>,
}
impl Motion {
    /// Sets the value a channel should reach and returns what to show now. With a transition and a
    /// previous target, a change animates from the value currently on screen.
    pub fn show(
        &mut self,
        id: u16,
        channel: impl Into<Channel>,
        target: Value,
        transition: Option<&Transition>,
        now: f64,
    ) -> Value {
        let key = (id, channel.into());
        match self.targets.insert(key, target) {
            Some(previous) if previous != target => match transition {
                Some(transition) => {
                    let from = self
                        .running
                        .get(&key)
                        .and_then(|a| a.at(now))
                        .unwrap_or(previous);
                    let transition = transition.clone();
                    self.running.insert(
                        key,
                        Animation {
                            from,
                            to: target,
                            start: now,
                            transition,
                        },
                    );
                }
                None => {
                    self.running.remove(&key);
                }
            },
            _ => {}
        }
        match self.running.get(&key).map(|a| a.at(now)) {
            Some(Some(value)) => value,
            Some(None) => {
                self.running.remove(&key);
                target
            }
            None => target,
        }
    }
    /// Starts an animation from `from` regardless of the previous target: entering nodes and layout changes.
    pub fn start(
        &mut self,
        id: u16,
        channel: impl Into<Channel>,
        from: Value,
        target: Value,
        transition: &Transition,
        now: f64,
    ) {
        let key = (id, channel.into());
        self.targets.insert(key, target);
        let transition = transition.clone();
        self.running.insert(
            key,
            Animation {
                from,
                to: target,
                start: now,
                transition,
            },
        );
    }
    /// The target last set for a channel.
    pub fn target(&self, id: u16, channel: impl Into<Channel>) -> Option<Value> {
        self.targets.get(&(id, channel.into())).copied()
    }
    /// Forgets a channel's target and animation: its value is set by the host again.
    pub fn release(&mut self, id: u16, channel: impl Into<Channel>) {
        let key = (id, channel.into());
        self.targets.remove(&key);
        self.running.remove(&key);
    }
    /// Whether any animation is still running at `now`, so the backend keeps drawing frames. An animation
    /// whose node is no longer walked (under a hidden node, say) is over once its time has passed.
    pub fn animating(&self, now: f64) -> bool {
        self.running.values().any(|a| a.at(now).is_some())
    }
    /// Drops everything about removed nodes, so a reused id starts fresh.
    pub fn forget(&mut self, ids: &[u16]) {
        self.targets.retain(|(id, _), _| !ids.contains(id));
        self.running.retain(|(id, _), _| !ids.contains(id));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn transition(easing: Easing, duration: f32) -> Transition {
        Transition {
            property: Property::Opacity,
            duration,
            easing,
            ..Default::default()
        }
    }

    #[test]
    fn curves_match_their_css_definitions() {
        let spring = transition(Easing::Spring, 560.);
        assert_eq!((ease(&spring, 0.), ease(&spring, 1.)), (0., 1.));
        // Sample 11 of 28 is the overshoot peak.
        assert!((ease(&spring, 11. / 28.) - 1.0828).abs() < 1e-4);
        let ease_out = transition(Easing::EaseOut, 420.);
        assert!(
            (ease(&ease_out, 0.5) - 0.684).abs() < 0.01,
            "{}",
            ease(&ease_out, 0.5)
        );
        let mut held = ease_out.clone();
        held.hold = 0.25;
        assert_eq!(ease(&held, 0.05), 0.);
        assert_eq!(ease(&held, 1.), 1.);
    }

    #[test]
    fn a_change_animates_from_what_is_on_screen() {
        let mut motion = Motion::default();
        let fade = transition(Easing::Linear, 100.);
        let show = |m: &mut Motion, v: f32, now| {
            m.show(1, Property::Opacity, Value::Number(v), Some(&fade), now)
                .number()
        };
        assert_eq!(
            show(&mut motion, 1., 0.),
            1.,
            "the first value shows at once"
        );
        assert_eq!(show(&mut motion, 0., 0.), 1.);
        assert!((show(&mut motion, 0., 50.) - 0.5).abs() < 1e-6);
        // Turning around halfway starts from 0.5, not from 0.
        assert!((show(&mut motion, 1., 50.) - 0.5).abs() < 1e-6);
        assert!((show(&mut motion, 1., 100.) - 0.75).abs() < 1e-6);
        assert_eq!(show(&mut motion, 1., 200.), 1.);
        assert!(!motion.animating(200.));
    }
}
