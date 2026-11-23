//! Document-wide transforms.
//!
//! The menu's transform actions surface small numeric prompts here. The scale
//! prompt multiplies every length in the document—artwork, skeleton, and bone
//! translation curves—about the origin. It deliberately leaves Z order (both a
//! shape's authored Z and its Z animation curves) and the dimensionless
//! rotation and scale curves alone.
//!
//! The prompt is an ordinary Bevy UI overlay rather than a native dialog,
//! because it needs a number field and must stay in sync with the editor's
//! state. While it is open it owns the keyboard: the other input systems check
//! [`TransformDialog::is_open`] and stay out of the way.

use bevy::input::ButtonState;
use bevy::input::keyboard::KeyboardInput;
use bevy::prelude::*;

use crate::bounds::bounds_for;
use crate::editor::{DocumentBounds, EditorState};
use crate::history::History;
use crate::theme;

/// Color of the validation message when the input is not a usable factor.
const ERROR: Color = Color::srgb_u8(0xf0, 0x7a, 0x9c);

/// Font size of the prompt's title.
const TITLE_FONT_SIZE: f32 = 15.0;

/// Font size of the prompt's explanatory text and input.
const BODY_FONT_SIZE: f32 = 12.0;

pub struct TransformPlugin;

impl Plugin for TransformPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<TransformDialog>()
            .add_systems(PreUpdate, clear_closing_frame)
            .add_systems(Startup, spawn_scale_dialog)
            .add_systems(
                Update,
                (
                    handle_scale_keys,
                    handle_scale_buttons,
                    style_dialog_buttons,
                    sync_scale_dialog,
                )
                    .chain(),
            );
    }
}

/// The transform prompt currently open, if any.
#[derive(Resource, Default)]
pub struct TransformDialog {
    scale: Option<ScalePrompt>,
    /// Set for the frame a prompt closes.
    ///
    /// The other input systems are not ordered relative to this one, so one
    /// that runs after the prompt closes would otherwise also act on the same
    /// click or key. Keeping [`Self::blocks_input`] true for one more frame
    /// lets them all ignore it, whoever runs first.
    closing: bool,
}

impl TransformDialog {
    /// Whether the scale prompt is open and visible.
    pub fn is_open(&self) -> bool {
        self.scale.is_some()
    }

    /// Whether the prompt owns input this frame.
    pub fn blocks_input(&self) -> bool {
        self.scale.is_some() || self.closing
    }

    /// Opens the scale prompt with an empty value.
    pub fn open_scale(&mut self) {
        self.scale = Some(ScalePrompt::default());
        self.closing = false;
    }

    /// Closes the prompt, keeping [`Self::blocks_input`] true for this frame.
    fn close(&mut self) {
        self.scale = None;
        self.closing = true;
    }
}

/// Clears the one-frame input block left by a prompt that closed last frame.
fn clear_closing_frame(mut dialog: ResMut<TransformDialog>) {
    dialog.closing = false;
}

/// The state of the scale prompt.
#[derive(Default)]
struct ScalePrompt {
    /// The factor typed so far.
    buffer: String,
    /// Whether the last commit was rejected for not being a positive number.
    invalid: bool,
}

/// The full-screen dimming layer that holds the prompt.
#[derive(Component)]
struct ScaleDialogRoot;

/// The text showing the factor being typed.
#[derive(Component)]
struct ScaleInputText;

/// The hint below the input, which doubles as the validation message.
#[derive(Component)]
struct ScaleHintText;

/// Confirms the prompt.
#[derive(Component)]
struct ScaleConfirmButton;

/// Abandons the prompt.
#[derive(Component)]
struct ScaleCancelButton;

/// Shared hover colors for the prompt's buttons.
#[derive(Component)]
struct DialogButton {
    base: Color,
    hover: Color,
}

