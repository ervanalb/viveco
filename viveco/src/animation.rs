//! Animations, keyframes, and curve sampling.

use std::cmp::Ordering;
use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::asset::Asset;
use crate::bone::BoneIndex;
use crate::pose::{Pose, ShapePose};
use crate::shape::ShapeIndex;

/// One playing instance of an animation:
/// `(animation index, time in seconds, weight)`.
///
/// Several instances can be blended by [`Asset::sample_pose`](crate::Asset::sample_pose).
pub type AnimationInstance = (usize, f32, f32);

/// A property of a bone that an animation curve can drive.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BoneProperty {
    /// Horizontal translation of the bone's local transform.
    X,
    /// Vertical translation of the bone's local transform.
    Y,
    /// Local rotation of the bone, in radians.
    Angle,
    /// Uniform scale of the bone's local transform.
    Scale,
}

/// A property of a shape that an animation curve can drive.
///
/// Shape curve values are offsets added to the shape's authored value. A
/// [`ShapeProperty::Z`] curve, for example, is added to
/// [`Shape::z`](crate::Shape::z), so a shape's painting order can change without
/// editing its rest Z.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShapeProperty {
    /// Offset added to the shape's Z order among its siblings.
    Z,
}

/// What a [`Keyframe`] drives.
///
/// Bone and shape targets store the target's flat index, which is how the rest
/// of the asset identifies both. Inserting, removing, or moving a bone or shape
/// therefore rewrites the target of every keyframe that refers to it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KeyframeTarget {
    /// Drives one property of a bone.
    Bone {
        /// Flat index of the bone.
        bone: BoneIndex,
        property: BoneProperty,
    },
    /// Drives one property of a shape.
    Shape {
        /// Flat index of the shape.
        shape: ShapeIndex,
        property: ShapeProperty,
    },
}

/// One value of an animation curve at a point in time.
///
/// The two optional handles are the control points of the one-dimensional
/// Bézier curve between this keyframe and its neighbours.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Keyframe {
    /// Position along the animation, in the range `0..duration`.
    pub timestamp: f32,
    /// The animated value at [`Self::timestamp`].
    pub value: f32,
    /// Bézier handle toward the previous keyframe.
    #[serde(default)]
    pub handle_in: Option<f32>,
    /// Bézier handle toward the next keyframe.
    #[serde(default)]
    pub handle_out: Option<f32>,
    /// What this keyframe animates.
    pub target: KeyframeTarget,
}

impl Keyframe {
    /// Builds a keyframe with no curve handles.
    pub fn new(timestamp: f32, value: f32, target: KeyframeTarget) -> Self {
        Self {
            timestamp,
            value,
            handle_in: None,
            handle_out: None,
            target,
        }
    }
}

/// A single animation clip driving one or more targets over time.
///
/// Keyframes are kept sorted by timestamp and constrained to `0..duration`.
/// Both invariants are enforced whenever the list is built through
/// [`Self::insert_keyframe`], [`Self::set_duration`], or by deserialization.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(from = "AnimationData")]
pub struct Animation {
    /// User-visible name. Empty names are shown by position.
    #[serde(default)]
    pub name: String,
    /// Length of the animation. Keyframes may not fall outside it.
    pub duration: f32,
    /// Whether the animation loops back to the start.
    pub cyclic: bool,
    /// The animation's keyframes, ordered by timestamp.
    #[serde(default)]
    pub keyframes: Vec<Keyframe>,
}

impl Default for Animation {
    fn default() -> Self {
        Self {
            name: String::new(),
            duration: 0.0,
            cyclic: false,
            keyframes: Vec::new(),
        }
    }
}

impl Animation {
    /// Builds an empty animation of the given duration.
    pub fn new(duration: f32) -> Self {
        Self {
            duration,
            ..Default::default()
        }
    }

    /// Inserts a keyframe, keeping the list ordered by timestamp.
    ///
    /// Keyframes outside `0..duration` are ignored, and equal timestamps place
    /// the new keyframe after the existing ones.
    pub fn insert_keyframe(&mut self, keyframe: Keyframe) {
        if !self.contains_time(keyframe.timestamp) {
            return;
        }
        let index = self
            .keyframes
            .partition_point(|existing| existing.timestamp <= keyframe.timestamp);
        self.keyframes.insert(index, keyframe);
    }

    /// Changes the duration, dropping keyframes that now fall outside
    /// `0..duration`.
    pub fn set_duration(&mut self, duration: f32) {
        self.keyframes
            .retain(|keyframe| timestamp_within(keyframe.timestamp, duration));
        self.duration = duration;
    }

    /// Restores the invariants: sorts keyframes by timestamp and drops those
    /// outside `0..duration`.
    pub fn normalize(&mut self) {
        let duration = self.duration;
        self.keyframes
            .retain(|keyframe| timestamp_within(keyframe.timestamp, duration));
        self.keyframes.sort_by(|a, b| {
            a.timestamp
                .partial_cmp(&b.timestamp)
                .unwrap_or(Ordering::Equal)
        });
    }

    fn contains_time(&self, timestamp: f32) -> bool {
        timestamp_within(timestamp, self.duration)
    }

