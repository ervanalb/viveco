//! Right-hand skeleton tree for rig mode.
//!
//! Mirrors the object tree: rows select bones, pressing and dragging reparents
//! or reorders them, and double-clicking starts an inline rename. The graphic's
//! object tree is hidden while this one is shown.
//!
//! Bones have no paint order, so rows are listed in vector order and the
//! `Before`/`After` slots simply reorder siblings.

use bevy::prelude::*;
use bevy::ui::{FocusPolicy, RelativeCursorPosition};
use bevy::window::PrimaryWindow;

use crate::editor::{CONTENT_TOP, CanvasInput, EditorMode, EditorState, SIDEBAR_WIDTH};
use crate::model::Bone;
use crate::rig::{
    BoneAddress, address_is_inside, begin_bone_rename, bone_name_at, move_bone_into, move_bone_near,
};
use crate::theme;

/// Unselected rows are transparent.
const ROW_BG: Color = Color::NONE;

/// Height of the visible drop indicator line, in screen pixels.
const DROP_LINE_HEIGHT: f32 = 2.0;

/// Height a drop slot occupies in the layout, in screen pixels.
const DROP_SLOT_HEIGHT: f32 = 8.0;

/// Height of a drop slot's invisible hit area, in screen pixels.
const DROP_HIT_HEIGHT: f32 = 16.0;

/// Extra space below a node's children, in screen pixels.
const CHILDREN_PADDING_BOTTOM: f32 = 8.0;

/// Left inset of a top-level row, in screen pixels.
const ROW_PADDING_LEFT: f32 = 8.0;

/// Right inset shared by rows and drop slots, in screen pixels.
const ROW_PADDING_RIGHT: f32 = 8.0;

/// Extra left inset added per tree level, matching the children's indent.
const ROW_INDENT: f32 = 14.0;

/// Left inset of a row or drop line at `depth` tree levels.
fn indent_for_depth(depth: usize) -> f32 {
    ROW_PADDING_LEFT + depth as f32 * ROW_INDENT
}

/// Distance, in screen pixels, the pointer must travel before a press becomes
/// a drag rather than a click.
const DRAG_THRESHOLD: f32 = 4.0;

/// Longest gap between two presses that still counts as a double-click, in
/// seconds.
const DOUBLE_CLICK_SECONDS: f64 = 0.35;

/// Largest cursor travel between the two presses of a double-click, in pixels.
const DOUBLE_CLICK_DISTANCE: f32 = 6.0;

pub struct RigSidebarPlugin;

impl Plugin for RigSidebarPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<BoneTreeDrag>()
            .add_systems(Startup, spawn_rig_sidebar)
            .add_systems(
                Update,
                (
                    handle_row_press,
                    update_drag_hover,
                    finish_drag,
                    rebuild_tree,
                    sync_row_labels,
                    paint_rows,
                    sync_sidebar_visibility,
                )
                    .chain()
                    .after(CanvasInput),
            );
    }
}

/// The panel that hosts the skeleton tree.
#[derive(Component)]
struct RigSidebarRoot;

/// The scrollable container holding the bone rows.
#[derive(Component)]
struct RigTree;

/// A clickable row addressing one bone.
#[derive(Component)]
struct BoneRow {
    address: BoneAddress,
}

/// The text node of a row, kept up to date while its name is edited.
#[derive(Component)]
struct BoneRowLabel {
    address: BoneAddress,
}

/// An invisible hit area marking an insertion point between rows.
#[derive(Component)]
struct BoneDropSlot {
    target: BoneAddress,
    kind: DropKind,
}

/// The visible line inside a [`BoneDropSlot`].
#[derive(Component)]
struct BoneSlotLine {
    target: BoneAddress,
    kind: DropKind,
}

/// Where a dragged bone row would be dropped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DropKind {
    /// Above `target`, between it and the bone before it.
    Before,
    /// Below `target`, and below all of its children.
    After,
    /// Inside `target` as its last child.
    Into,
}