fn spawn_scale_dialog(mut commands: Commands) {
    commands
        .spawn((
            ScaleDialogRoot,
            Node {
                position_type: PositionType::Absolute,
                left: Val::Px(0.0),
                top: Val::Px(0.0),
                width: Val::Percent(100.0),
                height: Val::Percent(100.0),
                display: Display::None,
                align_items: AlignItems::Center,
                justify_content: JustifyContent::Center,
                ..default()
            },
            BackgroundColor(Color::srgba(0.0, 0.0, 0.0, 0.45)),
            GlobalZIndex(200),
        ))
        .with_children(|root| {
            root.spawn((
                Node {
                    width: Val::Px(330.0),
                    flex_direction: FlexDirection::Column,
                    row_gap: Val::Px(12.0),
                    padding: UiRect::all(Val::Px(18.0)),
                    border: UiRect::all(Val::Px(1.0)),
                    border_radius: BorderRadius::all(Val::Px(8.0)),
                    ..default()
                },
                BackgroundColor(theme::PANEL_BG),
                BorderColor::all(theme::BORDER),
            ))
            .with_children(|panel| {
                panel.spawn((
                    Text::new("Scale Document"),
                    TextFont::from_font_size(TITLE_FONT_SIZE),
                    TextColor(theme::TEXT),
                ));
                panel.spawn((
                    Text::new(
                        "Multiply every coordinate, bone, and translation by this factor. \
                         Z order is not scaled.",
                    ),
                    TextFont::from_font_size(BODY_FONT_SIZE),
                    TextColor(theme::MUTED),
                ));

                panel
                    .spawn((
                        Node {
                            height: Val::Px(30.0),
                            align_items: AlignItems::Center,
                            padding: UiRect::horizontal(Val::Px(10.0)),
                            border: UiRect::all(Val::Px(1.0)),
                            border_radius: BorderRadius::all(Val::Px(4.0)),
                            ..default()
                        },
                        BackgroundColor(theme::BUTTON_BG),
                        BorderColor::all(theme::BORDER_FAINT),
                    ))
                    .with_children(|field| {
                        field.spawn((
                            Text::new("|"),
                            TextFont::from_font_size(TITLE_FONT_SIZE),
                            TextColor(theme::TEXT),
                            ScaleInputText,
                        ));
                    });

                panel.spawn((
                    Text::new("Enter a positive number."),
                    TextFont::from_font_size(BODY_FONT_SIZE),
                    TextColor(theme::MUTED),
                    ScaleHintText,
                ));

                panel
                    .spawn(Node {
                        flex_direction: FlexDirection::Row,
                        justify_content: JustifyContent::FlexEnd,
                        column_gap: Val::Px(8.0),
                        ..default()
                    })
                    .with_children(|row| {
                        spawn_button(
                            row,
                            "Cancel",
                            theme::BUTTON_BG,
                            theme::ACCENT_FAINT,
                            ScaleCancelButton,
                        );
                        spawn_button(
                            row,
                            "Scale",
                            theme::ACCENT_SELECTED,
                            theme::ACCENT_STRONG,
                            ScaleConfirmButton,
                        );
                    });
            });
        });
}

/// Spawns one prompt button, with `marker` identifying which action it runs.
fn spawn_button(
    row: &mut ChildSpawnerCommands,
    label: &str,
    base: Color,
    hover: Color,
    marker: impl Component,
) {
    row.spawn((
        Button,
        marker,
        DialogButton { base, hover },
        Node {
            height: Val::Px(28.0),
            min_width: Val::Px(72.0),
            align_items: AlignItems::Center,
            justify_content: JustifyContent::Center,
            padding: UiRect::horizontal(Val::Px(12.0)),
            border: UiRect::all(Val::Px(1.0)),
            border_radius: BorderRadius::all(Val::Px(4.0)),
            ..default()
        },
        BackgroundColor(base),
        BorderColor::all(theme::BORDER_FAINT),
    ))
    .with_children(|button| {
        button.spawn((
            Text::new(label),
            TextFont::from_font_size(BODY_FONT_SIZE),
            TextColor(theme::TEXT),
        ));
    });
}

/// Types into, confirms, or cancels the scale prompt.
fn handle_scale_keys(
    mut events: MessageReader<KeyboardInput>,
    mut dialog: ResMut<TransformDialog>,
    mut editor: ResMut<EditorState>,
    mut bounds: ResMut<DocumentBounds>,
    mut history: ResMut<History>,
) {
    let mut commit = false;
    let mut cancel = false;
    {
        let Some(prompt) = dialog.scale.as_mut() else {
            for _ in events.read() {}
            return;
        };
        for event in events.read() {
            if event.state != ButtonState::Pressed {
                continue;
            }
            match event.key_code {
                KeyCode::Enter => commit = true,
                KeyCode::Escape => cancel = true,
                KeyCode::Backspace => {
                    prompt.buffer.pop();
                    prompt.invalid = false;
                }
                _ => {
                    if let Some(text) = &event.text {
                        for ch in text.chars() {
                            if ch.is_ascii_digit() || matches!(ch, '.' | '-' | '+') {
                                prompt.buffer.push(ch);
                                prompt.invalid = false;
                            }
                        }
                    }
                }
            }
        }
    }

    if cancel {
        dialog.close();
    } else if commit {
        commit_scale(&mut dialog, &mut editor, &mut bounds, &mut history);
    }
}

