//! Left-hand tool palette.
//!
//! Switches between the selection tool and the pen tool. The active tool is
//! stored in [`ToolState`], which the editor reads to decide how canvas input
//! should behave. Switching away from the pen finishes any in-progress path so
//! it is not lost.

use bevy::prelude::*;

use crate::editor::{EditorState, TOOLBAR_WIDTH, Tool, ToolState, finish_pen};

const PANEL_BG: Color = Color::srgba_u8(0x12, 0x15, 0x1c, 0xf2);
const TEXT_COLOR: Color = Color::srgb_u8(0xdd, 0xe1, 0xe8);
const HEADER_COLOR: Color = Color::srgb_u8(0x8b, 0x93, 0xa1);
const BUTTON_BG: Color = Color::srgba_u8(0xff, 0xff, 0xff, 0x0a);
const BUTTON_ACTIVE: Color = Color::srgba_u8(0x4c, 0xc9, 0xf0, 0x59);

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
                top: Val::Px(0.0),
                width: Val::Px(TOOLBAR_WIDTH),
                height: Val::Percent(100.0),
                flex_direction: FlexDirection::Column,
                align_items: AlignItems::Center,
                padding: UiRect::all(Val::Px(8.0)),
                row_gap: Val::Px(6.0),
                ..default()
            },
            BackgroundColor(PANEL_BG),
        ))
        .with_children(|panel| {
            panel.spawn((
                Text::new("Tools"),
                TextFont::from_font_size(10.0),
                TextColor(HEADER_COLOR),
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
            BackgroundColor(BUTTON_BG),
            BorderColor::all(Color::srgba_u8(0xff, 0xff, 0xff, 0x1f)),
            ToolButton(tool),
        ))
        .with_children(|button| {
            button.spawn((
                Text::new(label),
                TextFont::from_font_size(10.0),
                TextColor(TEXT_COLOR),
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
    if keys.just_pressed(KeyCode::KeyV) {
        requested = Some(Tool::Select);
    }
    if keys.just_pressed(KeyCode::KeyP) {
        requested = Some(Tool::Pen);
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
            BUTTON_ACTIVE
        } else {
            BUTTON_BG
        };
    }
}
