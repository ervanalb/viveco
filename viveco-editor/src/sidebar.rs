//! Right-hand object tree.
//!
//! Shows the document hierarchy (shapes, their paths, and nested child shapes)
//! and lets the user select, reorder, reparent, and rename nodes. Selection
//! lives in [`EditorState`], the same resource the canvas uses, so the tree and
//! the canvas stay in sync.
//!
//! The rows, drag-and-drop plumbing, and repaint logic come from the shared
//! [`crate::tree`] widget; this module only supplies the shape/path hierarchy
//! and the rules for moving nodes within it.
//!
//! Sibling rows are listed in paint order, so a row below its siblings is drawn
//! on top of them and a parent's row sits above the rows of its children.
//!
//! Only sibling shapes can be reordered, since Z only orders shapes among their
//! siblings. Paths are shown under their owning shape: slots between path rows
//! reorder them within that shape, and dropping one on another shape's row
//! moves it into that shape.

use bevy::prelude::*;

use crate::editor::{
    CanvasInput, EditorMode, EditorState, TreeAddress, address_is_inside, cancel_node_rename,
    commit_node_rename, move_path_into_shape, move_path_near, move_shape_into, move_shape_near,
    node_name_at,
};
use crate::tree::{
    DropKind, TreeDrag, TreeLeaf, TreeModel, TreeNode, deselect_on_empty_press, finish_drag,
    handle_rename_input, handle_row_press, label, paint_rows, rebuild_tree, spawn_panel,
    sync_row_labels, sync_visibility, update_drag_hover,
};
use viveco::model::{Document, Path, Shape, indices_in_paint_order};

pub struct SidebarPlugin;

impl Plugin for SidebarPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<TreeDrag<TreeAddress>>()
            .add_systems(Startup, spawn_panel::<TreeAddress>)
            .add_systems(
                Update,
                (
                    handle_rename_input::<TreeAddress>,
                    handle_row_press::<TreeAddress>,
                    deselect_on_empty_press::<TreeAddress>,
                    update_drag_hover::<TreeAddress>,
                    finish_drag::<TreeAddress>,
                    rebuild_tree::<TreeAddress>,
                    sync_row_labels::<TreeAddress>,
                    paint_rows::<TreeAddress>,
                    sync_visibility::<TreeAddress>,
                )
                    .chain()
                    // Canvas input runs first so it still sees a rename as
                    // active during the click that commits it.
                    .after(CanvasInput),
            );
    }
}

impl TreeModel for TreeAddress {
    fn heading() -> &'static str {
        "Objects"
    }

    fn visible_in(mode: EditorMode) -> bool {
        mode == EditorMode::Draw
    }

    fn layout(document: &Document) -> Vec<TreeNode<Self>> {
        shape_nodes(
            &document.shapes,
            &indices_in_paint_order(&document.shapes),
            &[],
        )
    }

    fn name(document: &Document, address: &Self) -> String {
        let name = node_name_at(document, address).unwrap_or_default();
        match address {
            TreeAddress::Shape { .. } => label(name, "Shape"),
            TreeAddress::Path { .. } => label(name, "Path"),
        }
    }

    fn selected(editor: &EditorState) -> Option<&Self> {
        editor.selected_node.as_ref()
    }

    fn select(editor: &mut EditorState, address: Self) {
        editor.select_node(Some(address));
    }

    fn deselect(editor: &mut EditorState) {
        editor.select_node(None);
    }

    fn begin_rename(editor: &mut EditorState, address: Self) {
        editor.begin_node_rename(address);
    }

    fn rename_target(editor: &EditorState) -> Option<&Self> {
        editor.renaming.as_ref().map(|rename| &rename.address)
    }

    fn rename_buffer(editor: &EditorState) -> Option<&str> {
        editor
            .renaming
            .as_ref()
            .map(|rename| rename.buffer.as_str())
    }

    fn rename_buffer_mut(editor: &mut EditorState) -> Option<&mut String> {
        editor.renaming.as_mut().map(|rename| &mut rename.buffer)
    }

    fn commit_rename(editor: &mut EditorState) {
        commit_node_rename(editor);
    }

    fn cancel_rename(editor: &mut EditorState) {
        cancel_node_rename(editor);
    }

    /// Sibling shapes reorder among themselves or nest inside one another.
    /// Paths reorder within a shape, move next to a path in another shape, or
    /// move into a shape's row. A node can never be dropped on itself, into its
    /// own subtree, or into the shape that already owns it.
    fn resolve_drop(
        _document: &Document,
        source: &Self,
        target: &Self,
        kind: DropKind,
    ) -> Option<(Self, DropKind)> {
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

    fn drop_move(
        document: &mut Document,
        source: &Self,
        target: &Self,
        kind: DropKind,
    ) -> Option<Self> {
        match (source, target) {
            (TreeAddress::Shape { .. }, TreeAddress::Shape { .. }) => match kind {
                // Rows are listed in paint order, so a slot above the target
                // means a lower Z (before it in the vector) and a slot below
                // means a higher Z (after it).
                DropKind::Before => move_shape_near(document, source, target, false),
                DropKind::After => move_shape_near(document, source, target, true),
                DropKind::Into => move_shape_into(document, source, target),
            },
            (TreeAddress::Path { .. }, TreeAddress::Shape { .. }) if kind == DropKind::Into => {
                move_path_into_shape(document, source, target)
            }
            (TreeAddress::Path { .. }, TreeAddress::Path { .. }) => match kind {
                // Path rows are listed in vector order, so "before" is the
                // lower index and "after" is the higher one.
                DropKind::Before => move_path_near(document, source, target, false),
                DropKind::After => move_path_near(document, source, target, true),
                DropKind::Into => None,
            },
            _ => None,
        }
    }
}

