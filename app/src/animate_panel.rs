//! Bottom panel for animate mode.
//!
//! Shows the selected animation's duration and cyclic properties. The duration
//! is edited as text: click the value to type a new one, then press Enter to
//! commit or Escape to cancel. A new duration may delete keyframes that fall
//! outside it.
//!
//! The rest of the panel is the animation timeline, drawn by
//! [`crate::timeline`].

use bevy::input::ButtonState;
use bevy::input::keyboard::KeyboardInput;
use bevy::prelude::*;

use crate::animate::{commit_animation_rename, selected_animation, set_cyclic, set_duration};
use crate::editor::{BOTTOM_BAR_HEIGHT, EditorMode, EditorState, SIDEBAR_WIDTH, TOOLBAR_WIDTH};
use crate::theme;
use crate::timeline::spawn_timeline;

pub struct AnimatePanelPlugin;

impl Plugin for AnimatePanelPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<DurationEdit>()
            .add_systems(Startup, spawn_panel)
            .add_systems(
                Update,
                (
                    sync_visibility,
                    handle_field_press,
                    handle_duration_input,
                    handle_cyclic_toggle,
                    sync_contents,
                    paint_buttons,
                )
                    .chain(),
            );
    }
}

/// The root of the bottom animate panel.
#[derive(Component)]
struct AnimatePanelRoot;

/// A part of the panel whose visibility depends on whether an animation is
/// selected.
#[derive(Component, Clone, Copy)]
enum PanelContents {
    /// The duration and cyclic controls, shown when an animation is selected.
    Controls,
    /// The hint shown when no animation is selected.
    EmptyHint,
}

/// The selectable buttons in the controls row.
#[derive(Component, Clone, Copy, PartialEq, Eq)]
enum PanelButton {
    /// The duration text field.
    DurationField,
    /// The cyclic checkbox.
    CyclicToggle,
}

/// The text showing the duration value.
#[derive(Component)]
struct DurationText;

/// The checkbox square inside the cyclic toggle.
#[derive(Component)]
struct CyclicCheck;

/// The text edit in progress, if any.
#[derive(Resource, Default)]
pub struct DurationEdit {
    /// The index of the animation being edited.
    animation: Option<usize>,
    /// The text typed so far.
    buffer: Option<String>,
}

impl DurationEdit {
    /// Whether the field at `index` is being edited.
    fn editing(&self, index: usize) -> bool {
        self.animation == Some(index)
    }

    /// Whether the duration field currently owns the keyboard.
    ///
    /// Other shortcuts consult this so they do not steal keys typed into the
    /// field.
    pub fn is_editing(&self) -> bool {
        self.animation.is_some()
    }

    /// Abandons the edit in progress, if any.
    pub fn cancel(&mut self) {
        self.animation = None;
        self.buffer = None;
    }
}

fn spawn_panel(mut commands: Commands) {
    commands
        .spawn((
            Node {
                position_type: PositionType::Absolute,
                left: Val::Px(TOOLBAR_WIDTH),
                right: Val::Px(SIDEBAR_WIDTH),
                bottom: Val::Px(0.0),
                height: Val::Px(BOTTOM_BAR_HEIGHT),
                flex_direction: FlexDirection::Column,
                display: Display::None,
                ..default()
            },
            AnimatePanelRoot,
        ))
        .with_children(|panel| {
            // The controls band paints the panel background itself. The
            // timeline node below must stay free of Bevy UI backgrounds: Vello
            // UI scenes are composited beneath Bevy UI, so an opaque
            // background would hide the timeline.
            panel
                .spawn((
                    Node {
                        width: Val::Percent(100.0),
                        flex_direction: FlexDirection::Column,
                        row_gap: Val::Px(10.0),
                        padding: UiRect {
                            left: Val::Px(12.0),
                            right: Val::Px(12.0),
                            top: Val::Px(12.0),
                            bottom: Val::Px(10.0),
                        },
                        flex_shrink: 0.0,
                        ..default()
                    },
                    BackgroundColor(theme::PANEL_BG),
                ))
                .with_children(|band| {
                    band.spawn((
                        Node {
                            flex_direction: FlexDirection::Row,
                            align_items: AlignItems::Center,
                            column_gap: Val::Px(28.0),
                            display: Display::None,
                            ..default()
                        },
                        PanelContents::Controls,
                    ))
                    .with_children(spawn_duration_and_cyclic);

                    band.spawn((
                        Text::new("Select an animation to edit its properties."),
                        TextFont::from_font_size(12.0),
                        TextColor(theme::MUTED),
                        PanelContents::EmptyHint,
                    ));
                });

            spawn_timeline(panel);
        });
}

