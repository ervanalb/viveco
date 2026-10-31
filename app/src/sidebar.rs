//! Right-hand object tree.
//!
//! Shows the document hierarchy (layers, paths, and clip groups) and lets the
//! user select, reorder, reparent, and rename nodes. Selection lives in
//! [`EditorState`], the same resource the canvas uses, so the tree and the
//! canvas stay in sync.
//!
//! Rows are ordinary [`Button`] entities, with a [`RelativeCursorPosition`] used
//! to find the row under the cursor. Insertion points are explicit slot nodes:
//! one above the first row of each list and one below every row. Because a
//! node's children live inside its container, the slot below a parent falls
//! below its whole subtree. Slots are indented to their depth so they line up
//! with the rows they sit between.

use bevy::input::ButtonState;
use bevy::input::keyboard::KeyboardInput;
use bevy::prelude::*;
use bevy::ui::{FocusPolicy, RelativeCursorPosition};
use bevy::window::PrimaryWindow;

use crate::editor::{
    CanvasInput, EditorState, MENU_BAR_HEIGHT, PathAddress, SIDEBAR_WIDTH, address_is_inside,
    cancel_rename, commit_rename, move_item_into, move_item_near, move_item_to_layer, move_layer,
    name_at,
};
use crate::model::{Document, Item, Layer, Path};
use crate::theme;

/// Unselected rows are transparent.
const ROW_BG: Color = Color::NONE;

/// Height of the visible drop indicator line, in screen pixels.
const DROP_LINE_HEIGHT: f32 = 2.0;

/// Height a drop slot occupies in the layout, in screen pixels.
const DROP_SLOT_HEIGHT: f32 = 8.0;

/// Height of a drop slot's invisible hit area, in screen pixels.
///
/// The hit area is taller than the layout slot and overflows into the rows on
/// either side, so drops are easy to hit without padding out the list.
const DROP_HIT_HEIGHT: f32 = 16.0;

/// Extra space below a node's children, in screen pixels.
///
/// This keeps the slot's hit area below a node's last child separate from the
/// one below the node itself, which would otherwise overlap.
const CHILDREN_PADDING_BOTTOM: f32 = 8.0;

/// Left inset of a top-level row, in screen pixels.
const ROW_PADDING_LEFT: f32 = 8.0;

/// Right inset shared by rows and drop slots, in screen pixels.
const ROW_PADDING_RIGHT: f32 = 8.0;

/// Extra left inset added per tree level, matching the children's indent.
const ROW_INDENT: f32 = 14.0;

/// Left inset of a row or drop line at `depth` levels below a layer.
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

pub struct SidebarPlugin;

impl Plugin for SidebarPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<TreeDrag>()
            .add_systems(Startup, spawn_sidebar)
            .add_systems(
                Update,
                (
                    handle_rename_input,
                    handle_row_press,
                    update_drag_hover,
                    finish_drag,
                    rebuild_tree,
                    sync_row_labels,
                    paint_rows,
                )
                    .chain()
                    // Canvas input runs first so it still sees a rename as
                    // active during the click that commits it.
                    .after(CanvasInput),
            );
    }
}

/// The scrollable container holding the tree rows.
#[derive(Component)]
struct SidebarTree;

/// A clickable node addressing one node in the document.
#[derive(Component)]
struct TreeRow {
    address: PathAddress,
}

/// The text node of a row, kept up to date while its name is edited.
#[derive(Component)]
struct RowLabel {
    address: PathAddress,
}

/// An invisible hit area marking an insertion point between rows.
#[derive(Component)]
struct DropSlot {
    target: PathAddress,
    kind: DropKind,
}

/// The visible line inside a [`DropSlot`].
#[derive(Component)]
struct SlotLine {
    target: PathAddress,
    kind: DropKind,
}

/// Where a dragged row would be dropped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DropKind {
    /// Above `target`, between it and the row before it.
    Before,
    /// Below `target`, and below all of its children.
    After,
    /// Inside `target` (parenting for items).
    Into,
    /// Appended to the end of a layer.
    LayerAppend,
}

