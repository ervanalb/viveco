//! Right-hand animation list for animate mode.
//!
//! Lists the document's animations in order. A row selects an animation;
//! double-clicking a row renames it inline, and `Delete` removes the selected
//! animation. The header's `+` creates a new one. The object and skeleton
//! trees are hidden while this list is shown.

use bevy::input::ButtonState;
use bevy::input::keyboard::KeyboardInput;
use bevy::prelude::*;
use bevy::ui::RelativeCursorPosition;
use bevy::window::PrimaryWindow;

use crate::animate::{
    add_animation, animation_label, begin_animation_rename, cancel_animation_rename,
    commit_animation_rename, delete_selected_animation,
};
use crate::animate_panel::DurationEdit;
use crate::editor::{CONTENT_TOP, EditorMode, EditorState, SIDEBAR_WIDTH};
use crate::theme;

/// Longest gap between two presses that still counts as a double-click, in
/// seconds.
const DOUBLE_CLICK_SECONDS: f64 = 0.35;

/// Largest cursor travel between the two presses of a double-click, in pixels.
const DOUBLE_CLICK_DISTANCE: f32 = 6.0;

pub struct AnimateSidebarPlugin;

impl Plugin for AnimateSidebarPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, spawn_sidebar).add_systems(
            Update,
            (
                handle_row_press,
                handle_rename_input,
                handle_delete_key,
                handle_add,
                commit_on_empty_press,
                rebuild_list,
                sync_labels,
                paint_rows,
                sync_visibility,
            )
                .chain(),
        );
    }
}

/// The root of the animate-mode sidebar.
#[derive(Component)]
struct AnimateSidebarRoot;

/// The column holding the animation rows.
#[derive(Component)]
struct AnimationList;

/// Creates a new animation.
#[derive(Component)]
struct AddAnimationButton;

/// One selectable animation row, addressed by index.
#[derive(Component, Clone, Copy)]
struct AnimationRow(usize);

/// The text of an animation row, kept in step with the name and rename buffer.
#[derive(Component, Clone, Copy)]
struct AnimationRowLabel(usize);

fn spawn_sidebar(mut commands: Commands) {
    commands
        .spawn((
            Node {
                position_type: PositionType::Absolute,
                top: Val::Px(CONTENT_TOP),
                bottom: Val::Px(0.0),
                right: Val::Px(0.0),
                width: Val::Px(SIDEBAR_WIDTH),
                flex_direction: FlexDirection::Column,
                padding: UiRect::all(Val::Px(12.0)),
                row_gap: Val::Px(8.0),
                display: Display::None,
                ..default()
            },
            BackgroundColor(theme::PANEL_BG),
            AnimateSidebarRoot,
        ))
        .with_children(|panel| {
            panel
                .spawn(Node {
                    flex_direction: FlexDirection::Row,
                    align_items: AlignItems::Center,
                    column_gap: Val::Px(6.0),
                    ..default()
                })
                .with_children(|header| {
                    header.spawn((
                        Text::new("Animations"),
                        TextFont::from_font_size(16.0),
                        TextColor(theme::TEXT),
                        Node {
                            flex_grow: 1.0,
                            ..default()
                        },
                    ));
                    spawn_header_button(header, "+", AddAnimationButton);
                });

            panel.spawn((
                Node {
                    flex_direction: FlexDirection::Column,
                    width: Val::Percent(100.0),
                    flex_grow: 1.0,
                    overflow: Overflow::clip_y(),
                    row_gap: Val::Px(2.0),
                    ..default()
                },
                AnimationList,
            ));
        });
}

fn spawn_header_button(parent: &mut ChildSpawnerCommands, label: &str, marker: impl Component) {
    parent
        .spawn((
            Button,
            Node {
                min_width: Val::Px(26.0),
                height: Val::Px(24.0),
                padding: UiRect::horizontal(Val::Px(8.0)),
                align_items: AlignItems::Center,
                justify_content: JustifyContent::Center,
                border: UiRect::all(Val::Px(1.0)),
                border_radius: BorderRadius::all(Val::Px(6.0)),
                ..default()
            },
            BackgroundColor(theme::BUTTON_BG),
            BorderColor::all(theme::BORDER_FAINT),
            marker,
        ))
        .with_children(|button| {
            button.spawn((
                Text::new(label),
                TextFont::from_font_size(12.0),
                TextColor(theme::TEXT),
            ));
        });
}

