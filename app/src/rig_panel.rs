//! Bottom panel showing the selected bone in rig mode.
//!
//! The values are read-only for now: this stage covers creating, parenting,
//! and reshaping bones. The panel makes the stored endpoints visible so the
//! rig's state is inspectable.

use bevy::prelude::*;

use crate::editor::{BOTTOM_BAR_HEIGHT, EditorMode, EditorState, SIDEBAR_WIDTH, TOOLBAR_WIDTH};
use crate::rig::{bone_at, bone_endpoints};
use crate::theme;

pub struct RigPanelPlugin;

impl Plugin for RigPanelPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, spawn_rig_panel)
            .add_systems(Update, (sync_visibility, sync_values));
    }
}

/// The root of the bottom rig panel.
#[derive(Component)]
struct RigPanelRoot;

/// The text block describing the selected bone.
#[derive(Component)]
struct BoneInfo;

fn spawn_rig_panel(mut commands: Commands) {
    commands
        .spawn((
            Node {
                position_type: PositionType::Absolute,
                left: Val::Px(TOOLBAR_WIDTH),
                right: Val::Px(SIDEBAR_WIDTH),
                bottom: Val::Px(0.0),
                height: Val::Px(BOTTOM_BAR_HEIGHT),
                flex_direction: FlexDirection::Column,
                padding: UiRect::all(Val::Px(12.0)),
                row_gap: Val::Px(8.0),
                display: Display::None,
                ..default()
            },
            BackgroundColor(theme::PANEL_BG),
            RigPanelRoot,
        ))
        .with_children(|panel| {
            panel.spawn((
                Text::new("Bone"),
                TextFont::from_font_size(14.0),
                TextColor(theme::MUTED),
            ));
            panel.spawn((
                Text::new("No bone selected"),
                TextFont::from_font_size(13.0),
                TextColor(theme::TEXT),
                BoneInfo,
            ));
        });
}

/// Shows the panel only in rig mode.
fn sync_visibility(mode: Res<EditorMode>, mut panels: Query<&mut Node, With<RigPanelRoot>>) {
    let display = if *mode == EditorMode::Rig {
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

/// Rebuilds the info text from the selected bone.
///
/// Endpoint drags change the transform without touching the structure
/// revision, so the rendered string itself is the change signature.
fn sync_values(
    editor: Res<EditorState>,
    mut info: Query<&mut Text, With<BoneInfo>>,
    mut last: Local<Option<String>>,
) {
    let address = editor.selected_bone.as_ref();
    let bone = address.and_then(|address| bone_at(&editor.document, address));
    let endpoints = address.and_then(|address| bone_endpoints(&editor.document, address));
    let value = match (bone, endpoints) {
        (Some(bone), Some((head, tail))) => {
            let dx = tail[0] - head[0];
            let dy = tail[1] - head[1];
            let length = (dx * dx + dy * dy).sqrt();
            let angle = dy.atan2(dx).to_degrees();
            format!(
                "{}\nHead: {:.2}, {:.2}\nTail: {:.2}, {:.2}\nLength: {:.2}\nAngle: {:.1}°",
                if bone.name.is_empty() {
                    "Unnamed bone"
                } else {
                    &bone.name
                },
                head[0],
                head[1],
                tail[0],
                tail[1],
                length,
                angle,
            )
        }
        _ => "No bone selected".to_string(),
    };

    if last.as_deref() == Some(value.as_str()) {
        return;
    }
    *last = Some(value.clone());

    for mut text in &mut info {
        text.0 = value.clone();
    }
}