/// Builds the render layout for a sibling list of shapes.
///
/// Rows are listed in paint order, so siblings further down the list are
/// painted on top of the ones above them. A parent's row sits above its
/// children's rows for the same reason: children paint over their parent.
fn shape_nodes(
    shapes: &[Shape],
    indices: &[usize],
    parents: &[usize],
) -> Vec<TreeNode<TreeAddress>> {
    indices
        .iter()
        .map(|&index| {
            let shape = &shapes[index];
            let mut chain = parents.to_vec();
            chain.push(index);
            let child_indices = indices_in_paint_order(&shape.children);
            TreeNode {
                address: TreeAddress::Shape {
                    shapes: chain.clone(),
                },
                label: shape_label(shape),
                leaves: shape
                    .paths
                    .iter()
                    .enumerate()
                    .map(|(path, contour)| TreeLeaf {
                        address: TreeAddress::Path {
                            shapes: chain.clone(),
                            path,
                        },
                        label: path_label(contour),
                    })
                    .collect(),
                children: shape_nodes(&shape.children, &child_indices, &chain),
            }
        })
        .collect()
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
    use viveco::model::FORMAT_VERSION;

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
            animations: Vec::new(),
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

    /// Applies a drop the way [`finish_drag`] does.
    fn apply_drop(
        editor: &mut EditorState,
        source: &TreeAddress,
        target: &TreeAddress,
        kind: DropKind,
    ) {
        if let Some(address) = TreeAddress::drop_move(&mut editor.document, source, target, kind) {
            editor.select_node(Some(address));
            editor.structure_revision += 1;
        }
    }

    /// Delegates to the model's drop policy for readability in tests.
    fn resolve_drop(
        source: &TreeAddress,
        target: &TreeAddress,
        kind: DropKind,
    ) -> Option<(TreeAddress, DropKind)> {
        TreeAddress::resolve_drop(&Document::new(), source, target, kind)
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
        assert_eq!(editor.selected_node, Some(path(&[1], 1)));
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
        assert_eq!(editor.selected_node, Some(path(&[0], 1)));
    }

    #[test]
    fn a_path_can_be_dropped_between_another_shapes_paths() {
        let mut editor = state(vec![paths_shape(&["A", "B"]), paths_shape(&["C"])]);

        // The slot below C takes B out of the first shape and puts it below C.
        apply_drop(&mut editor, &path(&[0], 1), &path(&[1], 0), DropKind::After);

        assert_eq!(path_names(&editor.document.shapes[0]), ["A"]);
        assert_eq!(path_names(&editor.document.shapes[1]), ["C", "B"]);
        assert_eq!(editor.selected_node, Some(path(&[1], 1)));
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
