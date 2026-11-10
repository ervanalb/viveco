//! Runtime helpers for blending animated layers.
//!
//! [`AccelEasing`] eases a scalar toward a target with bounded speed and
//! acceleration, which is useful for blend weights that should not pop. It is
//! independent of [`Asset`](crate::Asset): it operates on plain numbers, so an
//! application decides where its animation data and clip lengths come from.

/// A scalar that chases a target with bounded speed and acceleration.
///
/// Each [`step`](Self::step) chooses a desired speed along the braking curve
/// `sqrt(2 * a * |error|)`, then limits how fast the speed itself may change.
/// Starting from rest, the value accelerates, cruises near the top speed, and
/// brakes into the target, giving a cubic-like ease in both directions. The
/// value never passes the target; on arrival it stops exactly on it.
///
/// ```
/// use viveco::AccelEasing;
///
/// let mut easing = AccelEasing::new(3.0, 10.0);
/// assert_eq!(easing.value, 0.0);
///
/// for _ in 0..60 {
///     easing.step(1.0, 1.0 / 60.0);
/// }
/// assert!((easing.value - 1.0).abs() < 1e-3);
/// ```
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AccelEasing {
    /// The current value.
    pub value: f32,
    /// The current rate of change, in value units per second.
    pub velocity: f32,
    /// Speed limit, in value units per second.
    pub max_speed: f32,
    /// Acceleration limit, in value units per second squared.
    pub max_accel: f32,
}

impl AccelEasing {
    /// Creates an easing at rest at value `0` with the given limits.
    ///
    /// `max_speed` is in value units per second, and `max_accel` is in value
    /// units per second squared. Both should be positive.
    pub const fn new(max_speed: f32, max_accel: f32) -> Self {
        Self {
            value: 0.0,
            velocity: 0.0,
            max_speed,
            max_accel,
        }
    }

    /// Moves the value toward `target` for `delta` seconds and returns it.
    ///
    /// The value reaches the target in finite time and stops exactly on it.
    pub fn step(&mut self, target: f32, delta: f32) -> f32 {
        let error = target - self.value;
        if error.abs() < 1e-4 && self.velocity.abs() < 1e-4 {
            self.value = target;
            self.velocity = 0.0;
            return self.value;
        }

        let desired = error.signum()
            * (2.0 * self.max_accel * error.abs())
                .sqrt()
                .min(self.max_speed);
        let max_change = self.max_accel * delta;
        self.velocity += (desired - self.velocity).clamp(-max_change, max_change);

        let next = self.value + self.velocity * delta;
        if (next - target).signum() != (self.value - target).signum() {
            // Reaching or passing the target: stop exactly on it.
            self.value = target;
            self.velocity = 0.0;
        } else {
            self.value = next;
        }

        self.value
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Steps until settled, returning the number of steps taken.
    fn settle(easing: &mut AccelEasing, target: f32, delta: f32) -> usize {
        for step in 0..10_000 {
            easing.step(target, delta);
            if easing.value == target {
                return step + 1;
            }
        }
        panic!("easing did not settle at {target}");
    }

    #[test]
    fn accel_easing_reaches_and_holds_its_target() {
        let mut easing = AccelEasing::new(3.0, 10.0);
        settle(&mut easing, 1.0, 1.0 / 60.0);
        assert_eq!(easing.value, 1.0);
        assert_eq!(easing.velocity, 0.0);

        settle(&mut easing, 0.0, 1.0 / 60.0);
        assert_eq!(easing.value, 0.0);
        assert_eq!(easing.velocity, 0.0);
    }

    #[test]
    fn accel_easing_never_passes_its_target() {
        let mut easing = AccelEasing::new(30.0, 100.0);
        for _ in 0..100 {
            easing.step(1.0, 1.0 / 60.0);
            assert!((0.0..=1.0).contains(&easing.value));
        }
    }

    #[test]
    fn accel_easing_respects_the_speed_limit() {
        let mut easing = AccelEasing::new(2.0, 100.0);
        let delta = 1.0 / 60.0;
        let before = easing.value;
        easing.step(1.0, delta);
        assert!(easing.value - before <= 2.0 * delta + 1e-6);
    }
}
