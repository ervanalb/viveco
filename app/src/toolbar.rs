//! Left-hand tool palette.
//!
//! Switches between the selection tool and the pen tool. The active tool is
//! stored in [`ToolState`], which the editor reads to decide how canvas input
//! should behave. Switching away from the pen finishes any in-progress path so
//! it is not lost.

use bevy::prelude::*;

use crate::editor::{EditorState, MENU_BAR_HEIGHT, TOOLBAR_WIDTH, Tool, ToolState, finish_pen};
use crate::theme;

pub struct ToolbarPlugin;

impl Plugin for ToolbarPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, spawn_toolbar)
            .add_systems(Update, (handle_tool_buttons, update_tool_visuals).chain());
    }
}

/// A button that activates a tool when clicked.
#[derive(Component, Clone, Copy)]
struct ToolButton(Tool);

fn spawn_toolbar(mut commands: Commands) {
    commands
        .spawn((
            Node {
                position_type: PositionType::Absolute,
                left: Val::Px(0.0),
                top: Val::Px(MENU_BAR_HEIGHT),
                bottom: Val::Px(0.0),
                width: Val::Px(TOOLBAR_WIDTH),
                flex_direction: FlexDirection::Column,
                align_items: AlignItems::Center,
                padding: UiRect::all(Val::Px(8.0)),
                row_gap: Val::Px(6.0),
                ..default()
            },
            BackgroundColor(theme::PANEL_BG),
        ))
        .with_children(|panel| {
            panel.spawn((
                Text::new("Tools"),
                TextFont::from_font_size(10.0),
                TextColor(theme::MUTED),
            ));
            spawn_tool_button(panel, Tool::Select, "Select");
            spawn_tool_button(panel, Tool::Pen, "Pen");
        });
}

fn spawn_tool_button(parent: &mut ChildSpawnerCommands, tool: Tool, label: &str) {
    parent
        .spawn((
            Button,
            Node {
                width: Val::Px(40.0),
                height: Val::Px(40.0),
                align_items: AlignItems::Center,
                justify_content: JustifyContent::Center,
                border: UiRect::all(Val::Px(1.0)),
                border_radius: BorderRadius::all(Val::Px(6.0)),
                ..default()
            },
            BackgroundColor(theme::BUTTON_BG),
            BorderColor::all(theme::BORDER_FAINT),
            ToolButton(tool),
        ))
        .with_children(|button| {
            button.spawn((
                Text::new(label),
                TextFont::from_font_size(10.0),
                TextColor(theme::TEXT),
            ));
        });
}

/// Activates a tool from a button click or the V/P shortcuts.
fn handle_tool_buttons(
    buttons: Query<(&Interaction, &ToolButton), Changed<Interaction>>,
    keys: Res<ButtonInput<KeyCode>>,
    mut tool: ResMut<ToolState>,
    mut editor: ResMut<EditorState>,
) {
    let mut requested = None;
    for (interaction, button) in &buttons {
        if *interaction == Interaction::Pressed {
            requested = Some(button.0);
        }
    }
    // Ctrl+V is paste and Ctrl+P is reserved for the browser/print convention;
    // the letter shortcuts only apply on their own. While a tree row is being
    // renamed, they belong to the text field.
    let ctrl = keys.pressed(KeyCode::ControlLeft) || keys.pressed(KeyCode::ControlRight);
    if !ctrl && editor.renaming.is_none() {
        if keys.just_pressed(KeyCode::KeyV) {
            requested = Some(Tool::Select);
        }
        if keys.just_pressed(KeyCode::KeyP) {
            requested = Some(Tool::Pen);
        }
    }

    let Some(next) = requested else {
        return;
    };
    if tool.0 == next {
        return;
    }
    // Do not lose a half-drawn path when leaving the pen tool.
    finish_pen(&mut editor, true);
    tool.0 = next;
}

/// Highlights the active tool's button.
fn update_tool_visuals(
    tool: Res<ToolState>,
    mut last: Local<Option<Tool>>,
    mut buttons: Query<(&ToolButton, &mut BackgroundColor)>,
) {
    if last.as_ref() == Some(&tool.0) {
        return;
    }
    *last = Some(tool.0);

    for (button, mut background) in &mut buttons {
        background.0 = if button.0 == tool.0 {
            theme::ACCENT_SELECTED
        } else {
            theme::BUTTON_BG
        };
    }
}