    /// Keyframes grouped by animated target, preserving timestamp order.
    pub fn curve_groups(&self) -> Vec<Vec<&Keyframe>> {
        let mut groups: Vec<Vec<&Keyframe>> = Vec::new();
        'keyframes: for keyframe in &self.keyframes {
            for group in &mut groups {
                if group[0].target == keyframe.target {
                    group.push(keyframe);
                    continue 'keyframes;
                }
            }
            groups.push(vec![keyframe]);
        }
        groups
    }

    /// The bone pose this animation's curves describe at `time`.
    ///
    /// Shape-targeted curves are sampled separately by
    /// [`Self::sample_shape_pose`]. Channels the animation does not drive stay
    /// at rest. Before the first
    /// keyframe a non-cyclic curve holds that keyframe's value, and after the
    /// last it holds the last value. A cyclic curve repeats with a period equal
    /// to the duration, so a sample near either end can interpolate across the
    /// seam using the keyframe at the other end.
    pub fn sample_pose(&self, asset: &Asset, time: f32) -> Pose {
        let mut pose = Pose::new(asset.bone_count());
        let heads = asset.rest_heads();
        let time = clamped_time(self, time);

        for group in self.curve_groups() {
            let Some(first) = group.first() else {
                continue;
            };
            let Some(value) = sample_curve(&group, time, self.duration, self.cyclic) else {
                continue;
            };
            let KeyframeTarget::Bone { bone, property } = &first.target else {
                continue;
            };
            let bone = *bone;
            let mut bone_pose = pose.get(bone);
            if let Some(head) = heads.get(bone) {
                bone_pose.pivot = *head;
            }
            match property {
                BoneProperty::X => bone_pose.translation[0] = value,
                BoneProperty::Y => bone_pose.translation[1] = value,
                BoneProperty::Angle => bone_pose.rotation = value,
                BoneProperty::Scale => bone_pose.scale = value,
            }
            pose.set(bone, bone_pose);
        }
        pose
    }

    /// The shape offsets this animation's curves describe at `time`.
    ///
    /// Only shape-targeted curves contribute; everything they do not drive
    /// stays at rest (zero). Sampling follows the same timing rules as
    /// [`Self::sample_pose`], and the returned offsets are added to the
    /// authored shape values. A curve for [`ShapeProperty::Z`], for example,
    /// is added to the shape's [`z`](crate::Shape::z).
    pub fn sample_shape_pose(&self, asset: &Asset, time: f32) -> ShapePose {
        let count = asset.shape_count();
        let mut pose = ShapePose::new(count);
        let time = clamped_time(self, time);

        for group in self.curve_groups() {
            let Some(first) = group.first() else {
                continue;
            };
            let Some(value) = sample_curve(&group, time, self.duration, self.cyclic) else {
                continue;
            };
            let KeyframeTarget::Shape { shape, property } = &first.target else {
                continue;
            };
            if *shape >= count {
                continue;
            }
            match property {
                ShapeProperty::Z => pose.set(*shape, value),
            }
        }
        pose
    }

    /// Writes `value` into the curve for `property` of `shape` at `time`.
    ///
    /// This is the shape-targeted counterpart of [`Self::set_bone_keyframe`].
    /// The time is clamped the same way, and an existing keyframe for the same
    /// target at that time is updated in place so repeated keying edits rather
    /// than stacks keyframes.
    pub fn set_shape_keyframe(
        &mut self,
        shape: ShapeIndex,
        property: ShapeProperty,
        time: f32,
        value: f32,
    ) {
        let time = clamped_time(self, time);
        let target = KeyframeTarget::Shape { shape, property };
        if let Some(existing) = self
            .keyframes
            .iter_mut()
            .find(|keyframe| keyframe.target == target && keyframe.timestamp == time)
        {
            existing.value = value;
            return;
        }
        self.insert_keyframe(Keyframe::new(time, value, target));
    }

    /// Writes `value` into the curve for `property` of `bone` at `time`.
    ///
    /// The time is clamped to the animation's range, matching the sampler. An
    /// existing keyframe for the same target at that time is updated in place,
    /// keeping its handles, so dragging a bone at a keyframed time edits rather
    /// than stacks keyframes. A time that clamps to exactly the duration lies
    /// outside the half-open keyframe range and is ignored.
    pub fn set_bone_keyframe(
        &mut self,
        bone: BoneIndex,
        property: BoneProperty,
        time: f32,
        value: f32,
    ) {
        let time = clamped_time(self, time);
        let target = KeyframeTarget::Bone { bone, property };
        if let Some(existing) = self
            .keyframes
            .iter_mut()
            .find(|keyframe| keyframe.target == target && keyframe.timestamp == time)
        {
            existing.value = value;
            return;
        }
        self.insert_keyframe(Keyframe::new(time, value, target));
    }

    /// Rewrites every bone translation curve as a vector scaled by `scale` and
    /// rotated by `rotation` radians about the origin.
    ///
    /// A bone's X and Y curves are components of one offset vector in asset
    /// space. A pure scale transforms each channel on its own. A rotation mixes
    /// the channels, so the two curves are split at the union of their
    /// keyframe times and rebuilt as rotated keyframes and Bézier handles. The
    /// split is exact: sampling either curve after the rewrite produces the
    /// rotated and scaled version of the original offset at every time.
    ///
    /// Angle and scale curves are dimensionless and shape Z curves are paint
    /// order, so all of them are left alone.
    pub(crate) fn transform_translations(&mut self, scale: f32, rotation: f32) {
        if rotation == 0.0 {
            // The channels stay independent, so no keyframe surgery is needed.
            for keyframe in &mut self.keyframes {
                let KeyframeTarget::Bone { property, .. } = &keyframe.target else {
                    continue;
                };
                if !matches!(property, BoneProperty::X | BoneProperty::Y) {
                    continue;
                }
                keyframe.value *= scale;
                if let Some(handle) = &mut keyframe.handle_in {
                    *handle *= scale;
                }
                if let Some(handle) = &mut keyframe.handle_out {
                    *handle *= scale;
                }
            }
            return;
        }

        let duration = self.duration;
        let cyclic = self.cyclic && duration.is_finite() && duration > 0.0;
        let mut groups: BTreeMap<BoneIndex, BoneTranslations> = BTreeMap::new();
        let mut retained = Vec::with_capacity(self.keyframes.len());
        for keyframe in std::mem::take(&mut self.keyframes) {
            match &keyframe.target {
                KeyframeTarget::Bone { bone, property } => match property {
                    BoneProperty::X => groups.entry(*bone).or_default().x.push(keyframe),
                    BoneProperty::Y => groups.entry(*bone).or_default().y.push(keyframe),
                    BoneProperty::Angle | BoneProperty::Scale => retained.push(keyframe),
                },
                KeyframeTarget::Shape { .. } => retained.push(keyframe),
            }
        }

        for (bone, curves) in groups {
            retained.extend(transformed_translation_curves(
                bone, curves, scale, rotation, duration, cyclic,
            ));
        }
        retained.sort_by(|a, b| {
            a.timestamp
                .partial_cmp(&b.timestamp)
                .unwrap_or(Ordering::Equal)
        });
        self.keyframes = retained;
    }
}