fn spawn_duration_and_cyclic(controls: &mut ChildSpawnerCommands) {
    // Duration.
    controls
        .spawn(Node {
            flex_direction: FlexDirection::Row,
            align_items: AlignItems::Center,
            column_gap: Val::Px(8.0),
            ..default()
        })
        .with_children(|group| {
            group.spawn((
                Text::new("Duration"),
                TextFont::from_font_size(12.0),
                TextColor(theme::MUTED),
            ));
            group
                .spawn((
                    Button,
                    PanelButton::DurationField,
                    Node {
                        min_width: Val::Px(76.0),
                        padding: UiRect::axes(Val::Px(10.0), Val::Px(5.0)),
                        align_items: AlignItems::Center,
                        justify_content: JustifyContent::Center,
                        border: UiRect::all(Val::Px(1.0)),
                        border_radius: BorderRadius::all(Val::Px(6.0)),
                        ..default()
                    },
                    BackgroundColor(theme::BUTTON_BG),
                    BorderColor::all(theme::BORDER_FAINT),
                ))
                .with_children(|field| {
                    field.spawn((
                        Text::new("0.00"),
                        TextFont::from_font_size(12.0),
                        TextColor(theme::TEXT),
                        DurationText,
                    ));
                });
            group.spawn((
                Text::new("s"),
                TextFont::from_font_size(12.0),
                TextColor(theme::MUTED),
            ));
        });

    // Cyclic.
    controls
        .spawn((
            Button,
            PanelButton::CyclicToggle,
            Node {
                padding: UiRect::axes(Val::Px(10.0), Val::Px(5.0)),
                align_items: AlignItems::Center,
                border: UiRect::all(Val::Px(1.0)),
                border_radius: BorderRadius::all(Val::Px(6.0)),
                ..default()
            },
            BackgroundColor(theme::BUTTON_BG),
            BorderColor::all(theme::BORDER_FAINT),
        ))
        .with_children(|toggle| {
            toggle
                .spawn(Node {
                    flex_direction: FlexDirection::Row,
                    align_items: AlignItems::Center,
                    column_gap: Val::Px(6.0),
                    ..default()
                })
                .with_children(|content| {
                    content.spawn((
                        Node {
                            width: Val::Px(14.0),
                            height: Val::Px(14.0),
                            border: UiRect::all(Val::Px(1.0)),
                            border_radius: BorderRadius::all(Val::Px(3.0)),
                            ..default()
                        },
                        BackgroundColor(Color::NONE),
                        BorderColor::all(theme::BORDER),
                        CyclicCheck,
                    ));
                    content.spawn((
                        Text::new("Cyclic"),
                        TextFont::from_font_size(12.0),
                        TextColor(theme::TEXT),
                    ));
                });
        });
}

/// Shows the panel only in animate mode, cancelling any edit in progress when
/// leaving it.
fn sync_visibility(
    mode: Res<EditorMode>,
    mut panels: Query<&mut Node, With<AnimatePanelRoot>>,
    mut edit: ResMut<DurationEdit>,
) {
    let display = if *mode == EditorMode::Animate {
        Display::Flex
    } else {
        Display::None
    };
    if *mode != EditorMode::Animate {
        edit.cancel();
    }
    for mut node in &mut panels {
        if node.display != display {
            node.display = display;
        }
    }
}

/// Starts editing the duration when its field is clicked, or commits an edit
/// already in progress.
fn handle_field_press(
    buttons: Query<(&Interaction, &PanelButton), Changed<Interaction>>,
    mut editor: ResMut<EditorState>,
    mut edit: ResMut<DurationEdit>,
) {
    for (interaction, button) in &buttons {
        if *interaction != Interaction::Pressed || *button != PanelButton::DurationField {
            continue;
        }
        // The field takes the keyboard from any inline rename in progress.
        commit_animation_rename(&mut editor);
        let Some(index) = editor.selected_animation else {
            return;
        };
        if edit.editing(index) {
            commit_duration(&mut editor, &mut edit);
        } else {
            let Some(animation) = selected_animation(&editor) else {
                return;
            };
            edit.buffer = Some(format!("{:.2}", animation.duration));
            edit.animation = Some(index);
        }
        return;
    }
}