/// Selects a row on press, or begins renaming it on the second press of a
/// double-click.
#[allow(clippy::too_many_arguments)]
fn handle_row_press(
    mode: Res<EditorMode>,
    mouse: Res<ButtonInput<MouseButton>>,
    time: Res<Time>,
    windows: Query<&Window, With<PrimaryWindow>>,
    rows: Query<(&Interaction, &AnimationRow), Changed<Interaction>>,
    mut editor: ResMut<EditorState>,
    mut duration_edit: ResMut<DurationEdit>,
    mut last_click: Local<Option<(f64, Vec2, usize)>>,
) {
    if *mode != EditorMode::Animate || !mouse.just_pressed(MouseButton::Left) {
        return;
    }
    let Ok(window) = windows.single() else {
        return;
    };
    let Some(cursor) = window.cursor_position() else {
        return;
    };

    for (interaction, row) in &rows {
        if *interaction != Interaction::Pressed {
            continue;
        }
        let index = row.0;
        let now = time.elapsed_secs_f64();
        let double_click = last_click.as_ref().is_some_and(|(t, p, i)| {
            now - *t <= DOUBLE_CLICK_SECONDS
                && cursor.distance(*p) <= DOUBLE_CLICK_DISTANCE
                && *i == index
        });

        // A rename in progress belongs to another row; commit it before acting
        // on this one so the edit is never silently lost.
        if editor
            .animation_renaming
            .as_ref()
            .is_some_and(|rename| rename.index != index)
        {
            commit_animation_rename(&mut editor);
        }

        if double_click {
            *last_click = None;
            // The text field and the rename cannot both own the keyboard.
            duration_edit.cancel();
            begin_animation_rename(&mut editor, index);
            return;
        }

        *last_click = Some((now, cursor, index));
        editor.selected_animation = Some(index);
        return;
    }
}

/// Types into, commits, or cancels an animation rename.
fn handle_rename_input(
    mode: Res<EditorMode>,
    mut events: MessageReader<KeyboardInput>,
    mut editor: ResMut<EditorState>,
) {
    if *mode != EditorMode::Animate || editor.animation_renaming.is_none() {
        for _ in events.read() {}
        return;
    }

    let mut commit = false;
    let mut cancel = false;
    {
        let Some(rename) = editor.animation_renaming.as_mut() else {
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
                    rename.buffer.pop();
                }
                _ => {
                    if let Some(text) = &event.text {
                        for ch in text.chars() {
                            if !ch.is_control() {
                                rename.buffer.push(ch);
                            }
                        }
                    }
                }
            }
        }
    }

    if cancel {
        cancel_animation_rename(&mut editor);
    } else if commit {
        commit_animation_rename(&mut editor);
    }
}

/// Removes the selected animation when `Delete` is pressed.
///
/// Nothing is deleted while a name or the duration field is being edited, so
/// the key belongs to whichever text field has the keyboard.
fn handle_delete_key(
    mode: Res<EditorMode>,
    keys: Res<ButtonInput<KeyCode>>,
    mut editor: ResMut<EditorState>,
    duration_edit: Res<DurationEdit>,
) {
    if *mode != EditorMode::Animate || !keys.just_pressed(KeyCode::Delete) {
        return;
    }
    if editor.is_renaming() || duration_edit.is_editing() {
        return;
    }
    delete_selected_animation(&mut editor);
}

/// Appends a new animation from the header button.
fn handle_add(
    mode: Res<EditorMode>,
    buttons: Query<&Interaction, (Changed<Interaction>, With<AddAnimationButton>)>,
    mut editor: ResMut<EditorState>,
    mut duration_edit: ResMut<DurationEdit>,
) {
    if *mode != EditorMode::Animate {
        return;
    }
    if !buttons.iter().any(|i| *i == Interaction::Pressed) {
        return;
    }
    commit_animation_rename(&mut editor);
    duration_edit.cancel();
    add_animation(&mut editor);
}