/// Runs the scale prompt's Cancel and Scale buttons.
fn handle_scale_buttons(
    confirm: Query<&Interaction, (Changed<Interaction>, With<ScaleConfirmButton>)>,
    cancel: Query<&Interaction, (Changed<Interaction>, With<ScaleCancelButton>)>,
    mut dialog: ResMut<TransformDialog>,
    mut editor: ResMut<EditorState>,
    mut bounds: ResMut<DocumentBounds>,
    mut history: ResMut<History>,
) {
    if cancel
        .iter()
        .any(|interaction| *interaction == Interaction::Pressed)
    {
        dialog.close();
        return;
    }
    if confirm
        .iter()
        .any(|interaction| *interaction == Interaction::Pressed)
    {
        commit_scale(&mut dialog, &mut editor, &mut bounds, &mut history);
    }
}

/// Parses and applies the factor being edited, leaving the prompt open when it
/// is not a positive finite number.
fn commit_scale(
    dialog: &mut TransformDialog,
    editor: &mut EditorState,
    bounds: &mut DocumentBounds,
    history: &mut History,
) {
    let Some(prompt) = dialog.scale.as_mut() else {
        return;
    };
    let factor = prompt.buffer.trim().parse::<f32>();
    let Ok(factor) = factor else {
        prompt.invalid = true;
        return;
    };
    if !factor.is_finite() || factor <= 0.0 {
        prompt.invalid = true;
        return;
    }

    dialog.close();
    apply_scale(editor, bounds, history, factor);
}

/// Scales the document, the transient rig pose, and the framing bounds.
fn apply_scale(
    editor: &mut EditorState,
    bounds: &mut DocumentBounds,
    history: &mut History,
    factor: f32,
) {
    editor.document.scale_uniform(factor);
    scale_pose(&mut editor.pose, editor.document.bone_count(), factor);
    // The document's extent changed, so the view must fit the new geometry.
    *bounds = DocumentBounds(bounds_for(&editor.document));
    history.observe(&editor.document, false);
}

/// Scales the transient pose's rest-space anchors and offsets.
///
/// The pose is not part of the saved file, but its pivots and translations are
/// asset-space values. Scaling them keeps a posed rig looking uniformly scaled
/// instead of deforming around the old coordinates.
fn scale_pose(pose: &mut viveco::Pose, bone_count: usize, factor: f32) {
    for index in 0..bone_count {
        let mut bone = pose.get(index);
        bone.pivot = [bone.pivot[0] * factor, bone.pivot[1] * factor];
        bone.translation = [bone.translation[0] * factor, bone.translation[1] * factor];
        pose.set(index, bone);
    }
}

/// Shows the prompt while it is open and mirrors its text into the field.
#[allow(clippy::type_complexity)]
fn sync_scale_dialog(
    dialog: Res<TransformDialog>,
    mut roots: Query<&mut Node, With<ScaleDialogRoot>>,
    mut input: Query<&mut Text, With<ScaleInputText>>,
    mut hints: Query<(&mut Text, &mut TextColor), (With<ScaleHintText>, Without<ScaleInputText>)>,
) {
    let display = if dialog.is_open() {
        Display::Flex
    } else {
        Display::None
    };
    for mut node in &mut roots {
        if node.display != display {
            node.display = display;
        }
    }

    let Some(prompt) = &dialog.scale else {
        return;
    };

    let label = format!("{}|", prompt.buffer);
    for mut text in &mut input {
        if text.0 != label {
            text.0 = label.clone();
        }
    }

    let (hint, color) = if prompt.invalid {
        ("Enter a positive number.", ERROR)
    } else {
        (
            "Enter a positive number. Z order is not scaled.",
            theme::MUTED,
        )
    };
    for (mut text, mut text_color) in &mut hints {
        if text.0 != hint {
            text.0 = hint.to_owned();
        }
        if text_color.0 != color {
            text_color.0 = color;
        }
    }
}