/// Pointer state for dragging tree rows.
///
/// Rows are rebuilt whenever the selection changes, so the drag lives in a
/// resource keyed by [`PathAddress`] rather than on an entity.
#[derive(Resource, Default)]
struct TreeDrag {
    /// The row the press started on.
    source: Option<PathAddress>,
    /// Screen position of the press, used to detect an actual drag.
    origin: Vec2,
    /// Set once the pointer has moved far enough to count as a drag.
    dragging: bool,
    /// The row and zone currently under the pointer.
    hovered: Option<(PathAddress, DropKind)>,
    /// The last press, for double-click detection.
    last_click: Option<(f64, Vec2, PathAddress)>,
}

fn spawn_sidebar(mut commands: Commands) {
    commands
        .spawn((
            Node {
                position_type: PositionType::Absolute,
                top: Val::Px(MENU_BAR_HEIGHT),
                bottom: Val::Px(0.0),
                right: Val::Px(0.0),
                width: Val::Px(SIDEBAR_WIDTH),
                flex_direction: FlexDirection::Column,
                padding: UiRect::all(Val::Px(12.0)),
                row_gap: Val::Px(8.0),
                ..default()
            },
            BackgroundColor(theme::PANEL_BG),
        ))
        .with_children(|panel| {
            panel.spawn((
                Text::new("Objects"),
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
                SidebarTree,
            ));
            panel.spawn((
                Text::new("Drag to reorder - Double-click to rename - Delete to remove"),
                TextFont::from_font_size(11.0),
                TextColor(theme::MUTED),
            ));
        });
}

/// Rebuilds the tree whenever the object hierarchy, selection, or rename
/// target changes.
///
/// Ordinary edits (such as dragging a point) do not change the tree, and
/// typing while renaming only updates labels, so neither rebuilds rows.
fn rebuild_tree(
    mut commands: Commands,
    editor: Res<EditorState>,
    tree: Query<Entity, With<SidebarTree>>,
    mut last: Local<Option<(u64, Option<PathAddress>, Option<PathAddress>)>>,
) {
    let renaming = editor
        .renaming
        .as_ref()
        .map(|rename| rename.address.clone());
    let signature = (editor.structure_revision, editor.selected.clone(), renaming);
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
    let layers: Vec<usize> = editor
        .document
        .layer_indices_in_paint_order()
        .into_iter()
        .rev()
        .collect();
    spawn_layer_list(
        parent,
        &editor.document,
        &layers,
        0,
        editor.selected.as_ref(),
    );
}

/// Spawns a list of layers with an insertion slot above the first and below
/// every layer.
fn spawn_layer_list(
    parent: &mut ChildSpawnerCommands,
    document: &Document,
    layers: &[usize],
    depth: usize,
    selected: Option<&PathAddress>,
) {
    let Some(&first) = layers.first() else {
        return;
    };
    spawn_slot(
        parent,
        PathAddress::Layer { layer: first },
        DropKind::Before,
        depth,
    );

    for &layer_index in layers {
        let layer = &document.layers[layer_index];
        let address = PathAddress::Layer { layer: layer_index };
        let is_selected = selected == Some(&address);
        spawn_container(
            parent,
            &layer_label(layer, layer_index),
            depth,
            address.clone(),
            is_selected,
            |container| {
                spawn_item_list(
                    container,
                    &layer.items,
                    layer_index,
                    &[],
                    depth + 1,
                    selected,
                );
            },
        );
        spawn_slot(parent, address, DropKind::After, depth);
    }
}

