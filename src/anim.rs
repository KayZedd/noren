use std::time::{Duration, Instant};

/// QML `Easing.OutCubic`.
pub fn ease_out_cubic(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    1.0 - (1.0 - t).powi(3)
}

/// A value that eases toward its target, like QML `Behavior on value { NumberAnimation }`.
#[derive(Clone, Debug)]
pub struct Animated {
    from: f32,
    to: f32,
    started: Option<Instant>,
    duration: Duration,
}

impl Animated {
    pub fn new(value: f32, duration: Duration) -> Self {
        Self {
            from: value,
            to: value,
            started: None,
            duration,
        }
    }

    pub fn target(&self) -> f32 {
        self.to
    }

    /// Retargets from the current animated position, so interrupted animations do not jump.
    pub fn set(&mut self, target: f32, now: Instant) {
        if target == self.to {
            return;
        }
        self.from = self.value(now);
        self.to = target;
        self.started = (!self.duration.is_zero()).then_some(now);
    }

    /// Jumps without animating.
    pub fn reset(&mut self, value: f32) {
        self.from = value;
        self.to = value;
        self.started = None;
    }

    pub fn value(&self, now: Instant) -> f32 {
        let Some(started) = self.started else {
            return self.to;
        };
        let t = now.saturating_duration_since(started).as_secs_f32() / self.duration.as_secs_f32();
        if t >= 1.0 {
            return self.to;
        }
        self.from + (self.to - self.from) * ease_out_cubic(t)
    }

    pub fn is_animating(&self, now: Instant) -> bool {
        self.started
            .is_some_and(|started| now.saturating_duration_since(started) < self.duration)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const D: Duration = Duration::from_millis(200);

    #[test]
    fn eases_out_and_finishes_exactly_at_target() {
        let t0 = Instant::now();
        let mut a = Animated::new(0.0, D);
        a.set(100.0, t0);
        assert_eq!(a.value(t0), 0.0);
        assert_eq!(a.value(t0 + D / 2), 87.5);
        assert!(a.is_animating(t0 + D / 2));
        assert_eq!(a.value(t0 + D), 100.0);
        assert!(!a.is_animating(t0 + D));
    }

    #[test]
    fn retarget_starts_from_current_position() {
        let t0 = Instant::now();
        let mut a = Animated::new(0.0, D);
        a.set(100.0, t0);
        a.set(0.0, t0 + D / 2);
        assert_eq!(a.value(t0 + D / 2), 87.5);
        assert_eq!(a.value(t0 + D / 2 + D), 0.0);
    }

    #[test]
    fn zero_duration_and_reset_jump() {
        let t0 = Instant::now();
        let mut a = Animated::new(1.0, Duration::ZERO);
        a.set(5.0, t0);
        assert_eq!(a.value(t0), 5.0);
        assert!(!a.is_animating(t0));
        let mut b = Animated::new(0.0, D);
        b.set(9.0, t0);
        b.reset(3.0);
        assert_eq!(b.value(t0), 3.0);
        assert_eq!(ease_out_cubic(-1.0), 0.0);
        assert_eq!(ease_out_cubic(2.0), 1.0);
    }
}
