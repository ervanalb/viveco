//! Right-hand skeleton tree for rig mode.
//!
//! Mirrors the object tree: rows select bones, pressing and dragging reparents
//! or reorders them, and double-clicking starts an inline rename. The graphic's
//! object tree is hidden while this one is shown.
//!
//! The rows, drag-and-drop plumbing, and repaint logic come from the shared
//! [`crate::tree`] widget; this module only supplies the bone hierarchy and the
//! rules for moving bones within it.
//!
//! Bones have no paint order, so rows are listed in vector order and the
//! `Before`/`After` slots simply reorder siblings.

use bevy::prelude::*;

use crate::editor::{CanvasInput, EditorMode, EditorState};
use crate::model::{Bone, Document};
use crate::rig::{
    BoneAddress, address_is_inside, begin_bone_rename, bone_name_at, cancel_bone_rename,
    commit_bone_rename, move_bone_into, move_bone_near,
};
use crate::tree::{
    DropKind, TreeDrag, TreeModel, TreeNode, finish_drag, handle_row_press, label, paint_rows,
    rebuild_tree, spawn_panel, sync_row_labels, sync_visibility, update_drag_hover,
};

pub struct RigSidebarPlugin;

impl Plugin for RigSidebarPlugin {
    fn build(&self, app: &mut App) {
        // Rename input is registered by `RigPlugin`, after bone input, so that
        // the keyboard still behaves as the bone editor expects.
        app.init_resource::<TreeDrag<BoneAddress>>()
            .add_systems(Startup, spawn_panel::<BoneAddress>)
            .add_systems(
                Update,
                (
                    handle_row_press::<BoneAddress>,
                    update_drag_hover::<BoneAddress>,
                    finish_drag::<BoneAddress>,
                    rebuild_tree::<BoneAddress>,
                    sync_row_labels::<BoneAddress>,
                    paint_rows::<BoneAddress>,
                    sync_visibility::<BoneAddress>,
                )
                    .chain()
                    .after(CanvasInput),
            );
    }
}

impl TreeModel for BoneAddress {
    fn heading() -> &'static str {
        "Skeleton"
    }

    fn visible_in(mode: EditorMode) -> bool {
        mode == EditorMode::Rig
    }

    fn layout(document: &Document) -> Vec<TreeNode<Self>> {
        bone_nodes(&document.bones, &[])
    }

    fn name(document: &Document, address: &Self) -> String {
        let name = bone_name_at(document, address).unwrap_or_default();
        label(name, "Bone")
    }

    fn selected(editor: &EditorState) -> Option<&Self> {
        editor.selected_bone.as_ref()
    }

    fn select(editor: &mut EditorState, address: Self) {
        editor.select_bone(Some(address));
    }

    fn begin_rename(editor: &mut EditorState, address: Self) {
        begin_bone_rename(editor, address);
    }

    fn rename_target(editor: &EditorState) -> Option<&Self> {
        editor.bone_renaming.as_ref().map(|rename| &rename.address)
    }

    fn rename_buffer(editor: &EditorState) -> Option<&str> {
        editor
            .bone_renaming
            .as_ref()
            .map(|rename| rename.buffer.as_str())
    }

    fn rename_buffer_mut(editor: &mut EditorState) -> Option<&mut String> {
        editor
            .bone_renaming
            .as_mut()
            .map(|rename| &mut rename.buffer)
    }

    fn commit_rename(editor: &mut EditorState) {
        commit_bone_rename(editor);
    }

    fn cancel_rename(editor: &mut EditorState) {
        cancel_bone_rename(editor);
    }

    /// Any bone can be moved next to or inside another bone, as long as it is
    /// not dropped on itself or into its own subtree.
    fn resolve_drop(source: &Self, target: &Self, kind: DropKind) -> Option<(Self, DropKind)> {
        let allowed = !address_is_inside(source, target);
        (allowed && source != target).then(|| (target.clone(), kind))
    }

    fn drop_move(
        document: &mut Document,
        source: &Self,
        target: &Self,
        kind: DropKind,
    ) -> Option<Self> {
        match kind {
            DropKind::Before => move_bone_near(document, source, target, false),
            DropKind::After => move_bone_near(document, source, target, true),
            DropKind::Into => move_bone_into(document, source, target),
        }
    }
}

/// Builds the render layout for a sibling list of bones.
fn bone_nodes(bones: &[Bone], parents: &[usize]) -> Vec<TreeNode<BoneAddress>> {
    bones
        .iter()
        .enumerate()
        .map(|(index, bone)| {
            let mut chain = parents.to_vec();
            chain.push(index);
            TreeNode {
                address: BoneAddress {
                    bones: chain.clone(),
                },
                label: bone_label(bone),
                leaves: Vec::new(),
                children: bone_nodes(&bone.children, &chain),
            }
        })
        .collect()
}

fn bone_label(bone: &Bone) -> String {
    label(&bone.name, "Bone")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::FORMAT_VERSION;

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

    /// Applies a drop the way [`finish_drag`] does.
    fn apply_drop(
        editor: &mut EditorState,
        source: &BoneAddress,
        target: &BoneAddress,
        kind: DropKind,
    ) {
        if let Some(address) = BoneAddress::drop_move(&mut editor.document, source, target, kind) {
            editor.select_bone(Some(address));
            editor.structure_revision += 1;
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
