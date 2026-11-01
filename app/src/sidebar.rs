//! Right-hand object tree.
//!
//! Shows the document hierarchy (shapes, their paths, and nested child shapes)
//! and lets the user select, reorder, reparent, and rename nodes. Selection
//! lives in [`EditorState`], the same resource the canvas uses, so the tree and
//! the canvas stay in sync.
//!
//! Rows are ordinary [`Button`] entities, with a [`RelativeCursorPosition`] used
//! to find the row under the cursor. Insertion points are explicit slot nodes:
//! one above the first row of each sibling list and one below every shape.
//! Because a shape's contents live inside its container, the slot below a shape
//! falls below its whole subtree. Slots are indented to their depth so they
//! line up with the rows they sit between.
//!
//! Sibling rows are listed in paint order, so a row below its siblings is drawn
//! on top of them and a parent's row sits above the rows of its children.
//!
//! Only sibling shapes can be reordered, since Z only orders shapes among their
//! siblings. Paths are shown under their owning shape: slots between path rows
//! reorder them within that shape, and dropping one on another shape's row
//! moves it into that shape.

use bevy::input::ButtonState;
use bevy::input::keyboard::KeyboardInput;
use bevy::prelude::*;
use bevy::ui::{FocusPolicy, RelativeCursorPosition};
use bevy::window::PrimaryWindow;

use crate::editor::{
    CONTENT_TOP, CanvasInput, EditorMode, EditorState, SIDEBAR_WIDTH, TreeAddress,
    address_is_inside, cancel_rename, commit_rename, move_path_into_shape, move_path_near,
    move_shape_into, move_shape_near, name_at,
};
use crate::model::{Document, Path, Shape, indices_in_paint_order};
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
                    sync_sidebar_visibility,
                )
                    .chain()
                    // Canvas input runs first so it still sees a rename as
                    // active during the click that commits it.
                    .after(CanvasInput),
            );
    }
}

/// The panel that hosts the object tree.
#[derive(Component)]
struct SidebarRoot;

/// The scrollable container holding the tree rows.
#[derive(Component)]
struct SidebarTree;

/// A clickable node addressing one node in the document.
#[derive(Component)]
struct TreeRow {
    address: TreeAddress,
}

/// The text node of a row, kept up to date while its name is edited.
#[derive(Component)]
struct RowLabel {
    address: TreeAddress,
}

/// An invisible hit area marking an insertion point between rows.
#[derive(Component)]
struct DropSlot {
    target: TreeAddress,
    kind: DropKind,
}

/// The visible line inside a [`DropSlot`].
#[derive(Component)]
struct SlotLine {
    target: TreeAddress,
    kind: DropKind,
}

/// Where a dragged row would be dropped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DropKind {
    /// Above `target`, between it and the shape before it.
    Before,
    /// Below `target`, and below all of its children.
    After,
    /// Inside `target`: a child shape, or a path moved into a shape.
    Into,
}

/// Pointer state for dragging tree rows.
///
/// Rows are rebuilt whenever the selection changes, so the drag lives in a
/// resource keyed by [`TreeAddress`] rather than on an entity.
#[derive(Resource, Default)]
struct TreeDrag {
    /// The row the press started on.
    source: Option<TreeAddress>,
    /// Screen position of the press, used to detect an actual drag.
    origin: Vec2,
    /// Set once the pointer has moved far enough to count as a drag.
    dragging: bool,
    /// The row and zone currently under the pointer.
    hovered: Option<(TreeAddress, DropKind)>,
    /// The last press, for double-click detection.
    last_click: Option<(f64, Vec2, TreeAddress)>,
}

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
                ..default()
            },
            BackgroundColor(theme::PANEL_BG),
            SidebarRoot,
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
        });
}