/// Commits an in-progress rename when the sidebar is pressed away from its
/// rows, so clicking elsewhere in the panel does not leave the edit dangling.
fn commit_on_empty_press(
    mode: Res<EditorMode>,
    mouse: Res<ButtonInput<MouseButton>>,
    windows: Query<&Window, With<PrimaryWindow>>,
    rows: Query<&RelativeCursorPosition, With<AnimationRow>>,
    mut editor: ResMut<EditorState>,
) {
    if *mode != EditorMode::Animate
        || !mouse.just_pressed(MouseButton::Left)
        || editor.animation_renaming.is_none()
    {
        return;
    }
    let Ok(window) = windows.single() else {
        return;
    };
    let Some(cursor) = window.cursor_position() else {
        return;
    };

    // Only the sidebar strip: toolbar, tab bar, canvas, and bottom panel are
    // elsewhere.
    if cursor.x < window.width() - SIDEBAR_WIDTH || cursor.y < CONTENT_TOP {
        return;
    }
    // A row under the cursor commits or starts its own rename.
    if rows.iter().any(|row| row.cursor_over()) {
        return;
    }

    commit_animation_rename(&mut editor);
}

/// Rebuilds the row list when the document's animations or the selection
/// change.
fn rebuilt_signature(editor: &EditorState) -> (u64, Option<usize>, usize) {
    (
        editor.structure_revision,
        editor.selected_animation,
        editor.document.animations.len(),
    )
}

fn rebuild_list(
    mut commands: Commands,
    editor: Res<EditorState>,
    list: Query<Entity, With<AnimationList>>,
    mut last: Local<Option<(u64, Option<usize>, usize)>>,
) {
    let signature = rebuilt_signature(&editor);
    if last.as_ref() == Some(&signature) {
        return;
    }
    *last = Some(signature);

    let Ok(list) = list.single() else {
        return;
    };
    commands.entity(list).despawn_children();
    commands.entity(list).with_children(|parent| {
        if editor.document.animations.is_empty() {
            parent.spawn((
                Text::new("No animations"),
                TextFont::from_font_size(12.0),
                TextColor(theme::MUTED),
            ));
            return;
        }
        for (index, animation) in editor.document.animations.iter().enumerate() {
            spawn_row(
                parent,
                index,
                &animation_label(animation, index),
                editor.selected_animation == Some(index),
            );
        }
    });
}

fn spawn_row(parent: &mut ChildSpawnerCommands, index: usize, label: &str, selected: bool) {
    parent
        .spawn((
            Button,
            Node {
                width: Val::Percent(100.0),
                padding: UiRect::axes(Val::Px(8.0), Val::Px(4.0)),
                align_items: AlignItems::Center,
                border_radius: BorderRadius::all(Val::Px(4.0)),
                ..default()
            },
            BackgroundColor(if selected {
                theme::ACCENT_SELECTED
            } else {
                Color::NONE
            }),
            RelativeCursorPosition::default(),
            AnimationRow(index),
        ))
        .with_children(|row| {
            row.spawn((
                Text::new(label),
                TextFont::from_font_size(13.0),
                TextColor(theme::TEXT),
                AnimationRowLabel(index),
            ));
        });
}

/// Keeps row labels in step with the animations' names and the rename buffer.
fn sync_labels(editor: Res<EditorState>, mut labels: Query<(&AnimationRowLabel, &mut Text)>) {
    let rename = editor.animation_renaming.as_ref();
    for (label, mut text) in &mut labels {
        let value = match (rename, editor.document.animations.get(label.0)) {
            (Some(rename), _) if rename.index == label.0 => format!("{}|", rename.buffer),
            (_, Some(animation)) => animation_label(animation, label.0),
            _ => String::new(),
        };
        if text.0 != value {
            text.0 = value;
        }
    }
}

/// Highlights the selected row, and hovered rows.
fn paint_rows(
    editor: Res<EditorState>,
    mut rows: Query<(&AnimationRow, &Interaction, &mut BackgroundColor)>,
) {
    for (row, interaction, mut background) in &mut rows {
        let color = if editor.selected_animation == Some(row.0) {
            theme::ACCENT_SELECTED
        } else if *interaction != Interaction::None {
            theme::ACCENT_FAINT
        } else {
            Color::NONE
        };
        if background.0 != color {
            background.0 = color;
        }
    }
}

/// Shows the panel only in animate mode.
fn sync_visibility(mode: Res<EditorMode>, mut panels: Query<&mut Node, With<AnimateSidebarRoot>>) {
    let display = if *mode == EditorMode::Animate {
        Display::Flex
    } else {
        Display::None
    };
    for mut node in &mut panels {
        if node.display != display {
            node.display = display;
        }
    }
}
