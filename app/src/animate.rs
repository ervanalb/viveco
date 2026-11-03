//! Animation mode: selecting, creating, and deleting the document's animations.
//!
//! Animations are anonymous, so the editor addresses them by their index in
//! [`Document::animations`](crate::model::Document::animations). Creating and
//! deleting therefore tracks the selection as indices shift.
//!
//! Everything else about an animation (its keyframes) is document data handled
//! by the model; this module only edits the properties exposed by the animate
//! panel.

use crate::editor::{AnimationRename, EditorState};
use crate::model::{Animation, BoneProperty, Document, Keyframe, KeyframeTarget};
use crate::pose::Pose;

/// Duration given to a newly created animation, in seconds.
pub const DEFAULT_ANIMATION_DURATION: f32 = 5.0;

/// Smallest duration an animation may have.
pub const MIN_ANIMATION_DURATION: f32 = 0.0;

/// Appends a new empty animation and selects it, returning its index.
pub fn add_animation(editor: &mut EditorState) -> usize {
    let index = editor.document.animations.len();
    editor
        .document
        .animations
        .push(Animation::new(DEFAULT_ANIMATION_DURATION));
    editor.selected_animation = Some(index);
    editor.structure_revision += 1;
    index
}

/// Removes the animation at `index` and keeps the selection on a neighbour.
///
/// Does nothing when `index` does not name an animation.
pub fn delete_animation(editor: &mut EditorState, index: usize) {
    if index >= editor.document.animations.len() {
        return;
    }
    editor.document.animations.remove(index);
    editor.selected_animation = match editor.selected_animation {
        Some(selected) if selected > index => Some(selected - 1),
        Some(selected) if selected == index => {
            if index < editor.document.animations.len() {
                Some(index)
            } else if index > 0 {
                Some(index - 1)
            } else {
                None
            }
        }
        other => other,
    };
    // An in-progress rename cannot outlive the row it belongs to.
    editor.animation_renaming = match editor.animation_renaming.take() {
        Some(mut rename) if rename.index > index => {
            rename.index -= 1;
            Some(rename)
        }
        Some(rename) if rename.index == index => None,
        other => other,
    };
    editor.structure_revision += 1;
}

/// Removes the selected animation, if there is one.
pub fn delete_selected_animation(editor: &mut EditorState) {
    if let Some(index) = editor.selected_animation {
        delete_animation(editor, index);
    }
}

/// The currently selected animation, if any.
pub fn selected_animation(editor: &EditorState) -> Option<&Animation> {
    editor
        .selected_animation
        .and_then(|index| editor.document.animations.get(index))
}

/// The name shown for an animation, falling back to its position when it has
/// no name.
pub fn animation_label(animation: &Animation, index: usize) -> String {
    if animation.name.is_empty() {
        format!("Animation {}", index + 1)
    } else {
        animation.name.clone()
    }
}

/// Starts an inline rename of the animation at `index`, selecting it and
/// seeding the text field with its current label.
pub fn begin_animation_rename(editor: &mut EditorState, index: usize) {
    let Some(animation) = editor.document.animations.get(index) else {
        return;
    };
    let buffer = animation_label(animation, index);
    editor.selected_animation = Some(index);
    editor.animation_renaming = Some(AnimationRename { index, buffer });
}

/// Writes the in-progress rename into the animation, if any.
pub fn commit_animation_rename(editor: &mut EditorState) {
    let Some(rename) = editor.animation_renaming.take() else {
        return;
    };
    if let Some(animation) = editor.document.animations.get_mut(rename.index) {
        animation.name = rename.buffer;
    }
}

/// Abandons the in-progress rename, if any.
pub fn cancel_animation_rename(editor: &mut EditorState) {
    editor.animation_renaming = None;
}

/// Sets an animation's duration, dropping any keyframes that now fall outside
/// it.
pub fn set_duration(editor: &mut EditorState, index: usize, duration: f32) {
    if let Some(animation) = editor.document.animations.get_mut(index) {
        animation.set_duration(duration.max(MIN_ANIMATION_DURATION));
    }
}