/// The X and Y curves of one bone, extracted from an animation.
#[derive(Default)]
struct BoneTranslations {
    x: Vec<Keyframe>,
    y: Vec<Keyframe>,
}

/// Rebuilds one bone's X and Y curves on shared keyframes under a scale and
/// rotation.
fn transformed_translation_curves(
    bone: BoneIndex,
    mut curves: BoneTranslations,
    scale: f32,
    rotation: f32,
    duration: f32,
    cyclic: bool,
) -> Vec<Keyframe> {
    // Both channels are split at every time used by either, so their keyframes
    // line up and can be transformed as paired vectors.
    let mut times: Vec<f32> = curves
        .x
        .iter()
        .chain(curves.y.iter())
        .map(|keyframe| keyframe.timestamp)
        .collect();
    times.sort_by(|a, b| a.partial_cmp(b).unwrap_or(Ordering::Equal));
    times.dedup_by(|a, b| *a == *b);
    if times.is_empty() {
        return Vec::new();
    }

    fill_times(
        &mut curves.x,
        &times,
        bone,
        BoneProperty::X,
        duration,
        cyclic,
    );
    fill_times(
        &mut curves.y,
        &times,
        bone,
        BoneProperty::Y,
        duration,
        cyclic,
    );
    materialize_handles(&mut curves.x, cyclic);
    materialize_handles(&mut curves.y, cyclic);

    let (sin, cos) = rotation.sin_cos();
    let rotate = |x: f32, y: f32| (scale * (cos * x - sin * y), scale * (sin * x + cos * y));

    debug_assert_eq!(curves.x.len(), curves.y.len());
    for (x, y) in curves.x.iter_mut().zip(curves.y.iter_mut()) {
        debug_assert_eq!(x.timestamp, y.timestamp);
        let (value_x, value_y) = rotate(x.value, y.value);
        x.value = value_x;
        y.value = value_y;
        let (in_x, in_y) = rotate_optional(x.handle_in, y.handle_in, &rotate);
        let (out_x, out_y) = rotate_optional(x.handle_out, y.handle_out, &rotate);
        x.handle_in = in_x;
        y.handle_in = in_y;
        x.handle_out = out_x;
        y.handle_out = out_y;
    }

    let mut result = Vec::with_capacity(curves.x.len() + curves.y.len());
    for (mut x, mut y) in curves.x.into_iter().zip(curves.y) {
        x.target = KeyframeTarget::Bone {
            bone,
            property: BoneProperty::X,
        };
        y.target = KeyframeTarget::Bone {
            bone,
            property: BoneProperty::Y,
        };
        result.push(x);
        result.push(y);
    }
    result
}

/// Gives a curve a keyframe at each of `times`, splitting existing segments so
/// the curve itself does not change. An empty channel becomes a flat zero
/// curve, which is how a rest translation that is not animated behaves.
fn fill_times(
    keyframes: &mut Vec<Keyframe>,
    times: &[f32],
    bone: BoneIndex,
    property: BoneProperty,
    duration: f32,
    cyclic: bool,
) {
    keyframes.dedup_by(|a, b| a.timestamp == b.timestamp);
    if keyframes.is_empty() {
        *keyframes = times
            .iter()
            .map(|&timestamp| {
                Keyframe::new(timestamp, 0.0, KeyframeTarget::Bone { bone, property })
            })
            .collect();
        return;
    }
    for &timestamp in times {
        split_curve_at(keyframes, timestamp, duration, cyclic);
    }
}

/// Rotates a pair of optional handles, which must either both be present or
/// both be absent because the curves share breakpoints.
fn rotate_optional(
    x: Option<f32>,
    y: Option<f32>,
    rotate: &impl Fn(f32, f32) -> (f32, f32),
) -> (Option<f32>, Option<f32>) {
    match (x, y) {
        (Some(x), Some(y)) => {
            let (x, y) = rotate(x, y);
            (Some(x), Some(y))
        }
        _ => (None, None),
    }
}

/// Replaces every handle with its effective value, so the two channels can be
/// rotated together without relying on the default-handle rules.
fn materialize_handles(keyframes: &mut [Keyframe], cyclic: bool) {
    if keyframes.is_empty() {
        return;
    }
    let handles: Vec<(Option<f32>, Option<f32>)> = {
        let refs: Vec<&Keyframe> = keyframes.iter().collect();
        (0..keyframes.len())
            .map(|index| {
                (
                    effective_handle_in(&refs, index, cyclic),
                    effective_handle_out(&refs, index, cyclic),
                )
            })
            .collect()
    };
    for (keyframe, (handle_in, handle_out)) in keyframes.iter_mut().zip(handles) {
        keyframe.handle_in = handle_in;
        keyframe.handle_out = handle_out;
    }
}

/// Inserts a keyframe at `time` without changing a curve.
fn split_curve_at(keyframes: &mut Vec<Keyframe>, time: f32, duration: f32, cyclic: bool) {
    if keyframes.is_empty() || keyframes.iter().any(|keyframe| keyframe.timestamp == time) {
        return;
    }
    let first = keyframes[0].timestamp;
    let last = keyframes[keyframes.len() - 1].timestamp;

    if !cyclic {
        if time < first {
            insert_hold(keyframes, time, keyframes[0].value, true);
            return;
        }
        if time > last {
            let value = keyframes[keyframes.len() - 1].value;
            insert_hold(keyframes, time, value, false);
            return;
        }
        let before = keyframes
            .windows(2)
            .position(|pair| pair[0].timestamp < time && time < pair[1].timestamp);
        if let Some(before) = before {
            let (start, end) = (keyframes[before].timestamp, keyframes[before + 1].timestamp);
            split_segment(keyframes, before, before + 1, start, end, time, time, false);
        }
        return;
    }

    let before = keyframes
        .windows(2)
        .position(|pair| pair[0].timestamp < time && time < pair[1].timestamp);
    if let Some(before) = before {
        let (start, end) = (keyframes[before].timestamp, keyframes[before + 1].timestamp);
        split_segment(keyframes, before, before + 1, start, end, time, time, true);
        return;
    }

    // The time lies in the seam that wraps from the last keyframe to the first
    // of the next cycle. A time before the first keyframe belongs to the next
    // cycle, so it is evaluated at `time + duration` but stored at `time`.
    if time > last {
        split_segment(
            keyframes,
            keyframes.len() - 1,
            0,
            last,
            first + duration,
            time,
            time,
            true,
        );
    } else if time < first {
        split_segment(
            keyframes,
            keyframes.len() - 1,
            0,
            last,
            first + duration,
            time,
            time + duration,
            true,
        );
    }
}

