//! Right-hand object tree.
//!
//! Shows the document hierarchy (layers, paths, and clip groups) and lets the
//! user select any paintable path. Selection lives in [`EditorState`], the same
//! resource the canvas uses, so the tree and the canvas stay in sync.

use std::cmp::Ordering;

use bevy::prelude::*;

use crate::editor::{EditorState, PathAddress, SIDEBAR_WIDTH};
use crate::model::{Item, Layer, Path};

const PANEL_BG: Color = Color::srgba_u8(0x12, 0x15, 0x1c, 0xf2);
const TEXT_COLOR: Color = Color::srgb_u8(0xdd, 0xe1, 0xe8);
const HEADER_COLOR: Color = Color::srgb_u8(0x8b, 0x93, 0xa1);
const ROW_BG: Color = Color::NONE;
const ROW_HOVER: Color = Color::srgba_u8(0x4c, 0xc9, 0xf0, 0x26);
const ROW_SELECTED: Color = Color::srgba_u8(0x4c, 0xc9, 0xf0, 0x59);

pub struct SidebarPlugin;

impl Plugin for SidebarPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, spawn_sidebar)
            .add_systems(Update, (select_from_tree, rebuild_tree).chain());
    }
}

/// The scrollable container holding the tree rows.
#[derive(Component)]
struct SidebarTree;

/// A clickable node addressing one path in the document.
#[derive(Component)]
struct TreeRow {
    address: PathAddress,
}

fn spawn_sidebar(mut commands: Commands) {
    commands
        .spawn((
            Node {
                position_type: PositionType::Absolute,
                top: Val::Px(0.0),
                right: Val::Px(0.0),
                width: Val::Px(SIDEBAR_WIDTH),
                height: Val::Percent(100.0),
                flex_direction: FlexDirection::Column,
                padding: UiRect::all(Val::Px(12.0)),
                row_gap: Val::Px(8.0),
                ..default()
            },
            BackgroundColor(PANEL_BG),
        ))
        .with_children(|panel| {
            panel.spawn((
                Text::new("Objects"),
                TextFont::from_font_size(16.0),
                TextColor(TEXT_COLOR),
            ));
            panel.spawn((
                Node {
                    flex_direction: FlexDirection::Column,
                    row_gap: Val::Px(2.0),
                    width: Val::Percent(100.0),
                    flex_grow: 1.0,
                    overflow: Overflow::clip_y(),
                    ..default()
                },
                SidebarTree,
            ));
            panel.spawn((
                Text::new("Click to select - Delete to remove"),
                TextFont::from_font_size(11.0),
                TextColor(HEADER_COLOR),
            ));
        });
}

/// Applies a tree click to the editor selection.
fn select_from_tree(
    mut rows: Query<(&Interaction, &TreeRow, &mut BackgroundColor), Changed<Interaction>>,
    mut editor: ResMut<EditorState>,
) {
    for (interaction, row, mut background) in &mut rows {
        match *interaction {
            Interaction::Pressed => {
                editor.select(Some(row.address.clone()));
            }
            Interaction::Hovered => {
                background.0 = ROW_HOVER;
            }
            Interaction::None => {
                let selected = editor.selected.as_ref() == Some(&row.address);
                background.0 = if selected { ROW_SELECTED } else { ROW_BG };
            }
        }
    }
}

/// Rebuilds the tree whenever the object hierarchy or the selection changes.
///
/// Rebuilding is avoided on ordinary edits (such as dragging a point) because
/// those do not alter the tree or its labels.
fn rebuild_tree(
    mut commands: Commands,
    editor: Res<EditorState>,
    tree: Query<Entity, With<SidebarTree>>,
    mut last: Local<Option<(u64, Option<PathAddress>)>>,
) {
    let signature = (editor.structure_revision, editor.selected.clone());
    if last.as_ref() == Some(&signature) {
        return;
    }
    *last = Some(signature);

    let Ok(tree) = tree.single() else {
        return;
    };
    commands.entity(tree).despawn_children();
    commands.entity(tree).with_children(|parent| {
        spawn_rows(parent, &editor);
    });
}