/// Pointer state for dragging bone rows.
#[derive(Resource, Default)]
struct BoneTreeDrag {
    source: Option<BoneAddress>,
    origin: Vec2,
    dragging: bool,
    hovered: Option<(BoneAddress, DropKind)>,
    last_click: Option<(f64, Vec2, BoneAddress)>,
}

fn spawn_rig_sidebar(mut commands: Commands) {
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
            RigSidebarRoot,
        ))
        .with_children(|panel| {
            panel.spawn((
                Text::new("Skeleton"),
                TextFont::from_font_size(16.0),
                TextColor(theme::TEXT),
            ));
            panel.spawn((
                Node {
                    flex_direction: FlexDirection::Column,
                    width: Val::Percent(100.0),
                    flex_grow: 1.0,
                    overflow: Overflow::clip_y(),
                    ..default()
                },
                RigTree,
            ));
        });
}

/// Shows the skeleton tree only in rig mode.
fn sync_sidebar_visibility(
    mode: Res<EditorMode>,
    mut panels: Query<&mut Node, With<RigSidebarRoot>>,
) {
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

/// Rebuilds the tree whenever the skeleton hierarchy, selection, or rename
/// target changes.
fn rebuild_tree(
    mut commands: Commands,
    editor: Res<EditorState>,
    tree: Query<Entity, With<RigTree>>,
    mut last: Local<Option<(u64, Option<BoneAddress>, Option<BoneAddress>)>>,
) {
    let renaming = editor
        .bone_renaming
        .as_ref()
        .map(|rename| rename.address.clone());
    let signature = (
        editor.structure_revision,
        editor.selected_bone.clone(),
        renaming,
    );
    if last.as_ref() == Some(&signature) {
        return;
    }
    *last = Some(signature);

    let Ok(tree) = tree.single() else {
        return;
    };
    commands.entity(tree).despawn_children();
    commands.entity(tree).with_children(|parent| {
        spawn_bone_list(
            parent,
            &editor.document.bones,
            &[],
            0,
            editor.selected_bone.as_ref(),
        );
    });
}

/// The address of a child bone of the bone at `parents`.
fn bone_address(parents: &[usize], index: usize) -> BoneAddress {
    let mut bones = parents.to_vec();
    bones.push(index);
    BoneAddress { bones }
}

/// Spawns a sibling list of bones with an insertion slot above the first and
/// below every bone.
fn spawn_bone_list(
    parent: &mut ChildSpawnerCommands,
    bones: &[Bone],
    parents: &[usize],
    depth: usize,
    selected: Option<&BoneAddress>,
) {
    let Some(first) = (0..bones.len()).next() else {
        return;
    };

    parent
        .spawn(Node {
            width: Val::Percent(100.0),
            flex_direction: FlexDirection::Column,
            padding: UiRect::bottom(Val::Px(CHILDREN_PADDING_BOTTOM)),
            ..default()
        })
        .with_children(|list| {
            spawn_slot(list, bone_address(parents, first), DropKind::Before, depth);

            for (index, bone) in bones.iter().enumerate() {
                let mut chain = parents.to_vec();
                chain.push(index);
                let address = BoneAddress {
                    bones: chain.clone(),
                };
                let is_selected = selected == Some(&address);

                spawn_container(
                    list,
                    &bone_label(bone),
                    depth,
                    address.clone(),
                    is_selected,
                    |container| {
                        if !bone.children.is_empty() {
                            spawn_bone_list(container, &bone.children, &chain, depth + 1, selected);
                        }
                    },
                );

                spawn_slot(list, address, DropKind::After, depth);
            }
        });
}

/// Spawns one tree node: a selectable header row plus a nested container for
/// its children.
fn spawn_container(
    parent: &mut ChildSpawnerCommands,
    label: &str,
    depth: usize,
    address: BoneAddress,
    selected: bool,
    children: impl FnOnce(&mut ChildSpawnerCommands),
) {
    parent
        .spawn(Node {
            width: Val::Percent(100.0),
            flex_direction: FlexDirection::Column,
            ..default()
        })
        .with_children(|container| {
            spawn_row(container, label, depth, address, selected);
            children(container);
        });
}

fn spawn_row(
    parent: &mut ChildSpawnerCommands,
    label: &str,
    depth: usize,
    address: BoneAddress,
    selected: bool,
) {
    parent
        .spawn((
            Button,
            Node {
                width: Val::Percent(100.0),
                padding: UiRect {
                    left: Val::Px(indent_for_depth(depth)),
                    right: Val::Px(ROW_PADDING_RIGHT),
                    top: Val::Px(2.0),
                    bottom: Val::Px(2.0),
                },
                align_items: AlignItems::Center,
                border_radius: BorderRadius::all(Val::Px(4.0)),
                ..default()
            },
            BackgroundColor(if selected {
                theme::ACCENT_SELECTED
            } else {
                ROW_BG
            }),
            RelativeCursorPosition::default(),
            BoneRow {
                address: address.clone(),
            },
        ))
        .with_children(|row| {
            row.spawn((
                Text::new(label),
                TextFont::from_font_size(13.0),
                TextColor(theme::TEXT),
                BoneRowLabel {
                    address: address.clone(),
                },
            ));
        });
}

/// Spawns an insertion slot between two rows.
fn spawn_slot(
    parent: &mut ChildSpawnerCommands,
    target: BoneAddress,
    kind: DropKind,
    depth: usize,
) {
    let left = Val::Px(indent_for_depth(depth));
    let right = Val::Px(ROW_PADDING_RIGHT);

    parent
        .spawn(Node {
            width: Val::Percent(100.0),
            height: Val::Px(DROP_SLOT_HEIGHT),
            flex_direction: FlexDirection::Column,
            justify_content: JustifyContent::Center,
            ..default()
        })
        .with_children(|slot| {
            slot.spawn((
                Node {
                    position_type: PositionType::Absolute,
                    left,
                    right,
                    top: Val::Px((DROP_SLOT_HEIGHT - DROP_HIT_HEIGHT) * 0.5),
                    height: Val::Px(DROP_HIT_HEIGHT),
                    ..default()
                },
                RelativeCursorPosition::default(),
                FocusPolicy::Pass,
                BoneDropSlot {
                    target: target.clone(),
                    kind,
                },
            ));

            slot.spawn((
                Node {
                    height: Val::Px(DROP_LINE_HEIGHT),
                    margin: UiRect {
                        left,
                        right,
                        ..default()
                    },
                    border_radius: BorderRadius::all(Val::Px(DROP_LINE_HEIGHT * 0.5)),
                    display: Display::None,
                    ..default()
                },
                BackgroundColor(theme::ACCENT),
                BoneSlotLine { target, kind },
            ));
        });
}

// ---------------------------------------------------------------------------
// Selection and dragging
// ---------------------------------------------------------------------------

/// Selects a row on press and starts dragging, or begins a rename on the
/// second press of a double-click.
fn handle_row_press(
    mode: Res<EditorMode>,
    rows: Query<(&Interaction, &BoneRow), Changed<Interaction>>,
    mouse: Res<ButtonInput<MouseButton>>,
    time: Res<Time>,
    windows: Query<&Window, With<PrimaryWindow>>,
    mut drag: ResMut<BoneTreeDrag>,
    mut editor: ResMut<EditorState>,
) {
    if *mode != EditorMode::Rig || !mouse.just_pressed(MouseButton::Left) {
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

        let now = time.elapsed_secs_f64();
        let address = row.address.clone();
        let double_click = drag.last_click.as_ref().is_some_and(|(t, p, a)| {
            now - *t <= DOUBLE_CLICK_SECONDS
                && cursor.distance(*p) <= DOUBLE_CLICK_DISTANCE
                && a == &address
        });
        if double_click {
            drag.last_click = None;
            drag.source = None;
            drag.dragging = false;
            drag.hovered = None;
            begin_bone_rename(&mut editor, address);
            return;
        }

        drag.last_click = Some((now, cursor, address.clone()));
        drag.source = Some(address.clone());
        drag.origin = cursor;
        drag.dragging = false;
        drag.hovered = None;
        editor.select_bone(Some(address));
        return;
    }
}

/// Tracks how far the pointer has moved and which slot or row it is over.
fn update_drag_hover(
    mode: Res<EditorMode>,
    mouse: Res<ButtonInput<MouseButton>>,
    windows: Query<&Window, With<PrimaryWindow>>,
    mut drag: ResMut<BoneTreeDrag>,
    slots: Query<(&BoneDropSlot, &RelativeCursorPosition)>,
    rows: Query<(&BoneRow, &RelativeCursorPosition)>,
) {
    let Some(source) = drag.source.clone() else {
        return;
    };
    if *mode != EditorMode::Rig {
        drag.source = None;
        drag.dragging = false;
        drag.hovered = None;
        return;
    }
    let Ok(window) = windows.single() else {
        return;
    };
    let Some(cursor) = window.cursor_position() else {
        return;
    };

    if mouse.pressed(MouseButton::Left)
        && !drag.dragging
        && cursor.distance(drag.origin) > DRAG_THRESHOLD
    {
        drag.dragging = true;
    }

    drag.hovered = None;
    if !drag.dragging {
        return;
    }

    let resolved = slots
        .iter()
        .filter(|(_, relative)| relative.cursor_over())
        .find_map(|(slot, _)| resolve_drop(&source, &slot.target, slot.kind));
    if let Some(resolved) = resolved {
        drag.hovered = Some(resolved);
        return;
    }

    for (row, relative) in &rows {
        if relative.cursor_over()
            && let Some(resolved) = resolve_drop(&source, &row.address, DropKind::Into)
        {
            drag.hovered = Some(resolved);
            return;
        }
    }
}

/// Checks that dropping `source` on `target` is allowed.
fn resolve_drop(
    source: &BoneAddress,
    target: &BoneAddress,
    kind: DropKind,
) -> Option<(BoneAddress, DropKind)> {
    let allowed = !address_is_inside(source, target);
    (allowed && source != target).then(|| (target.clone(), kind))
}

/// Applies the drop when the mouse is released after a drag.
fn finish_drag(
    mode: Res<EditorMode>,
    mouse: Res<ButtonInput<MouseButton>>,
    mut drag: ResMut<BoneTreeDrag>,
    mut editor: ResMut<EditorState>,
) {
    if !mouse.just_released(MouseButton::Left) {
        return;
    }

    let source = drag.source.take();
    let hovered = drag.hovered.take();
    let dragging = std::mem::replace(&mut drag.dragging, false);
    if *mode != EditorMode::Rig || !dragging {
        return;
    }
    let (Some(source), Some((target, kind))) = (source, hovered) else {
        return;
    };
    apply_drop(&mut editor, &source, &target, kind);
}

fn apply_drop(
    editor: &mut EditorState,
    source: &BoneAddress,
    target: &BoneAddress,
    kind: DropKind,
) {
    let moved = match kind {
        DropKind::Before => move_bone_near(&mut editor.document, source, target, false),
        DropKind::After => move_bone_near(&mut editor.document, source, target, true),
        DropKind::Into => move_bone_into(&mut editor.document, source, target),
    };

    if let Some(address) = moved {
        editor.select_bone(Some(address));
        editor.structure_revision += 1;
    }
}

// ---------------------------------------------------------------------------
// Painting
// ---------------------------------------------------------------------------

/// Keeps row labels in step with names and the rename buffer.
fn sync_row_labels(editor: Res<EditorState>, mut labels: Query<(&BoneRowLabel, &mut Text)>) {
    let renaming = editor.bone_renaming.as_ref();
    for (label, mut text) in &mut labels {
        let value = match renaming {
            Some(rename) if rename.address == label.address => format!("{}|", rename.buffer),
            _ => bone_row_name(&editor.document, &label.address),
        };
        if text.0 != value {
            text.0 = value;
        }
    }
}

/// Colors rows and shows the drop indicator for the hovered zone.
fn paint_rows(
    drag: Res<BoneTreeDrag>,
    editor: Res<EditorState>,
    mut rows: Query<(&BoneRow, &Interaction, &mut BackgroundColor)>,
    mut lines: Query<(&BoneSlotLine, &mut Node)>,
) {
    for (row, interaction, mut background) in &mut rows {
        let selected = editor.selected_bone.as_ref() == Some(&row.address);
        let inside = drag
            .hovered
            .as_ref()
            .is_some_and(|(address, kind)| address == &row.address && *kind == DropKind::Into);
        let color = if inside {
            theme::DROP_TARGET
        } else if selected {
            theme::ACCENT_SELECTED
        } else if *interaction == Interaction::Hovered {
            theme::ACCENT_FAINT
        } else {
            ROW_BG
        };
        if background.0 != color {
            background.0 = color;
        }
    }

    for (line, mut node) in &mut lines {
        let visible = drag
            .hovered
            .as_ref()
            .is_some_and(|(target, kind)| target == &line.target && kind == &line.kind);
        let display = if visible {
            Display::Flex
        } else {
            Display::None
        };
        if node.display != display {
            node.display = display;
        }
    }
}

// ---------------------------------------------------------------------------
// Labels
// ---------------------------------------------------------------------------

fn label(name: &str, fallback: &str) -> String {
    if name.is_empty() {
        fallback.to_string()
    } else {
        name.to_string()
    }
}

fn bone_label(bone: &Bone) -> String {
    label(&bone.name, "Bone")
}

fn bone_row_name(document: &crate::model::Document, address: &BoneAddress) -> String {
    let name = bone_name_at(document, address).unwrap_or_default();
    label(name, "Bone")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Document, FORMAT_VERSION};

    fn state(mut bones: Vec<Bone>) -> EditorState {
        let document = Document {
            version: FORMAT_VERSION,
            shapes: Vec::new(),
            bones: std::mem::take(&mut bones),
        };
        EditorState::new(document)
    }

    fn named(name: &str) -> Bone {
        Bone {
            name: name.into(),
            head: Some([0.0, 0.0]),
            tail: [10.0, 0.0],
            ..default()
        }
    }

    fn names(bones: &[Bone]) -> Vec<String> {
        bones.iter().map(|bone| bone.name.clone()).collect()
    }

    fn address(chain: &[usize]) -> BoneAddress {
        BoneAddress {
            bones: chain.to_vec(),
        }
    }

    #[test]
    fn an_after_slot_reorders_siblings() {
        let mut editor = state(vec![named("A"), named("B"), named("C")]);

        apply_drop(&mut editor, &address(&[0]), &address(&[2]), DropKind::After);

        assert_eq!(names(&editor.document.bones), ["B", "C", "A"]);
        assert_eq!(editor.selected_bone, Some(address(&[2])));
    }

    #[test]
    fn dropping_on_a_row_parents_the_bone() {
        let mut editor = state(vec![named("Parent"), named("Child")]);

        apply_drop(&mut editor, &address(&[1]), &address(&[0]), DropKind::Into);

        assert_eq!(names(&editor.document.bones), ["Parent"]);
        assert_eq!(names(&editor.document.bones[0].children), ["Child"]);
        assert_eq!(editor.selected_bone, Some(address(&[0, 0])));
    }

    #[test]
    fn a_bone_cannot_be_dropped_into_itself() {
        let mut editor = state(vec![named("Parent")]);
        apply_drop(&mut editor, &address(&[0]), &address(&[0]), DropKind::Into);
        assert_eq!(editor.document.bones.len(), 1);
    }
}
