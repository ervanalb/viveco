//! Shared right-hand tree widget.
//!
//! Both the object tree (draw mode) and the skeleton tree (rig mode) show a
//! hierarchy of selectable rows with insertion slots between them. The layout,
//! pointer handling, drag-and-drop plumbing, inline renaming, and painting are
//! identical; only the document model and the move rules differ. That shared
//! behaviour lives here and is parameterized by a [`TreeModel`], which each
//! mode implements for its own address type.
//!
//! Rows are ordinary [`Button`] entities, with a [`RelativeCursorPosition`] used
//! to find the row under the cursor. Insertion points are explicit slot nodes:
//! one above the first row of each sibling list and one below every node.
//! Because a node's contents live inside its container, the slot below a node
//! falls below its whole subtree. Slots are indented to their depth so they
//! line up with the rows they sit between.
//!
//! A [`TreeModel`] supplies a flat-ish [`TreeNode`] layout of its hierarchy;
//! leaf rows are listed directly under their owner, and nested children are
//! rendered one level deeper.

use std::marker::PhantomData;

use bevy::input::ButtonState;
use bevy::input::keyboard::KeyboardInput;
use bevy::prelude::*;
use bevy::ui::{FocusPolicy, RelativeCursorPosition};
use bevy::window::PrimaryWindow;

use crate::editor::{CONTENT_TOP, EditorMode, EditorState, SIDEBAR_WIDTH};
use crate::theme;
use viveco::Asset;

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

// ---------------------------------------------------------------------------
// Model
// ---------------------------------------------------------------------------

/// Where a dragged row would be dropped relative to its target.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DropKind {
    /// Above `target`, between it and the node before it.
    Before,
    /// Below `target`, and below all of its children.
    After,
    /// Inside `target`: a child node, or a leaf moved into a node.
    Into,
}

/// One row rendered under its owner without nesting: a path in the object
/// tree.
pub struct TreeLeaf<A> {
    /// The node this row addresses.
    pub address: A,
    /// The text shown on the row.
    pub label: String,
}

/// One model node and everything rendered beneath it.
pub struct TreeNode<A> {
    /// The node this row addresses.
    pub address: A,
    /// The text shown on the row.
    pub label: String,
    /// Rows listed directly under the node, before its children.
    pub leaves: Vec<TreeLeaf<A>>,
    /// Nested nodes rendered one level deeper.
    pub children: Vec<TreeNode<A>>,
}

/// The document-specific half of the tree widget.
///
/// An address type implements this to describe its hierarchy and how editing
/// operations map onto the shared rows.
pub trait TreeModel: Clone + PartialEq + Send + Sync + 'static {
    /// Heading shown above the tree.
    fn heading() -> &'static str;

    /// Whether the tree is shown in `mode`.
    fn visible_in(mode: EditorMode) -> bool;

    /// The hierarchy to render.
    fn layout(document: &Asset) -> Vec<TreeNode<Self>>;

    /// The display name of the node at `address`.
    fn name(document: &Asset, address: &Self) -> String;

    /// The currently selected node, if any.
    fn selected(editor: &EditorState) -> Option<&Self>;

    /// Selects `address`.
    fn select(editor: &mut EditorState, address: Self);

    /// Clears the selection.
    fn deselect(editor: &mut EditorState);

    /// Starts an inline rename of `address`.
    fn begin_rename(editor: &mut EditorState, address: Self);

    /// The node whose name is being edited inline, if any.
    fn rename_target(editor: &EditorState) -> Option<&Self>;

    /// The text typed so far while renaming, if any.
    fn rename_buffer(editor: &EditorState) -> Option<&str>;

    /// The text typed so far, mutably, while renaming.
    fn rename_buffer_mut(editor: &mut EditorState) -> Option<&mut String>;

    /// Commits the in-progress rename.
    fn commit_rename(editor: &mut EditorState);

    /// Abandons the in-progress rename.
    fn cancel_rename(editor: &mut EditorState);

    /// Checks that dropping `source` on `target` is allowed, returning the
    /// resolved target.
    ///
    /// The document is available so hierarchies addressed by flat index can
    /// test ancestry.
    fn resolve_drop(
        document: &Asset,
        source: &Self,
        target: &Self,
        kind: DropKind,
    ) -> Option<(Self, DropKind)>;

    /// Moves `source` to `target` and returns the node's new address.
    fn drop_move(
        document: &mut Asset,
        source: &Self,
        target: &Self,
        kind: DropKind,
    ) -> Option<Self>;
}

// ---------------------------------------------------------------------------
// Components and resources
// ---------------------------------------------------------------------------