/// Inserts a keyframe in the constant region before the first keyframe or after
/// the last one, keeping the hold flat.
fn insert_hold(keyframes: &mut Vec<Keyframe>, time: f32, value: f32, before_first: bool) {
    let target = keyframes[0].target.clone();
    let mut inserted = Keyframe::new(time, value, target);
    if before_first {
        inserted.handle_out = Some(value);
        keyframes[0].handle_in = Some(value);
        keyframes.insert(0, inserted);
    } else {
        inserted.handle_in = Some(value);
        if let Some(last) = keyframes.last_mut() {
            last.handle_out = Some(value);
        }
        keyframes.push(inserted);
    }
}

/// Splits the segment between two keyframes, preserving the cubic exactly on
/// both sides.
///
/// `time` is the timestamp the new keyframe is stored at. `parameter_time` is
/// the same moment on the unwrapped timeline, which differs for a cyclic seam
/// split before the first keyframe.
#[allow(clippy::too_many_arguments)]
fn split_segment(
    keyframes: &mut Vec<Keyframe>,
    before: usize,
    after: usize,
    start_time: f32,
    end_time: f32,
    time: f32,
    parameter_time: f32,
    cyclic: bool,
) {
    let gap = end_time - start_time;
    if gap.is_nan() || gap <= 0.0 {
        return;
    }
    let u = ((parameter_time - start_time) / gap).clamp(0.0, 1.0);
    let value_start = keyframes[before].value;
    let value_end = keyframes[after].value;
    let (control_out, control_in) = {
        let refs: Vec<&Keyframe> = keyframes.iter().collect();
        (
            effective_handle_out(&refs, before, cyclic).unwrap_or(value_start),
            effective_handle_in(&refs, after, cyclic).unwrap_or(value_end),
        )
    };
    // Split the cubic with de Casteljau so the two pieces reproduce it exactly.
    // A handle is a Bézier control value, not the curve's value at a third of
    // the segment, so the controls are interpolated rather than sampled.
    let lerp = |a: f32, b: f32, t: f32| a + (b - a) * t;
    let first_out = lerp(value_start, control_out, u);
    let first_in = lerp(control_out, control_in, u);
    let last_out = lerp(control_in, value_end, u);
    let middle_out = lerp(first_out, first_in, u);
    let middle_in = lerp(first_in, last_out, u);
    let value = lerp(middle_out, middle_in, u);

    let mut inserted = Keyframe::new(time, value, keyframes[before].target.clone());
    inserted.handle_in = Some(middle_out);
    inserted.handle_out = Some(middle_in);
    keyframes[before].handle_out = Some(first_out);
    keyframes[after].handle_in = Some(last_out);

    let position = keyframes.partition_point(|keyframe| keyframe.timestamp < time);
    keyframes.insert(position, inserted);
}

/// Whether `timestamp` falls in the valid animation range `0..duration`.
fn timestamp_within(timestamp: f32, duration: f32) -> bool {
    timestamp >= 0.0 && timestamp < duration
}

/// Clamps a playhead time to the animation's `0..=duration` range.
fn clamped_time(animation: &Animation, time: f32) -> f32 {
    let time = if time.is_finite() { time } else { 0.0 };
    if animation.duration.is_finite() && animation.duration > 0.0 {
        time.clamp(0.0, animation.duration)
    } else {
        0.0
    }
}

/// The index of the keyframe after `index`, wrapping for cyclic curves.
fn next_index(len: usize, index: usize, cyclic: bool) -> Option<usize> {
    if index + 1 < len {
        Some(index + 1)
    } else if cyclic {
        Some(0)
    } else {
        None
    }
}

/// The index of the keyframe before `index`, wrapping for cyclic curves.
fn previous_index(len: usize, index: usize, cyclic: bool) -> Option<usize> {
    if index > 0 {
        Some(index - 1)
    } else if cyclic {
        Some(len - 1)
    } else {
        None
    }
}

/// The value of a keyframe's outgoing control point.
///
/// An explicit [`Keyframe::handle_out`] wins. Otherwise the handle sits at the
/// midpoint of the keyframe and the following keyframe's incoming handle, or,
/// when that handle is omitted too, a third of the way toward the following
/// keyframe's value. The last keyframe of a non-cyclic curve has no outgoing
/// handle.
pub fn effective_handle_out(keyframes: &[&Keyframe], index: usize, cyclic: bool) -> Option<f32> {
    let keyframe = *keyframes.get(index)?;
    if let Some(handle) = keyframe.handle_out {
        return Some(handle);
    }
    let next = *keyframes.get(next_index(keyframes.len(), index, cyclic)?)?;
    Some(match next.handle_in {
        Some(handle) => (keyframe.value + handle) / 2.0,
        None => keyframe.value + (next.value - keyframe.value) / 3.0,
    })
}

/// The value of a keyframe's incoming control point.
///
/// Mirrors [`effective_handle_out`]: an explicit [`Keyframe::handle_in`] wins,
/// otherwise the handle is the midpoint toward the previous keyframe's
/// outgoing handle, or a third of the way toward the previous keyframe's value
/// when that handle is omitted. The first keyframe of a non-cyclic curve has no
/// incoming handle.
pub fn effective_handle_in(keyframes: &[&Keyframe], index: usize, cyclic: bool) -> Option<f32> {
    let keyframe = *keyframes.get(index)?;
    if let Some(handle) = keyframe.handle_in {
        return Some(handle);
    }
    let previous = *keyframes.get(previous_index(keyframes.len(), index, cyclic)?)?;
    Some(match previous.handle_out {
        Some(handle) => (keyframe.value + handle) / 2.0,
        None => keyframe.value + (previous.value - keyframe.value) / 3.0,
    })
}