/// Types into, commits, or cancels the duration field.
fn handle_duration_input(
    mode: Res<EditorMode>,
    mut events: MessageReader<KeyboardInput>,
    mut editor: ResMut<EditorState>,
    mut edit: ResMut<DurationEdit>,
) {
    if *mode != EditorMode::Animate || edit.animation.is_none() {
        for _ in events.read() {}
        return;
    }

    let Some(index) = edit.animation else {
        return;
    };
    // A selection change (undo, deletion) abandons the edit.
    if editor.selected_animation != Some(index) {
        edit.cancel();
        for _ in events.read() {}
        return;
    }

    let mut commit = false;
    let mut cancel = false;
    if let Some(buffer) = &mut edit.buffer {
        for event in events.read() {
            if event.state != ButtonState::Pressed {
                continue;
            }
            match event.key_code {
                KeyCode::Enter => commit = true,
                KeyCode::Escape => cancel = true,
                KeyCode::Backspace => {
                    buffer.pop();
                }
                _ => {
                    if let Some(text) = &event.text {
                        for ch in text.chars() {
                            if ch.is_ascii_digit() || matches!(ch, '.' | '-' | '+') {
                                buffer.push(ch);
                            }
                        }
                    }
                }
            }
        }
    }

    if cancel {
        edit.cancel();
    } else if commit {
        commit_duration(&mut editor, &mut edit);
    }
}

/// Parses and applies the text being edited, ignoring invalid input.
fn commit_duration(editor: &mut EditorState, edit: &mut DurationEdit) {
    let Some(index) = edit.animation else {
        return;
    };
    let Some(text) = edit.buffer.as_deref() else {
        edit.cancel();
        return;
    };
    if let Ok(value) = text.trim().parse::<f32>()
        && value.is_finite()
    {
        set_duration(editor, index, value);
    }
    edit.cancel();
}

/// Toggles the selected animation's cyclic flag.
fn handle_cyclic_toggle(
    buttons: Query<(&Interaction, &PanelButton), Changed<Interaction>>,
    mut editor: ResMut<EditorState>,
    mut edit: ResMut<DurationEdit>,
) {
    for (interaction, button) in &buttons {
        if *interaction != Interaction::Pressed || *button != PanelButton::CyclicToggle {
            continue;
        }
        let Some(index) = editor.selected_animation else {
            return;
        };
        // Switching property commits any duration edit first.
        if edit.editing(index) {
            commit_duration(&mut editor, &mut edit);
        }
        let Some(animation) = editor.document.animations.get(index) else {
            return;
        };
        let cyclic = !animation.cyclic;
        set_cyclic(&mut editor, index, cyclic);
        return;
    }
}

/// Shows the controls or the empty hint, and mirrors the selected animation's
/// values into the widgets.
fn sync_contents(
    mode: Res<EditorMode>,
    editor: Res<EditorState>,
    edit: Res<DurationEdit>,
    mut contents: Query<(&mut Node, &PanelContents)>,
    mut texts: Query<&mut Text, With<DurationText>>,
    mut checks: Query<&mut BackgroundColor, With<CyclicCheck>>,
) {
    if *mode != EditorMode::Animate {
        return;
    }
    let selected = selected_animation(&editor).zip(editor.selected_animation);

    for (mut node, contents) in &mut contents {
        let display = match (selected.is_some(), contents) {
            (true, PanelContents::Controls) | (false, PanelContents::EmptyHint) => Display::Flex,
            _ => Display::None,
        };
        if node.display != display {
            node.display = display;
        }
    }

    let Some((animation, index)) = selected else {
        return;
    };

    let label = if edit.editing(index) {
        edit.buffer
            .as_deref()
            .map(|buffer| format!("{buffer}|"))
            .unwrap_or_default()
    } else {
        format!("{:.2}", animation.duration)
    };
    for mut text in &mut texts {
        if text.0 != label {
            text.0 = label.clone();
        }
    }

    let check = if animation.cyclic {
        theme::ACCENT
    } else {
        Color::NONE
    };
    for mut background in &mut checks {
        if background.0 != check {
            background.0 = check;
        }
    }
}

/// Gives the field and toggle hover/active feedback.
fn paint_buttons(
    edit: Res<DurationEdit>,
    editor: Res<EditorState>,
    mut buttons: Query<(&PanelButton, &Interaction, &mut BackgroundColor)>,
) {
    for (button, interaction, mut background) in &mut buttons {
        let active = match button {
            PanelButton::DurationField => editor
                .selected_animation
                .is_some_and(|index| edit.editing(index)),
            PanelButton::CyclicToggle => false,
        };
        let color = if active {
            theme::ACCENT_SELECTED
        } else if *interaction != Interaction::None {
            theme::ACCENT_FAINT
        } else {
            theme::BUTTON_BG
        };
        if background.0 != color {
            background.0 = color;
        }
    }
}