/// The panel that hosts a tree.
#[derive(Component)]
pub struct TreePanel<A: Send + Sync + 'static>(PhantomData<A>);

/// The scrollable container holding a tree's rows.
#[derive(Component)]
pub struct TreeContainer<A: Send + Sync + 'static>(PhantomData<A>);

/// A clickable node addressing one row.
#[derive(Component)]
pub struct TreeRow<A: Send + Sync + 'static> {
    address: A,
}

/// The text node of a row, kept up to date while its name is edited.
#[derive(Component)]
pub struct TreeRowLabel<A: Send + Sync + 'static> {
    address: A,
}

/// An invisible hit area marking an insertion point between rows.
#[derive(Component)]
pub struct TreeDropSlot<A: Send + Sync + 'static> {
    target: A,
    kind: DropKind,
}

/// The visible line inside a [`TreeDropSlot`].
#[derive(Component)]
pub struct TreeSlotLine<A: Send + Sync + 'static> {
    target: A,
    kind: DropKind,
}

/// Pointer state for dragging tree rows.
///
/// Rows are rebuilt whenever the selection changes, so the drag lives in a
/// resource keyed by the address type rather than on an entity.
#[derive(Resource)]
pub struct TreeDrag<A: Send + Sync + 'static> {
    /// The row the press started on.
    source: Option<A>,
    /// Screen position of the press, used to detect an actual drag.
    origin: Vec2,
    /// Set once the pointer has moved far enough to count as a drag.
    dragging: bool,
    /// The row and zone currently under the pointer.
    hovered: Option<(A, DropKind)>,
    /// The last press, for double-click detection.
    last_click: Option<(f64, Vec2, A)>,
}

impl<A: Send + Sync + 'static> Default for TreeDrag<A> {
    fn default() -> Self {
        Self {
            source: None,
            origin: Vec2::ZERO,
            dragging: false,
            hovered: None,
            last_click: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Spawning
// ---------------------------------------------------------------------------

/// Spawns the panel that hosts a model's tree.
pub fn spawn_panel<M: TreeModel>(mut commands: Commands) {
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
                display: if M::visible_in(EditorMode::Draw) {
                    Display::Flex
                } else {
                    Display::None
                },
                ..default()
            },
            BackgroundColor(theme::PANEL_BG),
            TreePanel::<M>(PhantomData),
        ))
        .with_children(|panel| {
            panel.spawn((
                Text::new(M::heading()),
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
                TreeContainer::<M>(PhantomData),
            ));
        });
}

/// Shows a tree only in the mode it belongs to.
pub fn sync_visibility<M: TreeModel>(
    mode: Res<EditorMode>,
    mut panels: Query<&mut Node, With<TreePanel<M>>>,
) {
    let display = if M::visible_in(*mode) {
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

/// Spawns a sibling list of nodes with an insertion slot above the first and
/// below every node.
///
/// The list is wrapped in a padded container so that the slot below a nested
/// list's last node does not sit flush against the slot below the node that
/// owns the list.
fn spawn_list<M: TreeModel>(
    parent: &mut ChildSpawnerCommands,
    nodes: &[TreeNode<M>],
    depth: usize,
    selected: Option<&M>,
) {
    let Some(first) = nodes.first() else {
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
            spawn_slot::<M>(list, first.address.clone(), DropKind::Before, depth);

            for node in nodes {
                spawn_container::<M>(list, node, depth, selected);
                spawn_slot::<M>(list, node.address.clone(), DropKind::After, depth);
            }
        });
}

/// Spawns one tree node: a selectable header row, its leaves, and a nested
/// container for its children.
///
/// Grouping each node with its contents means the parent's `After` slot, which
/// is spawned by the surrounding list, lands below the whole subtree.
fn spawn_container<M: TreeModel>(
    parent: &mut ChildSpawnerCommands,
    node: &TreeNode<M>,
    depth: usize,
    selected: Option<&M>,
) {
    let address = node.address.clone();
    let is_selected = selected == Some(&address);

    parent
        .spawn(Node {
            width: Val::Percent(100.0),
            flex_direction: FlexDirection::Column,
            ..default()
        })
        .with_children(|container| {
            spawn_row::<M>(container, &node.label, depth, address, is_selected);

            // Leaves come first, directly beneath the node. A slot above the
            // first leaf and one below every leaf reorder them (and move them
            // between nodes).
            if let Some(first) = node.leaves.first() {
                spawn_slot::<M>(
                    container,
                    first.address.clone(),
                    DropKind::Before,
                    depth + 1,
                );
            }
            for leaf in &node.leaves {
                let leaf_selected = selected == Some(&leaf.address);
                spawn_row::<M>(
                    container,
                    &leaf.label,
                    depth + 1,
                    leaf.address.clone(),
                    leaf_selected,
                );
                spawn_slot::<M>(container, leaf.address.clone(), DropKind::After, depth + 1);
            }

            // Then the child nodes, one level deeper.
            spawn_list::<M>(container, &node.children, depth + 1, selected);
        });
}

fn spawn_row<M: TreeModel>(
    parent: &mut ChildSpawnerCommands,
    label: &str,
    depth: usize,
    address: M,
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
                TreeRowLabel { address },
            ));
        });
}

