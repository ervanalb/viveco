//! Animation mode: selecting, creating, and deleting the document's animations.
//!
//! Animations are anonymous, so the editor addresses them by their index in
//! [`viveco::Asset::animations`]. Creating and deleting therefore
//! tracks the selection as indices shift.
//!
//! Everything else about an animation (its keyframes and how they are
//! sampled) is document data handled by the `viveco` crate; this module only
//! edits the properties exposed by the animate panel.

use crate::editor::{AnimationRename, EditorState, selected_shape_index};
use viveco::{Animation, ShapeProperty};

// Effective Bézier handles are free helpers over a keyframe slice, so the
// editor re-exports them for the timeline that draws them.
pub use viveco::{effective_handle_in, effective_handle_out};

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

/// Adds or updates a Z keyframe for the selected shape at the playhead.
///
/// The keyed value is the Z offset the selected animation's curve already has
/// there, so keying without changing the curve never moves the shape. The
/// curve's values are added to the shape's authored Z when it is painted.
/// Returns `false` when no shape or animation is selected.
pub fn add_shape_z_keyframe(editor: &mut EditorState) -> bool {
    let Some(animation_index) = editor.selected_animation else {
        return false;
    };
    let Some(shape) = selected_shape_index(editor) else {
        return false;
    };
    let time = editor.playhead;
    let value = editor.document.animations[animation_index]
        .sample_shape_pose(&editor.document, time)
        .get(shape);
    let Some(animation) = editor.document.animations.get_mut(animation_index) else {
        return false;
    };
    animation.set_shape_keyframe(shape, ShapeProperty::Z, time, value);
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use viveco::{Asset, BoneProperty, Keyframe, KeyframeTarget};

    fn editor_with_keyframes() -> EditorState {
        let mut editor = EditorState::new(Asset::new());
        let index = add_animation(&mut editor);
        let animation = &mut editor.document.animations[index];
        animation.duration = 10.0;
        for timestamp in [1.0, 9.0] {
            animation.insert_keyframe(Keyframe::new(
                timestamp,
                0.0,
                KeyframeTarget::Bone {
                    bone: 0,
                    property: BoneProperty::X,
                },
            ));
        }
        editor
    }

    #[test]
    fn adding_an_animation_selects_it() {
        let mut editor = EditorState::new(Asset::new());

        let first = add_animation(&mut editor);
        let second = add_animation(&mut editor);

        assert_eq!((first, second), (0, 1));
        assert_eq!(editor.selected_animation, Some(1));
        assert_eq!(editor.document.animations.len(), 2);
        assert_eq!(editor.structure_revision, 2);
    }

    #[test]
    fn deleting_keeps_the_selection_on_a_neighbour() {
        let mut editor = EditorState::new(Asset::new());
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
        let mut editor = EditorState::new(Asset::new());
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
        let mut editor = EditorState::new(Asset::new());
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
        let mut editor = EditorState::new(Asset::new());
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

    #[test]
    fn adding_a_shape_z_keyframe_uses_the_current_offset() {
        use crate::editor::TreeAddress;
        use viveco::{Shape, ShapeProperty};

        let mut editor = EditorState::new(Asset {
            shapes: vec![Shape::default(), Shape::default()],
            ..Default::default()
        });
        add_animation(&mut editor);
        editor.playhead = 1.0;
        editor.selected_node = Some(TreeAddress::Shape { shapes: vec![1] });

        // With no curve yet, keying captures a zero offset.
        assert!(add_shape_z_keyframe(&mut editor));
        let animation = &editor.document.animations[0];
        assert_eq!(animation.keyframes.len(), 1);
        assert_eq!(
            animation.keyframes[0].target,
            KeyframeTarget::Shape {
                shape: 1,
                property: ShapeProperty::Z,
            }
        );
        assert_eq!(animation.keyframes[0].value, 0.0);

        // An existing curve keeps its value when keyed again.
        editor.document.animations[0].keyframes[0].value = 3.0;
        assert!(add_shape_z_keyframe(&mut editor));
        assert_eq!(editor.document.animations[0].keyframes.len(), 1);
        assert_eq!(editor.document.animations[0].keyframes[0].value, 3.0);

        // Without a selected shape there is nothing to key.
        editor.selected_node = None;
        assert!(!add_shape_z_keyframe(&mut editor));
    }
}