/// Shows the object tree only in draw mode; rig mode shows the skeleton tree
/// in the same place.
fn sync_sidebar_visibility(mode: Res<EditorMode>, mut panels: Query<&mut Node, With<SidebarRoot>>) {
    let display = if *mode == EditorMode::Draw {
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

/// Rebuilds the tree whenever the object hierarchy, selection, or rename
/// target changes.
///
/// Ordinary edits (such as dragging a point) do not change the tree, and
/// typing while renaming only updates labels, so neither rebuilds rows.
fn rebuild_tree(
    mut commands: Commands,
    editor: Res<EditorState>,
    tree: Query<Entity, With<SidebarTree>>,
    mut last: Local<Option<(u64, Option<TreeAddress>, Option<TreeAddress>)>>,
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
    // Bottom-most shape first, so each row lists what is drawn above it below.
    spawn_shape_list(
        parent,
        &editor.document.shapes,
        &[],
        0,
        editor.selected.as_ref(),
    );
}

/// The address of a child shape of the shape at `parents`.
fn shape_address(parents: &[usize], index: usize) -> TreeAddress {
    let mut shapes = parents.to_vec();
    shapes.push(index);
    TreeAddress::Shape { shapes }
}

/// Spawns a sibling list of shapes with an insertion slot above the first and
/// below every shape.
///
/// The list is wrapped in a padded container so that the slot below a nested
/// list's last shape does not sit flush against the slot below the shape that
/// owns the list.
///
/// Rows are listed in paint order, so siblings further down the list are
/// painted on top of the ones above them. A parent's row sits above its
/// children's rows for the same reason: children paint over their parent.
fn spawn_shape_list(
    parent: &mut ChildSpawnerCommands,
    shapes: &[Shape],
    parents: &[usize],
    depth: usize,
    selected: Option<&TreeAddress>,
) {
    let indices = indices_in_paint_order(shapes);
    let Some(&first) = indices.first() else {
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
            spawn_slot(list, shape_address(parents, first), DropKind::Before, depth);

            for &index in &indices {
                let shape = &shapes[index];
                let mut chain = parents.to_vec();
                chain.push(index);
                let address = TreeAddress::Shape {
                    shapes: chain.clone(),
                };
                let is_selected = selected == Some(&address);

                spawn_container(
                    list,
                    &shape_label(shape),
                    depth,
                    address.clone(),
                    is_selected,
                    |container| {
                        // The shape's own paths come first, directly beneath
                        // it. A slot above the first path and one below every
                        // path reorder them (and move them between shapes).
                        if !shape.paths.is_empty() {
                            spawn_slot(
                                container,
                                TreeAddress::Path {
                                    shapes: chain.clone(),
                                    path: 0,
                                },
                                DropKind::Before,
                                depth + 1,
                            );
                        }
                        for path_index in 0..shape.paths.len() {
                            let path_address = TreeAddress::Path {
                                shapes: chain.clone(),
                                path: path_index,
                            };
                            let path_selected = selected == Some(&path_address);
                            spawn_row(
                                container,
                                &path_label(&shape.paths[path_index]),
                                depth + 1,
                                path_address.clone(),
                                path_selected,
                            );
                            spawn_slot(container, path_address, DropKind::After, depth + 1);
                        }
                        // Then the child shapes, one level deeper.
                        if !shape.children.is_empty() {
                            spawn_shape_list(
                                container,
                                &shape.children,
                                &chain,
                                depth + 1,
                                selected,
                            );
                        }
                    },
                );

                spawn_slot(list, address, DropKind::After, depth);
            }
        });
}

/// Spawns one tree node: a selectable header row plus a nested container for
/// its contents.
///
/// Grouping each node with its contents means the parent's `After` slot, which
/// is spawned by the surrounding list, lands below the whole subtree.
fn spawn_container(
    parent: &mut ChildSpawnerCommands,
    label: &str,
    depth: usize,
    address: TreeAddress,
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
    address: TreeAddress,
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
    target: TreeAddress,
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
    // cursor. Adjacent slots can overlap near a list boundary where only one of
    // them accepts the dragged kind, so skip the ones that refuse the drop
    // instead of stopping there.
    let resolved = slots
        .iter()
        .filter(|(_, relative)| relative.cursor_over())
        .find_map(|(slot, _)| resolve_drop(&source, &slot.target, slot.kind));
    if let Some(resolved) = resolved {
        drag.hovered = Some(resolved);
        return;
    }

    // Outside a slot, the row under the pointer means "inside it".
    for (row, relative) in &rows {
        if relative.cursor_over()
            && let Some(resolved) = resolve_drop(&source, &row.address, DropKind::Into)
        {
            drag.hovered = Some(resolved);
            return;
        }
    }
}

/// Checks that a drop is allowed, returning the resolved target.
///
/// Sibling shapes reorder among themselves or nest inside one another. Paths
/// reorder within a shape, move next to a path in another shape, or move into a
/// shape's row. A node can never be dropped on itself, into its own subtree, or
/// into the shape that already owns it.
fn resolve_drop(
    source: &TreeAddress,
    target: &TreeAddress,
    kind: DropKind,
) -> Option<(TreeAddress, DropKind)> {
    let allowed = match (source, target) {
        (TreeAddress::Shape { .. }, TreeAddress::Shape { .. }) => {
            !address_is_inside(source, target)
        }
        (TreeAddress::Path { shapes, .. }, TreeAddress::Shape { shapes: owner }) => {
            kind == DropKind::Into && shapes != owner
        }
        (TreeAddress::Path { .. }, TreeAddress::Path { .. }) => kind != DropKind::Into,
        _ => false,
    };
    (allowed && source != target).then(|| (target.clone(), kind))
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
    source: &TreeAddress,
    target: &TreeAddress,
    kind: DropKind,
) {
    let moved = match (source, target) {
        (TreeAddress::Shape { .. }, TreeAddress::Shape { .. }) => match kind {
            // Rows are listed in paint order, so a slot above the target means
            // a lower Z (before it in the vector) and a slot below means a
            // higher Z (after it).
            DropKind::Before => move_shape_near(&mut editor.document, source, target, false),
            DropKind::After => move_shape_near(&mut editor.document, source, target, true),
            DropKind::Into => move_shape_into(&mut editor.document, source, target),
        },
        (TreeAddress::Path { .. }, TreeAddress::Shape { .. }) if kind == DropKind::Into => {
            move_path_into_shape(&mut editor.document, source, target)
        }
        (TreeAddress::Path { .. }, TreeAddress::Path { .. }) => match kind {
            // Path rows are listed in vector order, so "before" is the lower
            // index and "after" is the higher one.
            DropKind::Before => move_path_near(&mut editor.document, source, target, false),
            DropKind::After => move_path_near(&mut editor.document, source, target, true),
            DropKind::Into => None,
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

/// A display label, falling back to `fallback` when the name is empty.
fn label(name: &str, fallback: &str) -> String {
    if name.is_empty() {
        fallback.to_string()
    } else {
        name.to_string()
    }
}

fn row_name(document: &Document, address: &TreeAddress) -> String {
    let name = name_at(document, address).unwrap_or_default();
    match address {
        TreeAddress::Shape { .. } => label(name, "Shape"),
        TreeAddress::Path { .. } => label(name, "Path"),
    }
}

fn shape_label(shape: &Shape) -> String {
    label(&shape.name, "Shape")
}

fn path_label(path: &Path) -> String {
    label(&path.name, "Path")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::FORMAT_VERSION;

    fn named_shape(name: &str, z: f32) -> Shape {
        Shape {
            name: name.into(),
            z,
            paths: vec![Path::new([0.0, 0.0]).line_to([10.0, 0.0])],
            ..default()
        }
    }

    fn paths_shape(names: &[&str]) -> Shape {
        Shape {
            paths: names
                .iter()
                .enumerate()
                .map(|(index, name)| {
                    let x = index as f32 * 20.0;
                    Path::new([x, 0.0]).line_to([x + 10.0, 0.0]).named(*name)
                })
                .collect(),
            ..default()
        }
    }

    fn path_names(shape: &Shape) -> Vec<String> {
        shape.paths.iter().map(|path| path.name.clone()).collect()
    }

    fn names(shapes: &[Shape]) -> Vec<String> {
        shapes.iter().map(|shape| shape.name.clone()).collect()
    }

    fn state(shapes: Vec<Shape>) -> EditorState {
        EditorState::new(Document {
            version: FORMAT_VERSION,
            bones: Vec::new(),
            shapes,
        })
    }

    fn shape(chain: &[usize]) -> TreeAddress {
        TreeAddress::Shape {
            shapes: chain.to_vec(),
        }
    }

    fn path(chain: &[usize], index: usize) -> TreeAddress {
        TreeAddress::Path {
            shapes: chain.to_vec(),
            path: index,
        }
    }

    fn displayed_shapes(editor: &EditorState) -> Vec<String> {
        editor
            .document
            .shape_indices_in_paint_order()
            .into_iter()
            .map(|index| editor.document.shapes[index].name.clone())
            .collect()
    }

    #[test]
    fn a_before_slot_inserts_above_the_target() {
        // Stored A, B, C; displayed A, B, C.
        let mut editor = state(vec![
            named_shape("A", 0.0),
            named_shape("B", 1.0),
            named_shape("C", 2.0),
        ]);

        // The slot above B takes C up to sit between A and B.
        apply_drop(&mut editor, &shape(&[2]), &shape(&[1]), DropKind::Before);

        assert_eq!(names(&editor.document.shapes), ["A", "C", "B"]);
        assert_eq!(displayed_shapes(&editor), ["A", "C", "B"]);
    }

    #[test]
    fn an_after_slot_inserts_below_the_target() {
        let mut editor = state(vec![
            named_shape("A", 0.0),
            named_shape("B", 1.0),
            named_shape("C", 2.0),
        ]);

        // The slot below B takes A down to sit between B and C.
        apply_drop(&mut editor, &shape(&[0]), &shape(&[1]), DropKind::After);

        assert_eq!(names(&editor.document.shapes), ["B", "A", "C"]);
        assert_eq!(displayed_shapes(&editor), ["B", "A", "C"]);
    }

    #[test]
    fn the_slot_below_the_last_shape_lands_at_the_bottom() {
        let mut editor = state(vec![
            named_shape("A", 0.0),
            named_shape("B", 1.0),
            named_shape("C", 2.0),
        ]);

        // Below the bottom row, which is C.
        apply_drop(&mut editor, &shape(&[0]), &shape(&[2]), DropKind::After);

        assert_eq!(names(&editor.document.shapes), ["B", "C", "A"]);
        assert_eq!(displayed_shapes(&editor), ["B", "C", "A"]);
    }

    #[test]
    fn reordering_renormalizes_sibling_z() {
        let mut editor = state(vec![
            named_shape("A", 10.0),
            named_shape("B", 20.0),
            named_shape("C", 30.0),
        ]);

        apply_drop(&mut editor, &shape(&[0]), &shape(&[2]), DropKind::After);

        let zs: Vec<f32> = editor.document.shapes.iter().map(|shape| shape.z).collect();
        assert_eq!(zs, vec![0.0, 1.0, 2.0]);
    }

    /// Top-level `X` plus parent `P` with a single child `C`.
    fn nested_state() -> EditorState {
        let mut parent = named_shape("P", 1.0);
        parent.children = vec![named_shape("C", 0.0)];
        state(vec![named_shape("X", 0.0), parent])
    }

    #[test]
    fn a_child_can_be_moved_out_below_a_top_level_shape() {
        let mut editor = nested_state();

        // The slot below X takes C out of P.
        apply_drop(&mut editor, &shape(&[1, 0]), &shape(&[0]), DropKind::After);

        assert_eq!(names(&editor.document.shapes), ["X", "C", "P"]);
        assert!(editor.document.shapes[2].children.is_empty());
        // The child keeps its geometry, it is just reparented.
        assert_eq!(editor.document.shapes[0].paths.len(), 1);
    }

    #[test]
    fn a_shape_can_be_dropped_inside_another() {
        let mut editor = nested_state();

        // Dropping X onto P makes it P's second child.
        apply_drop(&mut editor, &shape(&[0]), &shape(&[1]), DropKind::Into);

        assert_eq!(names(&editor.document.shapes), ["P"]);
        assert_eq!(names(&editor.document.shapes[0].children), ["C", "X"]);
    }

    #[test]
    fn a_path_can_be_moved_into_another_shape() {
        let mut editor = nested_state();

        // `X` has one path at [0, 0]; move it into `P` at [1].
        apply_drop(&mut editor, &path(&[0], 0), &shape(&[1]), DropKind::Into);

        assert!(editor.document.shapes[0].paths.is_empty());
        assert_eq!(editor.document.shapes[1].paths.len(), 2);
        assert_eq!(editor.selected, Some(path(&[1], 1)));
    }

    #[test]
    fn a_path_reorders_within_a_shape() {
        let mut editor = state(vec![paths_shape(&["A", "B", "C"])]);

        // The slot above B takes C up to sit between A and B.
        apply_drop(
            &mut editor,
            &path(&[0], 2),
            &path(&[0], 1),
            DropKind::Before,
        );

        assert_eq!(path_names(&editor.document.shapes[0]), ["A", "C", "B"]);
        assert_eq!(editor.selected, Some(path(&[0], 1)));
    }

    #[test]
    fn a_path_can_be_dropped_between_another_shapes_paths() {
        let mut editor = state(vec![paths_shape(&["A", "B"]), paths_shape(&["C"])]);

        // The slot below C takes B out of the first shape and puts it below C.
        apply_drop(&mut editor, &path(&[0], 1), &path(&[1], 0), DropKind::After);

        assert_eq!(path_names(&editor.document.shapes[0]), ["A"]);
        assert_eq!(path_names(&editor.document.shapes[1]), ["C", "B"]);
        assert_eq!(editor.selected, Some(path(&[1], 1)));
    }

    #[test]
    fn slots_respect_the_dragged_kind() {
        // A path does not use a shape's sibling slots, but it does use slots
        // between paths, including a path in another shape.
        assert_eq!(
            resolve_drop(&path(&[0], 0), &shape(&[1]), DropKind::Before),
            None
        );
        assert_eq!(
            resolve_drop(&path(&[0], 0), &shape(&[1]), DropKind::After),
            None
        );
        assert_eq!(
            resolve_drop(&path(&[0], 0), &shape(&[1]), DropKind::Into),
            Some((shape(&[1]), DropKind::Into))
        );
        assert_eq!(
            resolve_drop(&path(&[0], 0), &path(&[1], 0), DropKind::Before),
            Some((path(&[1], 0), DropKind::Before))
        );
        assert_eq!(
            resolve_drop(&path(&[0], 0), &path(&[0], 1), DropKind::After),
            Some((path(&[0], 1), DropKind::After))
        );
        assert_eq!(
            resolve_drop(&path(&[0], 0), &path(&[0], 1), DropKind::Into),
            None
        );

        // A path cannot move into the shape that already owns it.
        assert_eq!(
            resolve_drop(&path(&[0], 0), &shape(&[0]), DropKind::Into),
            None
        );

        // A shape cannot be dropped onto a path target.
        assert_eq!(
            resolve_drop(&shape(&[0]), &path(&[1], 0), DropKind::Into),
            None
        );

        // Sibling shapes reorder and nest freely.
        assert_eq!(
            resolve_drop(&shape(&[0]), &shape(&[1]), DropKind::Before),
            Some((shape(&[1]), DropKind::Before))
        );
        assert_eq!(
            resolve_drop(&shape(&[0]), &shape(&[1]), DropKind::Into),
            Some((shape(&[1]), DropKind::Into))
        );
    }

    #[test]
    fn a_node_cannot_drop_on_itself_or_inside_itself() {
        assert_eq!(
            resolve_drop(&shape(&[1]), &shape(&[1]), DropKind::Before),
            None
        );
        assert_eq!(
            resolve_drop(&shape(&[1]), &shape(&[1, 0]), DropKind::After),
            None
        );
        assert_eq!(
            resolve_drop(&shape(&[1]), &shape(&[1, 0]), DropKind::Into),
            None
        );
        assert_eq!(
            resolve_drop(&shape(&[1]), &shape(&[0]), DropKind::Before),
            Some((shape(&[0]), DropKind::Before))
        );
    }
}