/// Highlights the prompt's buttons on hover and press.
fn style_dialog_buttons(mut buttons: Query<(&DialogButton, &Interaction, &mut BackgroundColor)>) {
    for (button, interaction, mut background) in &mut buttons {
        let color = match interaction {
            Interaction::Pressed => theme::ACCENT_SELECTED,
            Interaction::Hovered => button.hover,
            Interaction::None => button.base,
        };
        if background.0 != color {
            background.0 = color;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bounds::EMPTY_BOUNDS;
    use viveco::{Animation, Asset, Bone, BoneProperty, Keyframe, KeyframeTarget, Path, Shape};

    fn document() -> Asset {
        let mut animation = Animation::new(1.0);
        animation.insert_keyframe(Keyframe::new(
            0.0,
            4.0,
            KeyframeTarget::Bone {
                bone: 0,
                property: BoneProperty::X,
            },
        ));
        animation.insert_keyframe(Keyframe::new(
            0.5,
            0.25,
            KeyframeTarget::Bone {
                bone: 0,
                property: BoneProperty::Angle,
            },
        ));
        Asset {
            shapes: vec![Shape {
                z: 3.0,
                paths: vec![Path::new([1.0, 2.0]).line_to([3.0, 4.0])],
                ..Default::default()
            }],
            bones: vec![Bone {
                head: Some([5.0, 6.0]),
                tail: [7.0, 8.0],
                ..Default::default()
            }],
            animations: vec![animation],
        }
    }

    #[test]
    fn scaling_updates_geometry_bones_and_translation_curves() {
        let mut editor = EditorState::new(document());
        let mut bounds = DocumentBounds(EMPTY_BOUNDS);
        let mut history = History::new(editor.document.clone());

        apply_scale(&mut editor, &mut bounds, &mut history, 3.0);

        assert_eq!(editor.document.shapes[0].paths[0].start(), Some([3.0, 6.0]));
        assert_eq!(editor.document.shapes[0].z, 3.0);
        assert_eq!(editor.document.bones[0].head, Some([15.0, 18.0]));
        assert_eq!(editor.document.bones[0].tail, [21.0, 24.0]);
        assert_eq!(editor.document.animations[0].keyframes[0].value, 12.0);
        assert_eq!(editor.document.animations[0].keyframes[1].value, 0.25);
        assert_eq!(bounds.0.min, [3.0, 6.0]);
        assert_eq!(bounds.0.max, [9.0, 12.0]);

        // The whole change is one undo step.
        assert!(history.undo(&mut editor));
        assert_eq!(editor.document, document());
    }

    #[test]
    fn scaling_the_pose_keeps_the_deformation_proportional() {
        let mut pose = viveco::Pose::new(1);
        pose.set(
            0,
            viveco::BonePose {
                translation: [2.0, -4.0],
                pivot: [5.0, 6.0],
                scale: 1.5,
                rotation: 0.5,
            },
        );

        scale_pose(&mut pose, 1, 2.0);

        let bone = pose.get(0);
        assert_eq!(bone.translation, [4.0, -8.0]);
        assert_eq!(bone.pivot, [10.0, 12.0]);
        // The pose's rotation and scale are dimensionless.
        assert_eq!(bone.scale, 1.5);
        assert_eq!(bone.rotation, 0.5);
    }

    #[test]
    fn a_non_positive_or_unparseable_factor_is_rejected() {
        for text in ["", "0", "-2", "nope", "inf", "NaN"] {
            let mut dialog = TransformDialog {
                scale: Some(ScalePrompt {
                    buffer: text.to_owned(),
                    invalid: false,
                }),
                ..Default::default()
            };
            let mut editor = EditorState::new(document());
            let mut bounds = DocumentBounds(EMPTY_BOUNDS);
            let mut history = History::new(editor.document.clone());

            commit_scale(&mut dialog, &mut editor, &mut bounds, &mut history);

            assert_eq!(
                editor.document,
                document(),
                "{text:?} must not change the document"
            );
            assert!(dialog.is_open(), "{text:?} must leave the prompt open");
            assert!(dialog.scale.as_ref().is_some_and(|prompt| prompt.invalid));
        }
    }

    #[test]
    fn a_valid_factor_closes_the_prompt_and_applies() {
        let mut dialog = TransformDialog {
            scale: Some(ScalePrompt {
                buffer: " 2.5 ".to_owned(),
                invalid: false,
            }),
            ..Default::default()
        };
        let mut editor = EditorState::new(document());
        let mut bounds = DocumentBounds(EMPTY_BOUNDS);
        let mut history = History::new(editor.document.clone());

        commit_scale(&mut dialog, &mut editor, &mut bounds, &mut history);

        assert!(!dialog.is_open());
        // The key or click that closed the prompt must not also reach the
        // editor this frame.
        assert!(dialog.blocks_input());
        assert_eq!(editor.document.shapes[0].paths[0].start(), Some([2.5, 5.0]));
        assert_eq!(editor.document.animations[0].keyframes[0].value, 10.0);
    }
}