/// Spawns an insertion slot between two rows.
///
/// The slot keeps a fixed height so the layout does not jump when the
/// indicator appears; only the line inside it is toggled.
fn spawn_slot<M: TreeModel>(
    parent: &mut ChildSpawnerCommands,
    target: M,
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
                TreeDropSlot {
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
                TreeSlotLine { target, kind },
            ));
        });
}

// ---------------------------------------------------------------------------
// Renaming
// ---------------------------------------------------------------------------

/// Types into, commits, or cancels the row being renamed.
pub fn handle_rename_input<M: TreeModel>(
    mut events: MessageReader<KeyboardInput>,
    mut editor: ResMut<EditorState>,
) {
    if M::rename_buffer_mut(&mut editor).is_none() {
        for _ in events.read() {}
        return;
    }

    let mut commit = false;
    let mut cancel = false;
    {
        let Some(buffer) = M::rename_buffer_mut(&mut editor) else {
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
                    buffer.pop();
                }
                _ => {
                    if let Some(text) = &event.text {
                        for ch in text.chars() {
                            if !ch.is_control() {
                                buffer.push(ch);
                            }
                        }
                    }
                }
            }
        }
    }

    if cancel {
        M::cancel_rename(&mut editor);
    } else if commit {
        M::commit_rename(&mut editor);
    }
}

// ---------------------------------------------------------------------------
// Selection and dragging
// ---------------------------------------------------------------------------

/// Clears the selection when the panel is pressed away from its rows.
///
/// Presses on a row are left to [`handle_row_press`]. A press anywhere else in
/// the sidebar strip commits any in-progress rename and deselects, so clicking
/// the empty part of the tree clears everything.
pub fn deselect_on_empty_press<M: TreeModel>(
    mode: Res<EditorMode>,
    mouse: Res<ButtonInput<MouseButton>>,
    windows: Query<&Window, With<PrimaryWindow>>,
    rows: Query<&RelativeCursorPosition, With<TreeRow<M>>>,
    mut editor: ResMut<EditorState>,
) {
    if !M::visible_in(*mode) || !mouse.just_pressed(MouseButton::Left) {
        return;
    }
    let Ok(window) = windows.single() else {
        return;
    };
    let Some(cursor) = window.cursor_position() else {
        return;
    };

    // Only the sidebar strip: toolbar, tab bar, and bottom panel are elsewhere.
    if cursor.x < window.width() - SIDEBAR_WIDTH || cursor.y < CONTENT_TOP {
        return;
    }
    // A row under the cursor handles its own press.
    if rows.iter().any(|row| row.cursor_over()) {
        return;
    }

    M::commit_rename(&mut editor);
    M::deselect(&mut editor);
}

/// Selects a row on press and starts dragging, or begins a rename on the
/// second press of a double-click.
pub fn handle_row_press<M: TreeModel>(
    mode: Res<EditorMode>,
    rows: Query<(&Interaction, &TreeRow<M>), Changed<Interaction>>,
    mouse: Res<ButtonInput<MouseButton>>,
    time: Res<Time>,
    windows: Query<&Window, With<PrimaryWindow>>,
    mut drag: ResMut<TreeDrag<M>>,
    mut editor: ResMut<EditorState>,
) {
    if !M::visible_in(*mode) || !mouse.just_pressed(MouseButton::Left) {
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
            M::begin_rename(&mut editor, address);
            return;
        }

        drag.last_click = Some((now, cursor, address.clone()));
        drag.source = Some(address.clone());
        drag.origin = cursor;
        drag.dragging = false;
        drag.hovered = None;
        M::select(&mut editor, address);
        return;
    }
}

