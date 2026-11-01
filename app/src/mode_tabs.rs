//! Mode selector shown directly below the menu bar.
//!
//! Switching modes never discards document edits; it only finishes transient
//! gestures (such as a half-drawn path) so that the next mode starts cleanly.

use bevy::prelude::*;

use crate::editor::{
    EditorMode, EditorState, MENU_BAR_HEIGHT, TAB_BAR_HEIGHT, Tool, ToolState, commit_rename,
    finish_pen,
};
use crate::rig::{RigState, commit_bone_rename};
use crate::theme;

pub struct ModeTabsPlugin;

impl Plugin for ModeTabsPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, spawn_tabs)
            .add_systems(Update, (handle_tab_buttons, style_tabs).chain());
    }
}

/// One selectable mode tab.
#[derive(Component)]
struct ModeTab(EditorMode);

fn spawn_tabs(mut commands: Commands) {
    commands
        .spawn((
            Node {
                position_type: PositionType::Absolute,
                left: Val::Px(0.0),
                top: Val::Px(MENU_BAR_HEIGHT),
                width: Val::Percent(100.0),
                height: Val::Px(TAB_BAR_HEIGHT),
                flex_direction: FlexDirection::Row,
                align_items: AlignItems::Center,
                padding: UiRect::horizontal(Val::Px(8.0)),
                column_gap: Val::Px(4.0),
                ..default()
            },
            BackgroundColor(theme::PANEL_BG),
            GlobalZIndex(45),
        ))
        .with_children(|bar| {
            spawn_tab(bar, EditorMode::Draw, "Draw");
            spawn_tab(bar, EditorMode::Rig, "Rig");
        });
}

fn spawn_tab(bar: &mut ChildSpawnerCommands, mode: EditorMode, label: &str) {
    bar.spawn((
        Button,
        Node {
            height: Val::Percent(100.0),
            min_width: Val::Px(72.0),
            padding: UiRect::horizontal(Val::Px(14.0)),
            align_items: AlignItems::Center,
            justify_content: JustifyContent::Center,
            border_radius: BorderRadius::bottom(Val::Px(4.0)),
            ..default()
        },
        BackgroundColor(theme::BUTTON_BG),
        ModeTab(mode),
    ))
    .with_children(|tab| {
        tab.spawn((
            Text::new(label),
            TextFont::from_font_size(13.0),
            TextColor(theme::TEXT),
        ));
    });
}

fn handle_tab_buttons(
    tabs: Query<(&Interaction, &ModeTab), Changed<Interaction>>,
    mut mode: ResMut<EditorMode>,
    mut editor: ResMut<EditorState>,
    mut rig: ResMut<RigState>,
    mut tool: ResMut<ToolState>,
) {
    for (interaction, tab) in &tabs {
        if *interaction != Interaction::Pressed || *mode == tab.0 {
            continue;
        }
        switch_mode(&mut mode, &mut editor, &mut rig, &mut tool, tab.0);
    }
}

fn switch_mode(
    mode: &mut EditorMode,
    editor: &mut EditorState,
    rig: &mut RigState,
    tool: &mut ToolState,
    next: EditorMode,
) {
    // Finish transient gestures before leaving a mode so nothing is silently
    // abandoned or carried across.
    match *mode {
        EditorMode::Draw => {
            commit_rename(editor);
            finish_pen(editor, true);
        }
        EditorMode::Rig => {
            commit_bone_rename(editor);
        }
    }
    rig.cancel_editing();
    // Every mode starts on the select tool.
    tool.0 = Tool::Select;
    *mode = next;
}

fn style_tabs(
    mode: Res<EditorMode>,
    mut tabs: Query<(&ModeTab, &Interaction, &mut BackgroundColor)>,
) {
    for (tab, interaction, mut background) in &mut tabs {
        let color = if tab.0 == *mode {
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
