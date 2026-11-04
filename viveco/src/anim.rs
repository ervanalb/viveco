//! Animation curve sampling.
//!
//! The evaluator matches the editor's: keyframes are grouped by target (the
//! compiler already does this), keyframes are ordered by timestamp, and
//! missing Bézier handles are inferred from the neighbouring keyframe. A
//! non-cyclic curve holds its first and last values outside its keyframe range;
//! a cyclic curve repeats with a period equal to the animation's duration and
//! interpolates across the seam.

use crate::format::CurveRef;

/// Clamps a playhead to `0..=duration`. A non-finite or zero duration samples
/// at time zero.
pub fn clamped_time(time: f32, duration: f32) -> f32 {
    let time = if time.is_finite() { time } else { 0.0 };
    if duration.is_finite() && duration > 0.0 {
        time.clamp(0.0, duration)
    } else {
        0.0
    }
}

/// Samples one curve at `time`.
///
/// Returns `None` only when the curve has no keyframes.
pub fn sample_curve(curve: CurveRef<'_>, time: f32, duration: f32, cyclic: bool) -> Option<f32> {
    let len = curve.len();
    if len == 0 {
        return None;
    }
    let time = clamped_time(time, duration);
    let first = curve.keyframe(0)?;
    let last = curve.keyframe(len - 1)?;

    if !cyclic || !duration.is_finite() || duration <= 0.0 {
        if time <= first.timestamp() {
            return Some(first.value());
        }
        if time >= last.timestamp() {
            return Some(last.value());
        }
        for index in 0..len - 1 {
            let before = curve.keyframe(index)?;
            let after = curve.keyframe(index + 1)?;
            if time <= after.timestamp() {
                return Some(interpolate(
                    before.value(),
                    before.timestamp(),
                    after.timestamp(),
                    after.value(),
                    effective_handle_out(curve, index, false),
                    effective_handle_in(curve, index + 1, false),
                    time,
                ));
            }
        }
        return Some(last.value());
    }

    // Cyclic sampling: find the keyframes on either side of `time`, wrapping
    // across the seam. A keyframe at exactly `time` is both the previous and
    // the next one.
    let previous = (0..len)
        .rev()
        .find(|&index| curve.keyframe(index).is_some_and(|k| k.timestamp() <= time));
    let next = (0..len).find(|&index| curve.keyframe(index).is_some_and(|k| k.timestamp() >= time));

    let (before_index, before_time, after_index, after_value, after_time) = match (previous, next) {
        (Some(before), Some(after)) => {
            let before_key = curve.keyframe(before)?;
            let after_key = curve.keyframe(after)?;
            (
                before,
                before_key.timestamp(),
                after,
                after_key.value(),
                after_key.timestamp(),
            )
        }
        (Some(before), None) => {
            let before_key = curve.keyframe(before)?;
            (
                before,
                before_key.timestamp(),
                0,
                first.value(),
                first.timestamp() + duration,
            )
        }
        (None, Some(after)) => {
            let after_key = curve.keyframe(after)?;
            (
                len - 1,
                last.timestamp() - duration,
                after,
                after_key.value(),
                after_key.timestamp(),
            )
        }
        (None, None) => return None,
    };

    if after_time <= before_time {
        return Some(curve.keyframe(before_index)?.value());
    }

    Some(interpolate(
        curve.keyframe(before_index)?.value(),
        before_time,
        after_time,
        after_value,
        effective_handle_out(curve, before_index, true),
        effective_handle_in(curve, after_index, true),
        time,
    ))
}

/// The value of a keyframe's outgoing control point.
///
/// An explicit handle wins. Otherwise the handle is the midpoint toward the
/// next keyframe's incoming handle, or a third of the way toward the next
/// keyframe's value. The last keyframe of a non-cyclic curve has no outgoing
/// handle.
pub fn effective_handle_out(curve: CurveRef<'_>, index: usize, cyclic: bool) -> Option<f32> {
    let keyframe = curve.keyframe(index)?;
    if let Some(handle) = keyframe.handle_out() {
        return Some(handle);
    }
    let next = curve.keyframe(next_index(curve.len(), index, cyclic)?)?;
    Some(match next.handle_in() {
        Some(handle) => (keyframe.value() + handle) / 2.0,
        None => keyframe.value() + (next.value() - keyframe.value()) / 3.0,
    })
}

/// The value of a keyframe's incoming control point. Mirrors
/// [`effective_handle_out`].
pub fn effective_handle_in(curve: CurveRef<'_>, index: usize, cyclic: bool) -> Option<f32> {
    let keyframe = curve.keyframe(index)?;
    if let Some(handle) = keyframe.handle_in() {
        return Some(handle);
    }
    let previous = curve.keyframe(previous_index(curve.len(), index, cyclic)?)?;
    Some(match previous.handle_out() {
        Some(handle) => (keyframe.value() + handle) / 2.0,
        None => keyframe.value() + (previous.value() - keyframe.value()) / 3.0,
    })
}

fn next_index(len: usize, index: usize, cyclic: bool) -> Option<usize> {
    if index + 1 < len {
        Some(index + 1)
    } else if cyclic {
        Some(0)
    } else {
        None
    }
}

fn previous_index(len: usize, index: usize, cyclic: bool) -> Option<usize> {
    if index > 0 {
        Some(index - 1)
    } else if cyclic {
        Some(len - 1)
    } else {
        None
    }
}

/// Evaluates the cubic between two keyframes at `time`.
///
/// The horizontal control points sit a third of the way along the gap, so the
/// Bézier parameter is exactly the normalized time and only the values need
/// interpolating.
fn interpolate(
    start_value: f32,
    start_time: f32,
    end_time: f32,
    end_value: f32,
    control_out: Option<f32>,
    control_in: Option<f32>,
    time: f32,
) -> f32 {
    let gap = end_time - start_time;
    if gap.is_nan() || gap <= 0.0 {
        return start_value;
    }
    let s = ((time - start_time) / gap).clamp(0.0, 1.0);
    cubic(
        start_value,
        control_out.unwrap_or(start_value),
        control_in.unwrap_or(end_value),
        end_value,
        s,
    )
}

fn cubic(p0: f32, p1: f32, p2: f32, p3: f32, s: f32) -> f32 {
    let u = 1.0 - s;
    u * u * u * p0 + 3.0 * u * u * s * p1 + 3.0 * u * s * s * p2 + s * s * s * p3
}

#[cfg(test)]
mod tests {
    // Sampling is exercised end-to-end by the `viveco-bevy` example and by
    // tests that build a binary via the macro. The curve math above is a
    // direct port of the authoring app's sampler.
}