/// Sets whether an animation loops.
pub fn set_cyclic(editor: &mut EditorState, index: usize, cyclic: bool) {
    if let Some(animation) = editor.document.animations.get_mut(index) {
        animation.cyclic = cyclic;
    }
}

/// Keyframes grouped by animated target, preserving timestamp order.
pub(crate) fn curve_groups(animation: &Animation) -> Vec<Vec<&Keyframe>> {
    let mut groups: Vec<Vec<&Keyframe>> = Vec::new();
    'keyframes: for keyframe in &animation.keyframes {
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

/// The bone pose an animation's curves describe at `time`.
///
/// Channels the animation does not drive stay at rest. Before the first
/// keyframe a non-cyclic curve holds that keyframe's value, and after the last
/// it holds the last value. A cyclic curve repeats with a period equal to the
/// duration, so a sample near either end can interpolate across the seam using
/// the keyframe at the other end.
pub fn sample_pose(document: &Document, animation: &Animation, time: f32) -> Pose {
    let mut pose = Pose::default();
    let heads = crate::pose::rest_heads(document);
    let time = clamped_time(animation, time);

    for group in curve_groups(animation) {
        let Some(first) = group.first() else {
            continue;
        };
        let Some(value) = sample_curve(&group, time, animation.duration, animation.cyclic) else {
            continue;
        };
        let KeyframeTarget::Bone { bone, property } = &first.target else {
            continue;
        };
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
        pose.set(bone.clone(), bone_pose);
    }
    pose
}

/// Writes `value` into the curve for `property` of `bone` at `time`.
///
/// The time is clamped to the animation's range, matching the sampler. An
/// existing keyframe for the same target at that time is updated in place,
/// keeping its handles, so dragging a bone at a keyframed time edits rather
/// than stacks keyframes.
pub(crate) fn set_bone_keyframe(
    animation: &mut Animation,
    bone: &str,
    property: BoneProperty,
    time: f32,
    value: f32,
) {
    let time = clamped_time(animation, time);
    let target = KeyframeTarget::Bone {
        bone: bone.into(),
        property,
    };
    if let Some(existing) = animation
        .keyframes
        .iter_mut()
        .find(|keyframe| keyframe.target == target && keyframe.timestamp == time)
    {
        existing.value = value;
        return;
    }
    animation.insert_keyframe(Keyframe::new(time, value, target));
}

/// Advances the playhead by one frame of playback.
///
/// Returns the new time and whether playback should continue. A cyclic
/// animation wraps around either end; otherwise playback stops on reaching the
/// end, or the start when the velocity is negative. An animation with no
/// duration cannot play.
pub fn advance_playhead(
    playhead: f32,
    velocity: f32,
    delta: f32,
    duration: f32,
    cyclic: bool,
) -> (f32, bool) {
    let playhead = if playhead.is_finite() { playhead } else { 0.0 };
    if !duration.is_finite() || duration <= 0.0 {
        return (0.0, false);
    }
    if !velocity.is_finite() || !delta.is_finite() || velocity == 0.0 {
        return (playhead.clamp(0.0, duration), true);
    }

    let next = playhead + velocity * delta;
    if cyclic {
        return (next.rem_euclid(duration), true);
    }
    if next <= 0.0 {
        (0.0, false)
    } else if next >= duration {
        (duration, false)
    } else {
        (next, true)
    }
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
pub(crate) fn effective_handle_out(
    keyframes: &[&Keyframe],
    index: usize,
    cyclic: bool,
) -> Option<f32> {
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
/// outgoing handle, or a third of the way toward the previous keyframe's
/// value when that handle is omitted. The first keyframe of a non-cyclic curve
/// has no incoming handle.
pub(crate) fn effective_handle_in(
    keyframes: &[&Keyframe],
    index: usize,
    cyclic: bool,
) -> Option<f32> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Bone, BoneProperty, Document, Keyframe, KeyframeTarget};

    fn editor_with_keyframes() -> EditorState {
        let mut editor = EditorState::new(Document::new());
        let index = add_animation(&mut editor);
        let animation = &mut editor.document.animations[index];
        animation.duration = 10.0;
        for timestamp in [1.0, 9.0] {
            animation.insert_keyframe(Keyframe::new(
                timestamp,
                0.0,
                KeyframeTarget::Bone {
                    bone: "Arm".into(),
                    property: BoneProperty::X,
                },
            ));
        }
        editor
    }

    #[test]
    fn adding_an_animation_selects_it() {
        let mut editor = EditorState::new(Document::new());

        let first = add_animation(&mut editor);
        let second = add_animation(&mut editor);

        assert_eq!((first, second), (0, 1));
        assert_eq!(editor.selected_animation, Some(1));
        assert_eq!(editor.document.animations.len(), 2);
        assert_eq!(editor.structure_revision, 2);
    }

    #[test]
    fn deleting_keeps_the_selection_on_a_neighbour() {
        let mut editor = EditorState::new(Document::new());
        add_animation(&mut editor);
        add_animation(&mut editor);
        add_animation(&mut editor);

        // Deleting below the selection shifts its index down.
        editor.selected_animation = Some(2);
        delete_animation(&mut editor, 0);
        assert_eq!(editor.selected_animation, Some(1));

        // Deleting the selection selects the animation that takes its place.
        delete_animation(&mut editor, 1);
        assert_eq!(editor.selected_animation, Some(0));

        // Deleting the last animation clears the selection.
        delete_animation(&mut editor, 0);
        assert_eq!(editor.selected_animation, None);
        assert!(editor.document.animations.is_empty());
    }

    #[test]
    fn changing_duration_drops_outlying_keyframes() {
        let mut editor = editor_with_keyframes();

        set_duration(&mut editor, 0, 5.0);

        let animation = &editor.document.animations[0];
        assert_eq!(animation.duration, 5.0);
        assert_eq!(animation.keyframes.len(), 1);
        assert_eq!(animation.keyframes[0].timestamp, 1.0);
    }

    #[test]
    fn negative_durations_are_clamped() {
        let mut editor = editor_with_keyframes();

        set_duration(&mut editor, 0, -4.0);

        assert_eq!(editor.document.animations[0].duration, 0.0);
        assert!(editor.document.animations[0].keyframes.is_empty());
    }

    #[test]
    fn unnamed_animations_are_labelled_by_position() {
        let mut editor = EditorState::new(Document::new());
        add_animation(&mut editor);
        add_animation(&mut editor);

        assert_eq!(
            animation_label(&editor.document.animations[0], 0),
            "Animation 1"
        );

        editor.document.animations[1].name = "Walk".into();
        assert_eq!(animation_label(&editor.document.animations[1], 1), "Walk");
    }

    #[test]
    fn renaming_an_animation_updates_its_name() {
        let mut editor = EditorState::new(Document::new());
        let index = add_animation(&mut editor);

        begin_animation_rename(&mut editor, index);
        assert_eq!(editor.selected_animation, Some(index));
        assert_eq!(
            editor.animation_renaming.as_ref().unwrap().buffer,
            "Animation 1"
        );

        editor.animation_renaming.as_mut().unwrap().buffer = "Walk".into();
        commit_animation_rename(&mut editor);

        assert!(editor.animation_renaming.is_none());
        assert_eq!(editor.document.animations[index].name, "Walk");

        // Cancelling leaves the stored name alone.
        begin_animation_rename(&mut editor, index);
        editor.animation_renaming.as_mut().unwrap().buffer = "Run".into();
        cancel_animation_rename(&mut editor);
        assert_eq!(editor.document.animations[index].name, "Walk");
    }

    #[test]
    fn deleting_below_a_rename_shifts_its_index() {
        let mut editor = EditorState::new(Document::new());
        add_animation(&mut editor);
        add_animation(&mut editor);
        add_animation(&mut editor);
        begin_animation_rename(&mut editor, 2);

        delete_animation(&mut editor, 0);
        assert_eq!(editor.animation_renaming.as_ref().unwrap().index, 1);

        // Deleting the animation being renamed abandons the edit.
        delete_animation(&mut editor, 1);
        assert!(editor.animation_renaming.is_none());
    }

    #[test]
    fn cyclic_can_be_toggled() {
        let mut editor = editor_with_keyframes();

        set_cyclic(&mut editor, 0, true);
        assert!(editor.document.animations[0].cyclic);

        set_cyclic(&mut editor, 0, false);
        assert!(!editor.document.animations[0].cyclic);
    }

    fn keyframe(timestamp: f32, value: f32) -> Keyframe {
        Keyframe::new(
            timestamp,
            value,
            KeyframeTarget::Bone {
                bone: "Arm".into(),
                property: BoneProperty::X,
            },
        )
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
        let groups = curve_groups(&animation);
        let values = &groups[0];

        assert!((sample_curve(values, 0.0, 4.0, false).unwrap() - 10.0).abs() < 1e-4);
        assert!((sample_curve(values, 2.0, 4.0, false).unwrap() - 20.0).abs() < 1e-4);
        assert!((sample_curve(values, 5.0, 4.0, false).unwrap() - 30.0).abs() < 1e-4);
    }

    #[test]
    fn cyclic_curves_wrap_across_the_seam() {
        let animation = curve(2.0, true, vec![keyframe(0.5, 0.0), keyframe(1.5, 100.0)]);
        let groups = curve_groups(&animation);
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
        let values = &curve_groups(&animation)[0];

        // The explicit incoming handle on the second keyframe is a midpoint.
        assert_eq!(effective_handle_out(values, 0, false), Some(27.5));
        assert_eq!(effective_handle_in(values, 1, false), Some(45.0));
        // A curve end has no handle beyond it.
        assert_eq!(effective_handle_out(values, 1, false), None);
        assert_eq!(effective_handle_in(values, 0, false), None);

        // With no handle on the neighbour, the default is a third of the way.
        let animation = curve(4.0, false, vec![keyframe(0.0, 10.0), keyframe(2.0, 40.0)]);
        let values = &curve_groups(&animation)[0];
        assert_eq!(effective_handle_out(values, 0, false), Some(20.0));
        assert_eq!(effective_handle_in(values, 1, false), Some(30.0));
    }

    #[test]
    fn cyclic_default_handles_wrap_across_the_seam() {
        let animation = curve(2.0, true, vec![keyframe(0.5, 0.0), keyframe(1.5, 100.0)]);
        let values = &curve_groups(&animation)[0];

        // The first keyframe's incoming handle looks back to the last keyframe.
        let handle_in = effective_handle_in(values, 0, true).unwrap();
        assert!((handle_in - 100.0 / 3.0).abs() < 1e-4);
        let handle_out = effective_handle_out(values, 1, true).unwrap();
        assert!((handle_out - 200.0 / 3.0).abs() < 1e-4);
    }

    #[test]
    fn sample_pose_maps_channels_to_bone_properties() {
        let mut document = Document::new();
        document.bones.push(Bone {
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
                KeyframeTarget::Bone {
                    bone: "Arm".into(),
                    property,
                },
            ));
        }

        let pose = sample_pose(&document, &animation, 0.5);
        let arm = pose.get("Arm");

        assert_eq!(arm.translation, [3.0, -4.0]);
        assert_eq!(arm.rotation, 0.5);
        assert_eq!(arm.scale, 2.0);
        // The pose pivots about the bone's rest head, like the rig page.
        assert_eq!(arm.pivot, [10.0, 20.0]);
    }

    #[test]
    fn setting_a_bone_keyframe_inserts_or_replaces() {
        let mut animation = Animation::new(4.0);

        set_bone_keyframe(&mut animation, "Arm", BoneProperty::Angle, 1.0, 0.5);
        assert_eq!(animation.keyframes.len(), 1);
        assert_eq!(animation.keyframes[0].timestamp, 1.0);
        assert_eq!(animation.keyframes[0].value, 0.5);

        // The same target at the same time updates in place, keeping handles.
        animation.keyframes[0].handle_out = Some(0.9);
        set_bone_keyframe(&mut animation, "Arm", BoneProperty::Angle, 1.0, 0.75);
        assert_eq!(animation.keyframes.len(), 1);
        assert_eq!(animation.keyframes[0].value, 0.75);
        assert_eq!(animation.keyframes[0].handle_out, Some(0.9));

        // A different bone, property, or time is its own keyframe.
        set_bone_keyframe(&mut animation, "Leg", BoneProperty::Angle, 1.0, 0.1);
        set_bone_keyframe(&mut animation, "Arm", BoneProperty::X, 1.0, 2.0);
        set_bone_keyframe(&mut animation, "Arm", BoneProperty::Angle, 2.0, 0.2);
        assert_eq!(animation.keyframes.len(), 4);
    }

    #[test]
    fn setting_a_bone_keyframe_clamps_to_the_duration() {
        let mut animation = Animation::new(2.0);

        set_bone_keyframe(&mut animation, "Arm", BoneProperty::Scale, 9.0, 3.0);

        assert_eq!(animation.keyframes.len(), 1);
        assert_eq!(animation.keyframes[0].timestamp, 2.0);
    }

    #[test]
    fn cyclic_playback_wraps_around_the_end() {
        let (time, playing) = advance_playhead(1.9, 1.0, 0.2, 2.0, true);
        assert!((time - 0.1).abs() < 1e-5);
        assert!(playing);

        // Negative velocity wraps around the start too.
        let (time, playing) = advance_playhead(0.1, -1.0, 0.2, 2.0, true);
        assert!((time - 1.9).abs() < 1e-5);
        assert!(playing);
    }

    #[test]
    fn non_cyclic_playback_stops_at_either_end() {
        assert_eq!(advance_playhead(1.9, 1.0, 0.2, 2.0, false), (2.0, false));
        assert_eq!(advance_playhead(0.1, -1.0, 0.2, 2.0, false), (0.0, false));

        // Before the end the playhead keeps moving.
        let (time, playing) = advance_playhead(1.0, 1.0, 0.2, 2.0, false);
        assert!((time - 1.2).abs() < 1e-5);
        assert!(playing);
    }

    #[test]
    fn playback_without_a_duration_stops_immediately() {
        assert_eq!(advance_playhead(1.0, 1.0, 0.1, 0.0, true), (0.0, false));
        assert_eq!(
            advance_playhead(1.0, 1.0, 0.1, f32::NAN, false),
            (0.0, false)
        );
    }

    #[test]
    fn zero_velocity_holds_the_playhead() {
        let (time, playing) = advance_playhead(0.75, 0.0, 0.1, 2.0, true);
        assert_eq!(time, 0.75);
        assert!(playing);
    }

    #[test]
    fn channels_missing_from_the_animation_stay_at_rest() {
        let mut document = Document::new();
        for (name, head) in [("Arm", [10.0, 20.0]), ("Leg", [20.0, 20.0])] {
            document.bones.push(Bone {
                name: name.into(),
                head: Some(head),
                tail: [20.0, 40.0],
                children: Vec::new(),
            });
        }
        let animation = curve(2.0, false, vec![keyframe(0.0, 5.0)]);

        let pose = sample_pose(&document, &animation, 1.0);

        assert_eq!(pose.get("Arm").translation, [5.0, 0.0]);
        assert!(pose.get("Leg").is_identity());
    }
}