/// Tracks how far the pointer has moved and which slot or row it is over.
pub fn update_drag_hover<M: TreeModel>(
    mode: Res<EditorMode>,
    mouse: Res<ButtonInput<MouseButton>>,
    windows: Query<&Window, With<PrimaryWindow>>,
    editor: Res<EditorState>,
    mut drag: ResMut<TreeDrag<M>>,
    slots: Query<(&TreeDropSlot<M>, &RelativeCursorPosition)>,
    rows: Query<(&TreeRow<M>, &RelativeCursorPosition)>,
) {
    let Some(source) = drag.source.clone() else {
        return;
    };
    if !M::visible_in(*mode) {
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

    // Slots are separate nodes between the rows, so at most one is under the
    // cursor. Adjacent slots can overlap near a list boundary where only one of
    // them accepts the dragged kind, so skip the ones that refuse the drop
    // instead of stopping there.
    let resolved = slots
        .iter()
        .filter(|(_, relative)| relative.cursor_over())
        .find_map(|(slot, _)| M::resolve_drop(&editor.document, &source, &slot.target, slot.kind));
    if let Some(resolved) = resolved {
        drag.hovered = Some(resolved);
        return;
    }

    // Outside a slot, the row under the pointer means "inside it".
    for (row, relative) in &rows {
        if relative.cursor_over()
            && let Some(resolved) =
                M::resolve_drop(&editor.document, &source, &row.address, DropKind::Into)
        {
            drag.hovered = Some(resolved);
            return;
        }
    }
}

/// Applies the drop when the mouse is released after a drag.
pub fn finish_drag<M: TreeModel>(
    mode: Res<EditorMode>,
    mouse: Res<ButtonInput<MouseButton>>,
    mut drag: ResMut<TreeDrag<M>>,
    mut editor: ResMut<EditorState>,
) {
    if !mouse.just_released(MouseButton::Left) {
        return;
    }

    let source = drag.source.take();
    let hovered = drag.hovered.take();
    let dragging = std::mem::replace(&mut drag.dragging, false);
    if !M::visible_in(*mode) || !dragging {
        return;
    }
    let (Some(source), Some((target, kind))) = (source, hovered) else {
        return;
    };
    if let Some(address) = M::drop_move(&mut editor.document, &source, &target, kind) {
        M::select(&mut editor, address);
        editor.structure_revision += 1;
    }
}

// ---------------------------------------------------------------------------
// Rebuilding and painting
// ---------------------------------------------------------------------------

/// Rebuilds the tree whenever the hierarchy, selection, or rename target
/// changes.
///
/// Ordinary edits (such as dragging a point) do not change the tree, and
/// typing while renaming only updates labels, so neither rebuilds rows.
pub fn rebuild_tree<M: TreeModel>(
    mut commands: Commands,
    editor: Res<EditorState>,
    tree: Query<Entity, With<TreeContainer<M>>>,
    mut last: Local<Option<(u64, Option<M>, Option<M>)>>,
) {
    let signature = (
        editor.structure_revision,
        M::selected(&editor).cloned(),
        M::rename_target(&editor).cloned(),
    );
    if last.as_ref() == Some(&signature) {
        return;
    }
    *last = Some(signature);

    let Ok(tree) = tree.single() else {
        return;
    };
    let layout = M::layout(&editor.document);
    let selected = M::selected(&editor).cloned();
    commands.entity(tree).despawn_children();
    commands.entity(tree).with_children(|parent| {
        spawn_list::<M>(parent, &layout, 0, selected.as_ref());
    });
}

/// Keeps row labels in step with names and the rename buffer.
pub fn sync_row_labels<M: TreeModel>(
    editor: Res<EditorState>,
    mut labels: Query<(&TreeRowLabel<M>, &mut Text)>,
) {
    let target = M::rename_target(&editor);
    let buffer = M::rename_buffer(&editor);
    for (label, mut text) in &mut labels {
        let value = match (target, buffer) {
            (Some(target), Some(buffer)) if target == &label.address => format!("{buffer}|"),
            _ => M::name(&editor.document, &label.address),
        };
        if text.0 != value {
            text.0 = value;
        }
    }
}

/// Colors rows and shows the drop indicator for the hovered zone.
pub fn paint_rows<M: TreeModel>(
    drag: Res<TreeDrag<M>>,
    editor: Res<EditorState>,
    mut rows: Query<(&TreeRow<M>, &Interaction, &mut BackgroundColor)>,
    mut lines: Query<(&TreeSlotLine<M>, &mut Node)>,
) {
    for (row, interaction, mut background) in &mut rows {
        let selected = M::selected(&editor) == Some(&row.address);
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

/// A display label, falling back to `fallback` when the name is empty.
pub fn label(name: &str, fallback: &str) -> String {
    if name.is_empty() {
        fallback.to_string()
    } else {
        name.to_string()
    }
}
