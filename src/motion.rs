//! Transitions run on the UI thread: JS declares them once in a node's paint, and Rust animates every
//! frame without calling JS. A property animates from what is on screen to its new target, as a CSS
//! transition does, so a change in the middle of an animation turns around smoothly.
use crate::tree::{self, Easing, Keyframe, Property, Transition};
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
    let eased = timing(transition.easing, transition.bezier, t);
    // A keyframe at `hold`: the value stays at its start until the eased progress reaches it.
    if transition.hold > 0. && transition.hold < 1. {
        ((eased - transition.hold) / (1. - transition.hold)).max(0.)
    } else {
        eased
    }
}

/// The timing curve `easing` at a time fraction `t` in 0–1; `points` are for `Easing::Bezier`.
fn timing(easing: Easing, points: Option<[f32; 4]>, t: f32) -> f32 {
    let t = t.clamp(0., 1.);
    match easing {
        Easing::Linear => t,
        Easing::Ease => bezier([0.25, 0.1, 0.25, 1.], t),
        Easing::EaseOut => bezier([0., 0., 0.58, 1.], t),
        Easing::EaseInOut => bezier([0.42, 0., 0.58, 1.], t),
        Easing::Bezier => bezier(points.unwrap_or([0., 0., 1., 1.]), t),
        Easing::Spring => {
            let at = t * (SPRING.len() - 1) as f32;
            let i = (at as usize).min(SPRING.len() - 2);
            SPRING[i] + (SPRING[i + 1] - SPRING[i]) * (at - i as f32)
        }
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

/// Keyframe animations of a node: the list they run, and when that list started.
struct Loop {
    animations: Vec<tree::Animation>,
    start: f64,
}

/// What a node's keyframe animations show now, and whether any of them is still running.
#[derive(Default)]
pub struct Keyed {
    /// Values that override the base values of their properties.
    pub values: Vec<(Property, Value)>,
    pub running: bool,
}

/// What each animated channel shows, per node. Times are milliseconds on any monotonic clock.
#[derive(Default)]
pub struct Motion {
    targets: HashMap<(u16, Channel), Value>,
    running: HashMap<(u16, Channel), Animation>,
    loops: HashMap<u16, Loop>,
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
    /// Whether any node's `channel` is animating at `now`: the scroll glides, for one.
    pub fn animating_on(&self, channel: Channel, now: f64) -> bool {
        self.running
            .iter()
            .any(|((_, c), a)| *c == channel && a.at(now).is_some())
    }
    /// Runs a node's keyframe animations at `now`. A list that differs from the last one restarts them. With
    /// `reduced` (reduced motion), infinite animations do not run. With `frozen` (a capture), each animation shows
    /// its value `frozen` ms after its own start, and none counts as running.
    pub fn keyframes(
        &mut self,
        id: u16,
        animations: &[tree::Animation],
        reduced: bool,
        frozen: Option<f32>,
        now: f64,
    ) -> Keyed {
        if animations.is_empty() {
            self.loops.remove(&id);
            return Keyed::default();
        }
        let start = match self.loops.get(&id) {
            Some(known) if known.animations == animations => known.start,
            _ => {
                self.loops.insert(
                    id,
                    Loop {
                        animations: animations.to_vec(),
                        start: now,
                    },
                );
                now
            }
        };
        let mut keyed = Keyed::default();
        for animation in animations {
            if reduced && animation.iterations.is_none() {
                continue;
            }
            if let Some(at) = frozen {
                if let Some(value) = sample(animation, at) {
                    keyed.values.push((animation.property, value));
                }
                continue;
            }
            // Time since the animation's own start, which is after its delay.
            let since = (now - start) as f32 - animation.delay;
            let total = animation
                .iterations
                .map_or(f32::INFINITY, |n| n * animation.duration);
            keyed.running |= since < total;
            if since >= 0.
                && let Some(value) = sample(animation, since)
            {
                keyed.values.push((animation.property, value));
            }
        }
        keyed
    }
    /// Drops everything about removed nodes, so a reused id starts fresh.
    pub fn forget(&mut self, ids: &[u16]) {
        self.targets.retain(|(id, _), _| !ids.contains(id));
        self.running.retain(|(id, _), _| !ids.contains(id));
        self.loops.retain(|id, _| !ids.contains(id));
    }
}

/// The value of a keyframe animation `since` ms after its own start, or None once its iterations are over.
fn sample(animation: &tree::Animation, since: f32) -> Option<Value> {
    let turns = since / animation.duration;
    if animation.iterations.is_some_and(|n| turns >= n) {
        return None;
    }
    let iteration = turns.floor();
    let mut t = turns - iteration;
    if animation.alternate && iteration % 2. == 1. {
        t = 1. - t;
    }
    let property = animation.property;
    let value = |k: &Keyframe| {
        if property.is_color() {
            Value::color(k.color.unwrap_or(0))
        } else {
            Value::Number(k.value.unwrap_or(0.))
        }
    };
    let frames = &animation.keyframes;
    // The last keyframe at or before `t` starts the segment that `t` is in.
    let Some(i) = frames.iter().rposition(|k| k.offset <= t) else {
        return Some(value(&frames[0]));
    };
    Some(match frames.get(i + 1) {
        None => value(&frames[i]),
        Some(next) => {
            let from = &frames[i];
            let local = (t - from.offset) / (next.offset - from.offset);
            value(from).mix(
                value(next),
                timing(animation.easing, animation.bezier, local),
            )
        }
    })
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

    fn keyframe(offset: f32, value: f32) -> Keyframe {
        Keyframe {
            offset,
            value: Some(value),
            color: None,
        }
    }

    #[test]
    fn an_infinite_loop_eases_through_its_keyframes() {
        let pulse = tree::Animation {
            property: Property::Opacity,
            keyframes: vec![keyframe(0., 1.), keyframe(0.5, 0.5), keyframe(1., 1.)],
            duration: 3000.,
            easing: Easing::EaseInOut,
            ..Default::default()
        };
        let mut motion = Motion::default();
        let mut at = |now: f64| {
            motion
                .keyframes(1, std::slice::from_ref(&pulse), false, None, now)
                .values[0]
                .1
                .number()
        };
        let shown = [0., 750., 1500., 3000., 4500.].map(&mut at);
        for (got, want) in shown.iter().zip([1., 0.75, 0.5, 1., 0.5]) {
            assert!((got - want).abs() < 1e-4, "{shown:?}");
        }
        // A changed list restarts the loop at the time of the change.
        let slower = tree::Animation {
            duration: 6000.,
            ..pulse
        };
        let restarted = motion.keyframes(1, &[slower], false, None, 4500.);
        assert_eq!(restarted.values[0].1.number(), 1.);
        assert!(restarted.running);
    }

    #[test]
    fn alternate_plays_every_other_iteration_backwards() {
        let breathe = tree::Animation {
            property: Property::Opacity,
            keyframes: vec![keyframe(0., 0.7), keyframe(1., 1.)],
            duration: 5000.,
            easing: Easing::EaseInOut,
            alternate: true,
            ..Default::default()
        };
        let mut motion = Motion::default();
        let mut at = |now: f64| {
            motion
                .keyframes(1, std::slice::from_ref(&breathe), false, None, now)
                .values[0]
                .1
                .number()
        };
        assert_eq!(at(0.), 0.7);
        assert!((at(5000.) - 1.).abs() < 1e-4);
        assert!((at(7500.) - 0.85).abs() < 1e-4);
        assert!((at(10000.) - 0.7).abs() < 1e-4);
    }

    #[test]
    fn colors_interpolate_straight_rgba_including_alpha() {
        let color = |offset, color| tree::Keyframe {
            offset,
            value: None,
            color: Some(color),
        };
        let pulse = tree::Animation {
            property: Property::Background,
            keyframes: vec![
                color(0., 0x9696a00f),
                color(0.5, 0x9696a01f),
                color(1., 0x9696a00f),
            ],
            duration: 1600.,
            easing: Easing::EaseInOut,
            ..Default::default()
        };
        let mut motion = Motion::default();
        let mut at = |now: f64| {
            motion
                .keyframes(1, std::slice::from_ref(&pulse), false, None, now)
                .values[0]
                .1
                .rgba()
        };
        assert_eq!(at(0.), [150., 150., 160., 15.]);
        // Halfway through the first segment the alpha is halfway between 15 and 31; the RGB never changes.
        assert_eq!(at(400.), [150., 150., 160., 23.]);
        assert_eq!(at(800.), [150., 150., 160., 31.]);
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