/// Spawns a list of items with an insertion slot above the first and below
/// every item.
///
/// The list is wrapped in a padded container so that the slot below a nested
/// list's last item does not sit flush against the slot below the node that
/// owns the list.
fn spawn_item_list(
    parent: &mut ChildSpawnerCommands,
    items: &[Item],
    layer: usize,
    parents: &[usize],
    depth: usize,
    selected: Option<&PathAddress>,
) {
    let Some(first_index) = items.len().checked_sub(1) else {
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
            let mut first_chain = parents.to_vec();
            first_chain.push(first_index);
            spawn_slot(
                list,
                item_address(layer, first_chain, &items[first_index]),
                DropKind::Before,
                depth,
            );

            // Later items are painted on top, so list them first.
            for (index, item) in items.iter().enumerate().rev() {
                let mut chain = parents.to_vec();
                chain.push(index);
                let address = item_address(layer, chain.clone(), item);
                let is_selected = selected == Some(&address);

                match item {
                    Item::Path(path_item) => {
                        spawn_container(
                            list,
                            &path_label(&path_item.path),
                            depth,
                            address.clone(),
                            is_selected,
                            |_| {},
                        );
                    }
                    Item::Clip(group) => {
                        spawn_container(
                            list,
                            &clip_label(&group.clip),
                            depth,
                            address.clone(),
                            is_selected,
                            |container| {
                                spawn_item_list(
                                    container,
                                    &group.targets,
                                    layer,
                                    &chain,
                                    depth + 1,
                                    selected,
                                );
                            },
                        );
                    }
                }

                spawn_slot(list, address, DropKind::After, depth);
            }
        });
}

/// The address of an item inside a layer.
fn item_address(layer: usize, items: Vec<usize>, item: &Item) -> PathAddress {
    if matches!(item, Item::Clip(_)) {
        PathAddress::Clip { layer, items }
    } else {
        PathAddress::Item { layer, items }
    }
}

/// Spawns one tree node: a selectable header row plus a nested container for
/// its children.
///
/// Grouping each node with its children means the parent's `After` slot, which
/// is spawned by the surrounding list, lands below the whole subtree.
fn spawn_container(
    parent: &mut ChildSpawnerCommands,
    label: &str,
    depth: usize,
    address: PathAddress,
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
    address: PathAddress,
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
            TreeRow {
                address: address.clone(),
            },
        ))
        .with_children(|row| {
            row.spawn((
                Text::new(label),
                TextFont::from_font_size(13.0),
                TextColor(theme::TEXT),
                RowLabel {
                    address: address.clone(),
                },
            ));
        });
}

/// Spawns an insertion slot between two rows.
///
/// The slot keeps a fixed height so the layout does not jump when the
/// indicator appears; only the line inside it is toggled.
fn spawn_slot(
    parent: &mut ChildSpawnerCommands,
    target: PathAddress,
    kind: DropKind,
    depth: usize,
) {
    // The hit area and the line share the same horizontal insets as a row's
    // content, so a drop target lines up with the row it sits against. The hit
    // area is absolutely positioned so it can overflow vertically into the
    // neighbouring rows without taking up extra layout space.
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
                // The slot may cover a sliver of the rows above and below; let
                // their button interactions through.
                FocusPolicy::Pass,
                DropSlot {
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
                SlotLine { target, kind },
            ));
        });
}

// ---------------------------------------------------------------------------
// Renaming
// ---------------------------------------------------------------------------

/// Types into, commits, or cancels the row being renamed.
fn handle_rename_input(mut events: MessageReader<KeyboardInput>, mut editor: ResMut<EditorState>) {
    if editor.renaming.is_none() {
        for _ in events.read() {}
        return;
    }

    let mut commit = false;
    let mut cancel = false;
    {
        let Some(renaming) = editor.renaming.as_mut() else {
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
                    renaming.buffer.pop();
                }
                _ => {
                    if let Some(text) = &event.text {
                        for ch in text.chars() {
                            if !ch.is_control() {
                                renaming.buffer.push(ch);
                            }
                        }
                    }
                }
            }
        }
    }

    if cancel {
        cancel_rename(&mut editor);
    } else if commit {
        commit_rename(&mut editor);
    }
}

/// Selects a row on press and starts dragging, or begins a rename on the
/// second press of a double-click.
fn handle_row_press(
    rows: Query<(&Interaction, &TreeRow), Changed<Interaction>>,
    mouse: Res<ButtonInput<MouseButton>>,
    time: Res<Time>,
    windows: Query<&Window, With<PrimaryWindow>>,
    mut drag: ResMut<TreeDrag>,
    mut editor: ResMut<EditorState>,
) {
    if !mouse.just_pressed(MouseButton::Left) {
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
            editor.begin_rename(address);
            return;
        }

        drag.last_click = Some((now, cursor, address.clone()));
        drag.source = Some(address.clone());
        drag.origin = cursor;
        drag.dragging = false;
        drag.hovered = None;
        editor.select(Some(address));
        return;
    }
}