/// Samples one target's curve at `time`.
fn sample_curve(keyframes: &[&Keyframe], time: f32, duration: f32, cyclic: bool) -> Option<f32> {
    let first = *keyframes.first()?;
    let last = *keyframes.last()?;

    if !cyclic || !duration.is_finite() || duration <= 0.0 {
        if time <= first.timestamp {
            return Some(first.value);
        }
        if time >= last.timestamp {
            return Some(last.value);
        }
        for (index, pair) in keyframes.windows(2).enumerate() {
            if time <= pair[1].timestamp {
                return Some(interpolate(
                    pair[0].value,
                    pair[0].timestamp,
                    pair[1].timestamp,
                    pair[1].value,
                    effective_handle_out(keyframes, index, false),
                    effective_handle_in(keyframes, index + 1, false),
                    time,
                ));
            }
        }
        return Some(last.value);
    }

    let previous = keyframes
        .iter()
        .rposition(|keyframe| keyframe.timestamp <= time);
    let next = keyframes
        .iter()
        .position(|keyframe| keyframe.timestamp >= time);
    let (before_index, before_time, after_index, after_value, after_time) = match (previous, next) {
        (Some(before), Some(after)) => (
            before,
            keyframes[before].timestamp,
            after,
            keyframes[after].value,
            keyframes[after].timestamp,
        ),
        (Some(before), None) => (
            before,
            keyframes[before].timestamp,
            0,
            first.value,
            first.timestamp + duration,
        ),
        (None, Some(after)) => (
            keyframes.len() - 1,
            last.timestamp - duration,
            after,
            keyframes[after].value,
            keyframes[after].timestamp,
        ),
        (None, None) => return None,
    };
    if after_time <= before_time {
        return Some(keyframes[before_index].value);
    }
    Some(interpolate(
        keyframes[before_index].value,
        before_time,
        after_time,
        after_value,
        effective_handle_out(keyframes, before_index, true),
        effective_handle_in(keyframes, after_index, true),
        time,
    ))
}

/// Evaluates the cubic between two keyframes at `time`.
///
/// `start_time` is where the first keyframe sits on the evaluated timeline. For
/// a cyclic seam it can be the stored timestamp shifted by a whole period, so
/// the stored timestamp must not be used directly. The horizontal control
/// points sit a third of the way along the gap, so the Bézier parameter is
/// exactly the normalized time and only the values need interpolating.
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

/// A cubic Bézier in one dimension.
fn cubic(p0: f32, p1: f32, p2: f32, p3: f32, s: f32) -> f32 {
    let u = 1.0 - s;
    u * u * u * p0 + 3.0 * u * u * s * p1 + 3.0 * u * s * s * p2 + s * s * s * p3
}

/// Deserialization form of [`Animation`], normalized on conversion so a loaded
/// asset always satisfies the keyframe invariants.
#[derive(Deserialize)]
struct AnimationData {
    #[serde(default)]
    name: String,
    #[serde(default)]
    duration: f32,
    #[serde(default)]
    cyclic: bool,
    #[serde(default)]
    keyframes: Vec<Keyframe>,
}

impl From<AnimationData> for Animation {
    fn from(data: AnimationData) -> Self {
        let mut animation = Self {
            name: data.name,
            duration: data.duration,
            cyclic: data.cyclic,
            keyframes: data.keyframes,
        };
        animation.normalize();
        animation
    }
}

impl Asset {
    /// The index of the first animation named `name`.
    ///
    /// Animation names are descriptive and need not be unique, so this returns
    /// the first match in document order.
    pub fn animation_index_by_name(&self, name: &str) -> Option<usize> {
        self.animations
            .iter()
            .position(|animation| animation.name == name)
    }

    /// The blended bone pose of several animation instances at once.
    ///
    /// Shape offsets are blended separately by [`Self::sample_shape_pose`].
    /// Out-of-range indices, non-finite times, and non-positive weights are
    /// ignored. The remaining poses are added together relative to rest by
    /// [`Pose::blend`], which does not normalize weights: each instance
    /// contributes fully at weight `1`, and weights summing to `1` behave like
    /// a weighted average. When no instance contributes, the result is the
    /// rest pose.
    ///
    /// Every sampled pose is re-anchored to the bone's rest head before
    /// blending, so a bone that one animation leaves at rest still blends
    /// correctly with a bone another animation rotates. This is the same pivot
    /// the rig view uses.
    pub fn sample_pose(&self, animations: &[AnimationInstance]) -> Pose {
        let heads = self.rest_heads();
        let mut sampled = Vec::with_capacity(animations.len());

        for &(index, time, weight) in animations {
            let Some(animation) = self.animations.get(index) else {
                continue;
            };
            if !time.is_finite() || !weight.is_finite() || weight <= 0.0 {
                continue;
            }
            let mut pose = animation.sample_pose(self, time);
            for (bone, head) in heads.iter().enumerate() {
                pose.set(bone, pose.get(bone).with_pivot(*head));
            }
            sampled.push((pose, weight));
        }

        let samples: Vec<(&Pose, f32)> = sampled
            .iter()
            .map(|(pose, weight)| (pose, *weight))
            .collect();
        Pose::blend(self.bone_count(), &samples)
    }