fn spawn_rows(parent: &mut ChildSpawnerCommands, editor: &EditorState) {
    // Topmost layer first, matching the way the picture stacks on screen.
    let mut layers: Vec<usize> = (0..editor.document.layers.len()).collect();
    layers.sort_by(|&a, &b| {
        let left = &editor.document.layers[a];
        let right = &editor.document.layers[b];
        right.z.partial_cmp(&left.z).unwrap_or(Ordering::Equal)
    });

    let selected = editor.selected.as_ref();
    for layer_index in layers {
        let layer = &editor.document.layers[layer_index];
        spawn_header(parent, &layer_label(layer, layer_index), 0);
        spawn_items(parent, &layer.items, layer_index, &[], 1, selected);
    }
}

fn spawn_items(
    parent: &mut ChildSpawnerCommands,
    items: &[Item],
    layer: usize,
    parents: &[usize],
    depth: usize,
    selected: Option<&PathAddress>,
) {
    // Later items are painted on top, so list them first.
    for (index, item) in items.iter().enumerate().rev() {
        let mut chain = parents.to_vec();
        chain.push(index);

        match item {
            Item::Path(path_item) => {
                let address = PathAddress::Item {
                    layer,
                    items: chain,
                };
                let is_selected = selected == Some(&address);
                spawn_row(
                    parent,
                    &path_label(&path_item.path),
                    depth,
                    address,
                    is_selected,
                );
            }
            Item::Clip(group) => {
                let address = PathAddress::Clip {
                    layer,
                    items: chain.clone(),
                };
                let is_selected = selected == Some(&address);
                spawn_row(
                    parent,
                    &clip_label(&group.clip),
                    depth,
                    address,
                    is_selected,
                );
                spawn_items(parent, &group.targets, layer, &chain, depth + 1, selected);
            }
        }
    }
}

fn spawn_row(
    parent: &mut ChildSpawnerCommands,
    label: &str,
    depth: usize,
    address: PathAddress,
    selected: bool,
) {
    parent
        .spawn((
            Button,
            Node {
                width: Val::Percent(100.0),
                padding: UiRect {
                    left: Val::Px(8.0 + depth as f32 * 14.0),
                    right: Val::Px(8.0),
                    top: Val::Px(3.0),
                    bottom: Val::Px(3.0),
                },
                align_items: AlignItems::Center,
                border_radius: BorderRadius::all(Val::Px(4.0)),
                ..default()
            },
            BackgroundColor(if selected { ROW_SELECTED } else { ROW_BG }),
            TreeRow { address },
        ))
        .with_children(|row| {
            row.spawn((
                Text::new(label),
                TextFont::from_font_size(13.0),
                TextColor(TEXT_COLOR),
            ));
        });
}

fn spawn_header(parent: &mut ChildSpawnerCommands, label: &str, depth: usize) {
    parent.spawn((
        Text::new(label),
        TextFont::from_font_size(12.0),
        TextColor(HEADER_COLOR),
        Node {
            margin: UiRect::new(
                Val::Px(8.0 + depth as f32 * 14.0),
                Val::Px(8.0),
                Val::Px(6.0),
                Val::Px(2.0),
            ),
            ..default()
        },
    ));
}

fn path_label(path: &Path) -> String {
    if path.name.is_empty() {
        "Path".to_string()
    } else {
        path.name.clone()
    }
}

fn clip_label(clip: &Path) -> String {
    format!("{} (clip)", path_label(clip))
}

fn layer_label(layer: &Layer, index: usize) -> String {
    if layer.name.is_empty() {
        format!("Layer {}", index + 1)
    } else {
        layer.name.clone()
    }
}