// ---------------------------------------------------------------------------
// Dragging
// ---------------------------------------------------------------------------

/// Tracks how far the pointer has moved and which slot or row it is over.
fn update_drag_hover(
    mouse: Res<ButtonInput<MouseButton>>,
    windows: Query<&Window, With<PrimaryWindow>>,
    mut drag: ResMut<TreeDrag>,
    slots: Query<(&DropSlot, &RelativeCursorPosition)>,
    rows: Query<(&TreeRow, &RelativeCursorPosition)>,
) {
    let Some(source) = drag.source.clone() else {
        return;
    };
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

    // Slots are separate nodes between the rows, so at most one is under the
    // cursor and they never fight with a row for the pointer.
    for (slot, relative) in &slots {
        if relative.cursor_over() {
            drag.hovered = resolve_drop(&source, &slot.target, slot.kind);
            return;
        }
    }

    // Outside a slot, the row under the pointer means "inside it".
    for (row, relative) in &rows {
        if !relative.cursor_over() {
            continue;
        }
        let kind = if matches!(row.address, PathAddress::Layer { .. }) {
            DropKind::LayerAppend
        } else {
            DropKind::Into
        };
        drag.hovered = resolve_drop(&source, &row.address, kind);
        return;
    }
}

/// Checks that a drop is allowed, returning the resolved target.
///
/// Layers only reorder among themselves. Items can be inserted between or
/// inside other items, and appended to a layer. A node can never be dropped on
/// itself or into its own subtree.
fn resolve_drop(
    source: &PathAddress,
    target: &PathAddress,
    kind: DropKind,
) -> Option<(PathAddress, DropKind)> {
    let source_is_layer = matches!(source, PathAddress::Layer { .. });
    let target_is_layer = matches!(target, PathAddress::Layer { .. });
    let allowed = match kind {
        DropKind::Before | DropKind::After => source_is_layer == target_is_layer,
        DropKind::Into => !source_is_layer && !target_is_layer,
        DropKind::LayerAppend => !source_is_layer && target_is_layer,
    };
    if !allowed || source == target || address_is_inside(source, target) {
        return None;
    }
    Some((target.clone(), kind))
}

/// Applies the drop when the mouse is released after a drag.
fn finish_drag(
    mouse: Res<ButtonInput<MouseButton>>,
    mut drag: ResMut<TreeDrag>,
    mut editor: ResMut<EditorState>,
) {
    if !mouse.just_released(MouseButton::Left) {
        return;
    }

    let source = drag.source.take();
    let hovered = drag.hovered.take();
    let dragging = std::mem::replace(&mut drag.dragging, false);
    if !dragging {
        return;
    }
    let (Some(source), Some((target, kind))) = (source, hovered) else {
        return;
    };
    apply_drop(&mut editor, &source, &target, kind);
}

fn apply_drop(
    editor: &mut EditorState,
    source: &PathAddress,
    target: &PathAddress,
    kind: DropKind,
) {
    let moved = match (source, target) {
        (
            PathAddress::Layer {
                layer: source_layer,
            },
            PathAddress::Layer {
                layer: target_layer,
            },
        ) => {
            if move_layer(
                &mut editor.document,
                *source_layer,
                *target_layer,
                kind == DropKind::After,
            ) {
                Some(source.clone())
            } else {
                None
            }
        }
        (source, PathAddress::Layer { layer }) if !matches!(source, PathAddress::Layer { .. }) => {
            move_item_to_layer(&mut editor.document, source, *layer)
        }
        (source, _) if !matches!(source, PathAddress::Layer { .. }) => match kind {
            // Items are stored bottom-first but listed top-first, so inserting
            // at the target's index puts the item above it in the tree.
            DropKind::Before => move_item_near(&mut editor.document, source, target, true),
            DropKind::After => move_item_near(&mut editor.document, source, target, false),
            DropKind::Into => move_item_into(&mut editor.document, source, target),
            DropKind::LayerAppend => None,
        },
        _ => None,
    };

    if let Some(address) = moved {
        editor.select(Some(address));
        editor.structure_revision += 1;
    }
}