    /// The blended shape offsets of several animation instances at once.
    ///
    /// This is the shape-targeted counterpart of [`Self::sample_pose`].
    /// Out-of-range indices, non-finite times, and non-positive weights are
    /// ignored. Each instance contributes its offset scaled by its weight, so
    /// weights summing to `1` behave like a weighted average. When no instance
    /// contributes, the result is a rest pose with every offset at zero.
    pub fn sample_shape_pose(&self, animations: &[AnimationInstance]) -> ShapePose {
        let mut sampled = Vec::with_capacity(animations.len());

        for &(index, time, weight) in animations {
            let Some(animation) = self.animations.get(index) else {
                continue;
            };
            if !time.is_finite() || !weight.is_finite() || weight <= 0.0 {
                continue;
            }
            sampled.push((animation.sample_shape_pose(self, time), weight));
        }

        let samples: Vec<(&ShapePose, f32)> = sampled
            .iter()
            .map(|(pose, weight)| (pose, *weight))
            .collect();
        ShapePose::blend(self.shape_count(), &samples)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bone::Bone;
    use crate::shape::Shape;

    fn keyframe(timestamp: f32, value: f32) -> Keyframe {
        Keyframe::new(
            timestamp,
            value,
            KeyframeTarget::Bone {
                bone: 0,
                property: BoneProperty::X,
            },
        )
    }

    fn bone_keyframe(
        timestamp: f32,
        value: f32,
        bone: BoneIndex,
        property: BoneProperty,
    ) -> Keyframe {
        Keyframe::new(timestamp, value, KeyframeTarget::Bone { bone, property })
    }

    fn keyframe_times(animation: &Animation) -> Vec<f32> {
        animation
            .keyframes
            .iter()
            .map(|keyframe| keyframe.timestamp)
            .collect()
    }

    fn curve(duration: f32, cyclic: bool, keyframes: Vec<Keyframe>) -> Animation {
        let mut animation = Animation::new(duration);
        animation.cyclic = cyclic;
        for keyframe in keyframes {
            animation.insert_keyframe(keyframe);
        }
        animation
    }

    #[test]
    fn non_cyclic_curves_hold_their_endpoints() {
        let animation = curve(4.0, false, vec![keyframe(1.0, 10.0), keyframe(3.0, 30.0)]);
        let groups = animation.curve_groups();
        let values = &groups[0];

        assert!((sample_curve(values, 0.0, 4.0, false).unwrap() - 10.0).abs() < 1e-4);
        assert!((sample_curve(values, 2.0, 4.0, false).unwrap() - 20.0).abs() < 1e-4);
        assert!((sample_curve(values, 5.0, 4.0, false).unwrap() - 30.0).abs() < 1e-4);
    }

    #[test]
    fn cyclic_curves_wrap_across_the_seam() {
        let animation = curve(2.0, true, vec![keyframe(0.5, 0.0), keyframe(1.5, 100.0)]);
        let groups = animation.curve_groups();
        let values = &groups[0];

        // Before the first keyframe the sample uses the previous cycle's last
        // keyframe, and after the last it uses the next cycle's first. With no
        // handles anywhere the default control points make each seam straight.
        assert!((sample_curve(values, 0.0, 2.0, true).unwrap() - 50.0).abs() < 1e-3);
        assert!((sample_curve(values, 1.9, 2.0, true).unwrap() - 60.0).abs() < 1e-2);
    }

    #[test]
    fn omitted_handles_default_to_midpoints_and_thirds() {
        let mut neighbour_handle = keyframe(2.0, 30.0);
        neighbour_handle.handle_in = Some(45.0);
        let animation = curve(4.0, false, vec![keyframe(0.0, 10.0), neighbour_handle]);
        let values = &animation.curve_groups()[0];

        // The explicit incoming handle on the second keyframe is a midpoint.
        assert_eq!(effective_handle_out(values, 0, false), Some(27.5));
        assert_eq!(effective_handle_in(values, 1, false), Some(45.0));
        // A curve end has no handle beyond it.
        assert_eq!(effective_handle_out(values, 1, false), None);
        assert_eq!(effective_handle_in(values, 0, false), None);

        // With no handle on the neighbour, the default is a third of the way.
        let animation = curve(4.0, false, vec![keyframe(0.0, 10.0), keyframe(2.0, 40.0)]);
        let values = &animation.curve_groups()[0];
        assert_eq!(effective_handle_out(values, 0, false), Some(20.0));
        assert_eq!(effective_handle_in(values, 1, false), Some(30.0));
    }

    #[test]
    fn cyclic_default_handles_wrap_across_the_seam() {
        let animation = curve(2.0, true, vec![keyframe(0.5, 0.0), keyframe(1.5, 100.0)]);
        let values = &animation.curve_groups()[0];

        // The first keyframe's incoming handle looks back to the last keyframe.
        let handle_in = effective_handle_in(values, 0, true).unwrap();
        assert!((handle_in - 100.0 / 3.0).abs() < 1e-4);
        let handle_out = effective_handle_out(values, 1, true).unwrap();
        assert!((handle_out - 200.0 / 3.0).abs() < 1e-4);
    }

    #[test]
    fn sample_pose_maps_channels_to_bone_properties() {
        let mut asset = Asset::new();
        asset.bones.push(Bone {
            name: "Arm".into(),
            head: Some([10.0, 20.0]),
            tail: [10.0, 40.0],
            children: Vec::new(),
        });
        let mut animation = Animation::new(1.0);
        for (property, value) in [
            (BoneProperty::X, 3.0),
            (BoneProperty::Y, -4.0),
            (BoneProperty::Angle, 0.5),
            (BoneProperty::Scale, 2.0),
        ] {
            animation.insert_keyframe(Keyframe::new(
                0.0,
                value,
                KeyframeTarget::Bone { bone: 0, property },
            ));
        }

        let pose = animation.sample_pose(&asset, 0.5);
        let arm = pose.get(0);

        assert_eq!(arm.translation, [3.0, -4.0]);
        assert_eq!(arm.rotation, 0.5);
        assert_eq!(arm.scale, 2.0);
        // The pose pivots about the bone's rest head, like the rig view.
        assert_eq!(arm.pivot, [10.0, 20.0]);
    }

    #[test]
    fn sample_shape_pose_maps_z_curves_to_offsets() {
        let asset = Asset {
            shapes: vec![
                Shape::default(),
                Shape {
                    children: vec![Shape::default()],
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let mut animation = Animation::new(1.0);
        // The nested child is flat shape index 2.
        animation.insert_keyframe(Keyframe::new(
            0.0,
            4.0,
            KeyframeTarget::Shape {
                shape: 2,
                property: ShapeProperty::Z,
            },
        ));

        let pose = animation.sample_shape_pose(&asset, 0.5);
        assert_eq!(pose.z_offsets(), &[0.0, 0.0, 4.0]);
    }

    #[test]
    fn shape_offsets_blend_across_instances() {
        let asset = Asset {
            shapes: vec![Shape::default()],
            ..Default::default()
        };
        let mut first = Animation::new(1.0);
        first.insert_keyframe(Keyframe::new(
            0.0,
            4.0,
            KeyframeTarget::Shape {
                shape: 0,
                property: ShapeProperty::Z,
            },
        ));
        let mut second = Animation::new(1.0);
        second.insert_keyframe(Keyframe::new(
            0.0,
            8.0,
            KeyframeTarget::Shape {
                shape: 0,
                property: ShapeProperty::Z,
            },
        ));
        let mut asset = asset;
        asset.animations = vec![first, second];

        let pose = asset.sample_shape_pose(&[(0, 0.0, 0.25), (1, 0.0, 0.75)]);
        assert!((pose.get(0) - 7.0).abs() < 1e-5);

        // Invalid instances leave every offset at rest.
        assert!(asset.sample_shape_pose(&[]).is_identity());
        assert!(asset.sample_shape_pose(&[(9, 0.0, 1.0)]).is_identity());
    }

    #[test]
    fn setting_a_shape_keyframe_inserts_or_replaces() {
        let mut animation = Animation::new(4.0);

        animation.set_shape_keyframe(1, ShapeProperty::Z, 1.0, 2.0);
        assert_eq!(animation.keyframes.len(), 1);
        assert_eq!(animation.keyframes[0].value, 2.0);
        assert_eq!(
            animation.keyframes[0].target,
            KeyframeTarget::Shape {
                shape: 1,
                property: ShapeProperty::Z,
            }
        );

        // The same target at the same time updates in place, keeping handles.
        animation.keyframes[0].handle_out = Some(3.0);
        animation.set_shape_keyframe(1, ShapeProperty::Z, 1.0, 5.0);
        assert_eq!(animation.keyframes.len(), 1);
        assert_eq!(animation.keyframes[0].value, 5.0);
        assert_eq!(animation.keyframes[0].handle_out, Some(3.0));

        // A different shape or time is its own keyframe.
        animation.set_shape_keyframe(0, ShapeProperty::Z, 1.0, 0.0);
        animation.set_shape_keyframe(1, ShapeProperty::Z, 2.0, 0.0);
        assert_eq!(animation.keyframes.len(), 3);
    }

    #[test]
    fn setting_a_bone_keyframe_inserts_or_replaces() {
        let mut animation = Animation::new(4.0);

        animation.set_bone_keyframe(0, BoneProperty::Angle, 1.0, 0.5);
        assert_eq!(animation.keyframes.len(), 1);
        assert_eq!(animation.keyframes[0].timestamp, 1.0);
        assert_eq!(animation.keyframes[0].value, 0.5);

        // The same target at the same time updates in place, keeping handles.
        animation.keyframes[0].handle_out = Some(0.9);
        animation.set_bone_keyframe(0, BoneProperty::Angle, 1.0, 0.75);
        assert_eq!(animation.keyframes.len(), 1);
        assert_eq!(animation.keyframes[0].value, 0.75);
        assert_eq!(animation.keyframes[0].handle_out, Some(0.9));

        // A different bone, property, or time is its own keyframe.
        animation.set_bone_keyframe(1, BoneProperty::Angle, 1.0, 0.1);
        animation.set_bone_keyframe(0, BoneProperty::X, 1.0, 2.0);
        animation.set_bone_keyframe(0, BoneProperty::Angle, 2.0, 0.2);
        assert_eq!(animation.keyframes.len(), 4);
    }

    #[test]
    fn setting_a_bone_keyframe_outside_the_half_open_range_is_ignored() {
        let mut animation = Animation::new(2.0);

        // Both ends clamp to the closed sampler range, but the duration itself
        // is outside the half-open keyframe range.
        animation.set_bone_keyframe(0, BoneProperty::Scale, 9.0, 3.0);
        assert!(animation.keyframes.is_empty());

        animation.set_bone_keyframe(0, BoneProperty::Scale, -1.0, 3.0);
        assert_eq!(animation.keyframes.len(), 1);
        assert_eq!(animation.keyframes[0].timestamp, 0.0);
    }

    #[test]
    fn keyframes_at_exactly_the_duration_are_dropped() {
        let mut animation = Animation::new(2.0);
        animation.keyframes = vec![keyframe(0.0, 0.0), keyframe(2.0, 10.0)];

        animation.normalize();

        assert_eq!(animation.keyframes.len(), 1);
        assert_eq!(animation.keyframes[0].timestamp, 0.0);
    }

    #[test]
    fn channels_missing_from_the_animation_stay_at_rest() {
        let mut asset = Asset::new();
        for (name, head) in [("Arm", [10.0, 20.0]), ("Leg", [20.0, 20.0])] {
            asset.bones.push(Bone {
                name: name.into(),
                head: Some(head),
                tail: [20.0, 40.0],
                children: Vec::new(),
            });
        }
        let animation = curve(2.0, false, vec![keyframe(0.0, 5.0)]);

        let pose = animation.sample_pose(&asset, 1.0);

        assert_eq!(pose.get(0).translation, [5.0, 0.0]);
        assert!(pose.get(1).is_identity());
    }

    #[test]
    fn animations_round_trip_through_json() {
        let mut animation = Animation::new(5.0);
        animation.cyclic = true;
        let mut keyframe = bone_keyframe(1.5, 10.0, 0, BoneProperty::Angle);
        keyframe.handle_in = Some(0.25);
        keyframe.handle_out = Some(0.75);
        animation.insert_keyframe(keyframe);

        let asset = Asset {
            animations: vec![animation],
            ..Default::default()
        };

        let json = asset.to_json_string().unwrap();
        assert!(json.contains("\"handle_in\""));
        assert!(json.contains("\"handle_out\""));
        assert_eq!(Asset::from_json_str(&json).unwrap(), asset);

        // Every field is written even when empty, so postcard's positional
        // decoding stays aligned with the struct.
        let empty = Asset {
            animations: vec![Animation::new(1.0)],
            ..Default::default()
        };
        let json = empty.to_json_string().unwrap();
        assert!(json.contains("\"keyframes\""));
        assert!(json.contains("\"name\""));
    }

    #[test]
    fn animation_names_round_trip() {
        let named = Asset {
            animations: vec![Animation {
                name: "Walk".into(),
                ..Animation::new(2.0)
            }],
            ..Default::default()
        };
        let json = named.to_json_string().unwrap();
        assert!(json.contains("\"Walk\""));
        assert_eq!(Asset::from_json_str(&json).unwrap(), named);

        let unnamed = Asset {
            animations: vec![Animation::new(2.0)],
            ..Default::default()
        };
        assert_eq!(
            Asset::from_json_str(&unnamed.to_json_string().unwrap()).unwrap(),
            unnamed
        );
    }

    #[test]
    fn inserting_keyframes_keeps_them_ordered_and_in_range() {
        let mut animation = Animation::new(10.0);
        animation.insert_keyframe(bone_keyframe(5.0, 0.0, 1, BoneProperty::X));
        animation.insert_keyframe(bone_keyframe(1.0, 0.0, 0, BoneProperty::X));
        animation.insert_keyframe(bone_keyframe(5.0, 0.0, 2, BoneProperty::Y));
        animation.insert_keyframe(bone_keyframe(-1.0, 0.0, 3, BoneProperty::X));
        animation.insert_keyframe(bone_keyframe(11.0, 0.0, 4, BoneProperty::X));

        assert_eq!(keyframe_times(&animation), vec![1.0, 5.0, 5.0]);

        // Equal timestamps keep insertion order.
        let bones: Vec<BoneIndex> = animation
            .keyframes
            .iter()
            .map(|keyframe| match &keyframe.target {
                KeyframeTarget::Bone { bone, .. } => *bone,
                KeyframeTarget::Shape { .. } => unreachable!(),
            })
            .collect();
        assert_eq!(bones, vec![0, 1, 2]);
    }

    #[test]
    fn changing_duration_drops_outlying_keyframes() {
        let mut animation = Animation::new(10.0);
        for timestamp in [0.0, 2.0, 8.0, 10.0] {
            animation.insert_keyframe(bone_keyframe(timestamp, timestamp, 0, BoneProperty::X));
        }

        animation.set_duration(5.0);

        assert_eq!(animation.duration, 5.0);
        assert_eq!(keyframe_times(&animation), vec![0.0, 2.0]);
    }

    #[test]
    fn animations_are_found_by_name() {
        let asset = Asset {
            animations: vec![
                Animation::new(1.0),
                Animation {
                    name: "Walk".into(),
                    ..Animation::new(2.0)
                },
            ],
            ..Default::default()
        };

        assert_eq!(asset.animation_index_by_name("Walk"), Some(1));
        assert_eq!(asset.animation_index_by_name("Missing"), None);
    }

    #[test]
    fn sample_pose_adds_instances_at_full_weight() {
        let mut asset = Asset::new();
        asset.bones.push(Bone {
            name: "Arm".into(),
            head: Some([10.0, 20.0]),
            tail: [10.0, 40.0],
            children: Vec::new(),
        });

        let mut first = Animation::new(1.0);
        first.insert_keyframe(bone_keyframe(0.0, 0.2, 0, BoneProperty::Angle));
        let mut second = Animation::new(1.0);
        second.insert_keyframe(bone_keyframe(0.0, 0.3, 0, BoneProperty::Angle));
        asset.animations = vec![first, second];

        // Both layers contribute fully instead of being averaged.
        let pose = asset.sample_pose(&[(0, 0.0, 1.0), (1, 0.0, 1.0)]);
        assert!((pose.get(0).rotation - 0.5).abs() < 1e-5);
        assert_eq!(pose.get(0).pivot, [10.0, 20.0]);
    }

    #[test]
    fn sample_pose_blends_animation_instances_by_weight() {
        let mut asset = Asset::new();
        asset.bones.push(Bone {
            name: "Arm".into(),
            head: Some([10.0, 20.0]),
            tail: [10.0, 40.0],
            children: Vec::new(),
        });

        let mut rest = Animation::new(1.0);
        rest.insert_keyframe(bone_keyframe(0.0, 0.0, 0, BoneProperty::Angle));
        let mut turned = Animation::new(1.0);
        turned.insert_keyframe(bone_keyframe(
            0.0,
            std::f32::consts::FRAC_PI_2,
            0,
            BoneProperty::Angle,
        ));
        asset.animations = vec![rest, turned];

        let pose = asset.sample_pose(&[(0, 0.0, 0.25), (1, 0.0, 0.75)]);
        let arm = pose.get(0);
        assert!((arm.rotation - std::f32::consts::FRAC_PI_2 * 0.75).abs() < 1e-5);
        assert_eq!(arm.pivot, [10.0, 20.0]);
        // An unused channel stays at its animation value, so the blend is a
        // weighted average of one animation at rest and one rotated.
        assert_eq!(arm.translation, [0.0, 0.0]);
        assert!((arm.scale - 1.0).abs() < 1e-5);
    }

    #[test]
    fn sample_pose_ignores_empty_and_invalid_instances() {
        let asset = Asset::new();
        assert!(asset.sample_pose(&[]).is_identity());
        assert!(
            asset
                .sample_pose(&[(7, 0.0, 1.0), (0, 0.0, 0.0)])
                .is_identity()
        );

        // Missing bones and animations leave a rest pose rather than panicking.
        let mut asset = Asset::new();
        asset.bones.push(Bone::default());
        asset.animations.push(Animation::new(1.0));
        assert!(asset.sample_pose(&[(0, f32::NAN, 1.0)]).is_identity());
    }

    #[test]
    fn loading_animations_normalizes_their_keyframes() {
        let json = r#"{
            "type": "viveco",
            "version": 0,
            "animations": [{
                "duration": 4.0,
                "cyclic": true,
                "keyframes": [
                    {"timestamp": 3.0, "value": 3.0,
                     "target": {"bone": {"bone": 0, "property": "x"}}},
                    {"timestamp": -1.0, "value": 0.0,
                     "target": {"bone": {"bone": 1, "property": "y"}}},
                    {"timestamp": 1.0, "value": 1.0,
                     "target": {"bone": {"bone": 2, "property": "angle"}}},
                    {"timestamp": 9.0, "value": 9.0,
                     "target": {"bone": {"bone": 3, "property": "scale"}}}
                ]
            }]
        }"#;

        let asset = Asset::from_json_str(json).unwrap();
        let animation = &asset.animations[0];
        assert!(animation.cyclic);
        assert_eq!(keyframe_times(animation), vec![1.0, 3.0]);
    }
}