// ---------------------------------------------------------------------------
// Painting
// ---------------------------------------------------------------------------

/// Keeps row labels in step with names and the rename buffer.
fn sync_row_labels(editor: Res<EditorState>, mut labels: Query<(&RowLabel, &mut Text)>) {
    let renaming = editor.renaming.as_ref();
    for (label, mut text) in &mut labels {
        let value = match renaming {
            Some(rename) if rename.address == label.address => format!("{}|", rename.buffer),
            _ => row_name(&editor.document, &label.address),
        };
        if text.0 != value {
            text.0 = value;
        }
    }
}

/// Colors rows and shows the drop indicator for the hovered zone.
fn paint_rows(
    drag: Res<TreeDrag>,
    editor: Res<EditorState>,
    mut rows: Query<(&TreeRow, &Interaction, &mut BackgroundColor)>,
    mut lines: Query<(&SlotLine, &mut Node)>,
) {
    for (row, interaction, mut background) in &mut rows {
        let selected = editor.selected.as_ref() == Some(&row.address);
        let inside = drag.hovered.as_ref().is_some_and(|(address, kind)| {
            address == &row.address && matches!(kind, DropKind::Into | DropKind::LayerAppend)
        });
        let color = if inside {
            theme::ACCENT_FAINT
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

fn row_name(document: &crate::model::Document, address: &PathAddress) -> String {
    let fallback = match address {
        PathAddress::Layer { layer } => format!("Layer {}", layer + 1),
        PathAddress::Item { .. } => "Path".to_string(),
        PathAddress::Clip { .. } => "Path (clip)".to_string(),
    };
    let name = name_at(document, address).unwrap_or_default();
    if name.is_empty() {
        fallback
    } else if matches!(address, PathAddress::Clip { .. }) {
        format!("{name} (clip)")
    } else {
        name.to_string()
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ClipGroup, PathItem, Style};

    fn named_line(name: &str) -> Path {
        let mut path = Path::new([0.0, 0.0]).line_to([1.0, 0.0]);
        path.name = name.into();
        path
    }

    fn named_path(name: &str) -> Item {
        Item::Path(PathItem {
            path: named_line(name),
            style: Style::default(),
        })
    }

    fn names(items: &[Item]) -> Vec<String> {
        items
            .iter()
            .map(|item| match item {
                Item::Path(path_item) => path_item.path.name.clone(),
                Item::Clip(group) => group.clip.name.clone(),
            })
            .collect()
    }

    fn state(items: Vec<Item>) -> EditorState {
        EditorState::new(Document {
            version: 1,
            layers: vec![Layer {
                name: "L0".into(),
                z: 0.0,
                items,
            }],
        })
    }

    fn layer(index: usize) -> PathAddress {
        PathAddress::Layer { layer: index }
    }

    fn item(chain: &[usize]) -> PathAddress {
        PathAddress::Item {
            layer: 0,
            items: chain.to_vec(),
        }
    }

    fn clip(chain: &[usize]) -> PathAddress {
        PathAddress::Clip {
            layer: 0,
            items: chain.to_vec(),
        }
    }

    fn displayed_layers(editor: &EditorState) -> Vec<String> {
        editor
            .document
            .layer_indices_in_paint_order()
            .into_iter()
            .rev()
            .map(|index| editor.document.layers[index].name.clone())
            .collect()
    }

    #[test]
    fn a_before_slot_inserts_above_the_target() {
        // Stored A, B, C; displayed C, B, A.
        let mut editor = state(vec![named_path("A"), named_path("B"), named_path("C")]);

        // The top slot above B.
        apply_drop(&mut editor, &item(&[0]), &item(&[1]), DropKind::Before);

        assert_eq!(names(&editor.document.layers[0].items), ["B", "A", "C"]);
    }

    #[test]
    fn an_after_slot_inserts_below_the_target() {
        let mut editor = state(vec![named_path("A"), named_path("B"), named_path("C")]);

        // The slot between C and B puts C below B.
        apply_drop(&mut editor, &item(&[2]), &item(&[1]), DropKind::After);

        assert_eq!(names(&editor.document.layers[0].items), ["A", "C", "B"]);
    }

    #[test]
    fn the_slot_below_the_last_item_lands_at_the_bottom() {
        let mut editor = state(vec![named_path("A"), named_path("B"), named_path("C")]);

        // Below the bottom row, which is A.
        apply_drop(&mut editor, &item(&[2]), &item(&[0]), DropKind::After);

        assert_eq!(names(&editor.document.layers[0].items), ["C", "A", "B"]);
    }

    /// Layer 0 holds `X` then a clip `C` with targets `t0`, `t1`. Displayed
    /// top to bottom: `C`, `t1`, `t0`, `X`.
    fn nested_state() -> EditorState {
        state(vec![
            named_path("X"),
            Item::Clip(ClipGroup {
                clip: named_line("C"),
                style: Style::default(),
                targets: vec![named_path("t0"), named_path("t1")],
            }),
        ])
    }

    #[test]
    fn the_slot_below_a_clip_stays_in_the_layer() {
        let mut editor = nested_state();

        // The layer-level slot below the clip keeps X where it is.
        apply_drop(&mut editor, &item(&[0]), &clip(&[1]), DropKind::After);

        let items = &editor.document.layers[0].items;
        assert_eq!(names(items), ["X", "C"]);
        let Item::Clip(group) = &items[1] else {
            panic!("expected a clip group");
        };
        assert_eq!(names(&group.targets), ["t0", "t1"]);
    }

    #[test]
    fn the_slot_below_a_clips_last_child_moves_the_item_inside() {
        let mut editor = nested_state();

        // The slot inside the clip, below its last displayed child t0.
        apply_drop(&mut editor, &item(&[0]), &item(&[1, 0]), DropKind::After);

        let items = &editor.document.layers[0].items;
        assert_eq!(names(items), ["C"]);
        let Item::Clip(group) = &items[0] else {
            panic!("expected a clip group");
        };
        assert_eq!(names(&group.targets), ["X", "t0", "t1"]);
    }

    #[test]
    fn slots_respect_the_dragged_kind() {
        // Layers only reorder, so an item target is not a layer slot.
        assert_eq!(resolve_drop(&layer(0), &item(&[0]), DropKind::After), None);
        // Items do not insert between layers.
        assert_eq!(resolve_drop(&item(&[0]), &layer(0), DropKind::After), None);
        // ...but they do append to a layer.
        assert_eq!(
            resolve_drop(&item(&[0]), &layer(0), DropKind::LayerAppend),
            Some((layer(0), DropKind::LayerAppend))
        );
        // And they can be placed inside other items.
        assert_eq!(
            resolve_drop(&item(&[0]), &clip(&[1]), DropKind::Into),
            Some((clip(&[1]), DropKind::Into))
        );
        // Layers reorder against one another.
        assert_eq!(
            resolve_drop(&layer(0), &layer(1), DropKind::Before),
            Some((layer(1), DropKind::Before))
        );
    }

    #[test]
    fn a_node_cannot_drop_on_itself_or_inside_itself() {
        assert_eq!(
            resolve_drop(&clip(&[1]), &clip(&[1]), DropKind::Before),
            None
        );
        assert_eq!(
            resolve_drop(&clip(&[1]), &item(&[1, 0]), DropKind::After),
            None
        );
        assert_eq!(
            resolve_drop(&clip(&[1]), &item(&[1, 0]), DropKind::Into),
            None
        );
        assert_eq!(
            resolve_drop(&clip(&[1]), &item(&[0]), DropKind::Before),
            Some((item(&[0]), DropKind::Before))
        );
    }

    #[test]
    fn a_layer_moved_before_another_lands_above_it() {
        let mut editor = EditorState::new(Document {
            version: 1,
            layers: vec![
                Layer {
                    name: "A".into(),
                    z: 1.0,
                    items: Vec::new(),
                },
                Layer {
                    name: "B".into(),
                    z: 0.0,
                    items: Vec::new(),
                },
            ],
        });
        assert_eq!(displayed_layers(&editor), ["A", "B"]);

        // B moves into the slot above A.
        apply_drop(&mut editor, &layer(1), &layer(0), DropKind::Before);

        assert_eq!(displayed_layers(&editor), ["B", "A"]);
    }
}
