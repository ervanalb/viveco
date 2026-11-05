//! Skeleton trees and bone manipulation.
//!
//! Bones are addressed by their flat pre-order [`BoneIndex`], the same index
//! that weights and animation keyframes store. Bones form a tree of absolute
//! start/end points; moving a bone rewrites the points of its whole subtree, so
//! the hierarchy stays rigid.
//!
//! Because add, remove, and reorder shift the flat indices of later bones, those
//! operations rewrite the asset's weights and animation targets with the remap
//! helpers in this module. Anything else that keeps bone-indexed data alongside
//! an asset (such as a [`Pose`](crate::Pose)) must apply the same remap.
//!
//! These operations only touch the asset object model. Transient poses come
//! from [`crate::Pose`], and interactive editing gesture state belongs to the
//! application using this crate.

use serde::{Deserialize, Serialize};

use crate::asset::Asset;
use crate::shape::Point;

/// Flat index of a bone, in depth-first pre-order.
///
/// `0` is the first top-level bone, followed by its whole subtree, then the
/// next top-level bone, and so on. A bone's subtree therefore occupies a
/// contiguous range of indices. Every reference to a bone (weights and
/// animation targets) stores this index; inserting, removing, or reparenting a
/// bone rewrites the references of the whole asset.
pub type BoneIndex = usize;

/// A single bone in the rig's hierarchy.
///
/// A bone is the segment from `head` to `tail`, stored directly in asset
/// space. Children are stored the same way, so the hierarchy is purely
/// structural: moving a bone rewrites the endpoints of everything below it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Bone {
    /// Optional human-readable name, shown in the editor's skeleton tree.
    #[serde(default)]
    pub name: String,

    /// The origin end of the bone, in asset space.
    ///
    /// This may be omitted when the bone has a parent, in which case the head
    /// follows the parent's tail. A root bone has no parent, so its head is
    /// always present.
    #[serde(default)]
    pub head: Option<Point>,

    /// The far end of the bone, in asset space.
    #[serde(default)]
    pub tail: Point,

    /// Nested bones, painted relative to this one.
    #[serde(default)]
    pub children: Vec<Bone>,
}

impl Default for Bone {
    fn default() -> Self {
        Self {
            name: String::new(),
            head: None,
            tail: [0.0, 0.0],
            children: Vec::new(),
        }
    }
}

impl Bone {
    /// Builds a bone from asset-space endpoints.
    pub fn from_endpoints(head: Point, tail: Point) -> Self {
        Self {
            head: Some(head),
            tail,
            ..Default::default()
        }
    }

    /// The head to draw and edit with, resolving an omitted head to the
    /// parent's tail. Falls back to the tail for a root bone with no head.
    pub fn head_with_parent(&self, parent_tail: Option<Point>) -> Point {
        self.head.or(parent_tail).unwrap_or(self.tail)
    }

    /// The number of bones in this subtree, including the bone itself.
    pub fn subtree_len(&self) -> usize {
        1 + self.children.iter().map(Bone::subtree_len).sum::<usize>()
    }
}

/// One of a bone's two draggable endpoints.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BoneEnd {
    /// The origin end.
    Head,
    /// The far end.
    Tail,
}

// ---------------------------------------------------------------------------
// Index remaps
// ---------------------------------------------------------------------------

/// The bone index remap produced by inserting `len` new bones at flat index
/// `start`.
///
/// `count` is the number of bones before the insertion.
pub fn insertion_map(count: usize, start: BoneIndex, len: usize) -> Vec<Option<BoneIndex>> {
    (0..count)
        .map(|index| Some(if index < start { index } else { index + len }))
        .collect()
}

/// The bone index remap produced by deleting the `len` bones occupying flat
/// indices `start..start + len`.
///
/// `count` is the number of bones before the removal.
pub fn removal_map(count: usize, start: BoneIndex, len: usize) -> Vec<Option<BoneIndex>> {
    (0..count)
        .map(|index| {
            if index < start {
                Some(index)
            } else if index < start + len {
                None
            } else {
                Some(index - len)
            }
        })
        .collect()
}

/// The bone index remap produced by moving the `len` bones occupying flat
/// indices `start..start + len` to `target`.
///
/// `target` is the block's new index once it has been lifted out of the tree,
/// i.e. it is measured without the moved bones. `count` is the number of bones
/// before the move.
pub fn move_map(
    count: usize,
    start: BoneIndex,
    len: usize,
    target: BoneIndex,
) -> Vec<Option<BoneIndex>> {
    (0..count)
        .map(|index| {
            if index >= start && index < start + len {
                Some(target + (index - start))
            } else {
                let rest = if index < start { index } else { index - len };
                Some(if rest < target { rest } else { rest + len })
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Tree addressing
// ---------------------------------------------------------------------------

/// The parent and sibling position of the bone at `index`.
///
/// `None` when no bone has that index; a root bone yields `(None, position)`.
fn locate(asset: &Asset, index: BoneIndex) -> Option<(Option<BoneIndex>, usize)> {
    fn walk(
        bones: &[Bone],
        parent: Option<BoneIndex>,
        cursor: &mut usize,
        target: BoneIndex,
    ) -> Option<(Option<BoneIndex>, usize)> {
        for (position, bone) in bones.iter().enumerate() {
            let current = *cursor;
            *cursor += 1;
            if current == target {
                return Some((parent, position));
            }
            if let Some(found) = walk(&bone.children, Some(current), cursor, target) {
                return Some(found);
            }
        }
        None
    }

    let mut cursor = 0;
    walk(&asset.bones, None, &mut cursor, index)
}

fn take_bone(asset: &mut Asset, index: BoneIndex) -> Option<Bone> {
    let (parent, position) = locate(asset, index)?;
    let siblings = match parent {
        Some(parent) => &mut asset.bone_by_index_mut(parent)?.children,
        None => &mut asset.bones,
    };
    (position < siblings.len()).then(|| siblings.remove(position))
}

/// Removes a bone, materializing an omitted head so its absolute geometry is
/// preserved once it is detached from its parent.
fn take_bone_materialized(asset: &mut Asset, index: BoneIndex) -> Option<Bone> {
    let parent = asset.parent_tail(index);
    let mut bone = take_bone(asset, index)?;
    if bone.head.is_none() {
        bone.head = Some(bone.head_with_parent(parent));
    }
    Some(bone)
}

/// The flat index of the bone at `position` among the children of `parent`
/// (`None` for the top level) in the asset's current tree.
///
/// Used after an insertion to learn where the moved block landed, which is what
/// [`move_map`] expects as its target.
fn flat_index_at(asset: &Asset, parent: Option<BoneIndex>, position: usize) -> Option<BoneIndex> {
    match parent {
        None => {
            asset.bones.get(position)?;
            Some(asset.bones[..position].iter().map(Bone::subtree_len).sum())
        }
        Some(parent) => {
            let children = &asset.bone_by_index(parent)?.children;
            children.get(position)?;
            let before: usize = children[..position].iter().map(Bone::subtree_len).sum();
            Some(parent + 1 + before)
        }
    }
}

/// Pins any children connected to this bone's tail to their current absolute
/// head, so moving the tail does not drag them along.
///
/// Rig-mode edits are local to the bone being edited; a child that was
/// connected with an omitted head keeps its own geometry once the parent
/// moves.
fn detach_connected_children(bone: &mut Bone) {
    let tail = bone.tail;
    for child in &mut bone.children {
        if child.head.is_none() {
            child.head = Some(tail);
        }
    }
}

/// Clears a stored head that sits exactly on the parent's tail.
///
/// Such a bone is visually connected, so it shares the parent's tail joint.
/// Keeping the redundant head would make it a separate joint that comes apart
/// when the parent's tail moves.
fn omit_coincident_head(bone: &mut Bone, parent_tail: Option<Point>) {
    if parent_tail.is_some_and(|tail| bone.head == Some(tail)) {
        bone.head = None;
    }
}

impl Asset {
    /// The number of bones in the rig, counting descendants.
    pub fn bone_count(&self) -> usize {
        self.bones.iter().map(Bone::subtree_len).sum()
    }

    /// The bone with the given pre-order index, if any.
    pub fn bone_by_index(&self, index: BoneIndex) -> Option<&Bone> {
        fn find<'a>(bones: &'a [Bone], cursor: &mut usize, target: usize) -> Option<&'a Bone> {
            for bone in bones {
                let current = *cursor;
                *cursor += 1;
                if current == target {
                    return Some(bone);
                }
                if let Some(found) = find(&bone.children, cursor, target) {
                    return Some(found);
                }
            }
            None
        }
        let mut cursor = 0;
        find(&self.bones, &mut cursor, index)
    }

    /// The bone with the given pre-order index, if any.
    pub fn bone_by_index_mut(&mut self, index: BoneIndex) -> Option<&mut Bone> {
        fn find<'a>(
            bones: &'a mut [Bone],
            cursor: &mut usize,
            target: usize,
        ) -> Option<&'a mut Bone> {
            bones.iter_mut().find_map(|bone| {
                let current = *cursor;
                *cursor += 1;
                if current == target {
                    Some(bone)
                } else {
                    find(&mut bone.children, cursor, target)
                }
            })
        }
        let mut cursor = 0;
        find(&mut self.bones, &mut cursor, index)
    }

    /// The number of bones in the subtree rooted at `index`, including the
    /// bone itself.
    pub fn bone_subtree_len(&self, index: BoneIndex) -> Option<usize> {
        self.bone_by_index(index).map(Bone::subtree_len)
    }

    /// The pre-order index of the first bone named `name`.
    ///
    /// Names are descriptive and need not be unique, so this returns the first
    /// match in the same depth-first order as [`Self::bone_by_index`].
    pub fn bone_index_by_name(&self, name: &str) -> Option<BoneIndex> {
        fn find(bones: &[Bone], name: &str, cursor: &mut usize) -> Option<BoneIndex> {
            for bone in bones {
                let current = *cursor;
                *cursor += 1;
                if bone.name == name {
                    return Some(current);
                }
                if let Some(found) = find(&bone.children, name, cursor) {
                    return Some(found);
                }
            }
            None
        }

        let mut cursor = 0;
        find(&self.bones, name, &mut cursor)
    }

    /// The parent of the bone at `index`, or `None` for a root or missing bone.
    pub fn parent_of(&self, index: BoneIndex) -> Option<BoneIndex> {
        locate(self, index)?.0
    }

    /// Whether `ancestor` is `index` itself or one of its ancestors.
    ///
    /// A bone's subtree occupies the flat indices
    /// `ancestor..ancestor + subtree_len`, so this is a range test.
    pub fn is_ancestor_or_self(&self, ancestor: BoneIndex, index: BoneIndex) -> bool {
        self.bone_subtree_len(ancestor)
            .is_some_and(|len| (ancestor..ancestor + len).contains(&index))
    }

    /// The tail of the bone's parent, which an omitted head follows.
    pub fn parent_tail(&self, index: BoneIndex) -> Option<Point> {
        let parent = self.parent_of(index)?;
        Some(self.bone_by_index(parent)?.tail)
    }

    /// The asset-space positions of a bone's head and tail, resolving an
    /// omitted head to the parent's tail.
    pub fn bone_endpoints(&self, index: BoneIndex) -> Option<(Point, Point)> {
        let bone = self.bone_by_index(index)?;
        let head = bone.head_with_parent(self.parent_tail(index));
        Some((head, bone.tail))
    }

    /// Translates a bone's own endpoints. Connected children stay where they
    /// are. Returns `false` when the index is stale.
    pub fn translate_bone(&mut self, index: BoneIndex, delta: Point) -> bool {
        // A press that has not moved yet must not detach anything: the initial
        // frame of a body drag has zero movement, and materializing connected
        // children's heads there would silently split every shared joint.
        if delta == [0.0, 0.0] {
            return self.bone_by_index(index).is_some();
        }
        let parent = self.parent_tail(index);
        let Some(bone) = self.bone_by_index_mut(index) else {
            return false;
        };
        // Detach a connected head so the bone keeps its shape as it moves
        // instead of being stretched back to its parent's tail.
        if bone.head.is_none() {
            bone.head = Some(bone.head_with_parent(parent));
        }
        detach_connected_children(bone);
        if let Some(head) = bone.head.as_mut() {
            head[0] += delta[0];
            head[1] += delta[1];
        }
        bone.tail = [bone.tail[0] + delta[0], bone.tail[1] + delta[1]];
        true
    }

    /// A bone name not used anywhere in the rig, such as `Bone` or `Bone 2`.
    pub fn unique_bone_name(&self) -> String {
        fn collect(bones: &[Bone], used: &mut std::collections::HashSet<String>) {
            for bone in bones {
                used.insert(bone.name.clone());
                collect(&bone.children, used);
            }
        }

        let mut used = std::collections::HashSet::new();
        collect(&self.bones, &mut used);
        if !used.contains("Bone") {
            return "Bone".to_owned();
        }
        let mut suffix = 2;
        loop {
            let candidate = format!("Bone {suffix}");
            if !used.contains(&candidate) {
                return candidate;
            }
            suffix += 1;
        }
    }

    /// Adds a new top-level bone at the end of the rig and returns its index.
    pub fn add_bone(&mut self, head: Point, tail: Point, name: impl Into<String>) -> BoneIndex {
        let index = self.bone_count();
        let mut bone = Bone::from_endpoints(head, tail);
        bone.name = name.into();
        self.bones.push(bone);
        index
    }

    /// Adds a bone as the last child of `parent` and returns its index.
    ///
    /// The new bone lands inside the parent's subtree, so every later bone
    /// shifts up by one; this rewrites the asset's weights and animation
    /// targets to match. A [`Pose`](crate::Pose) kept alongside the asset must
    /// be remapped with the same [`insertion_map`], whose insertion point is
    /// the position right after the parent's subtree.
    pub fn add_child_bone(
        &mut self,
        parent: BoneIndex,
        head: Point,
        tail: Point,
        name: impl Into<String>,
    ) -> Option<BoneIndex> {
        let start = parent + self.bone_subtree_len(parent)?;
        let count = self.bone_count();

        let parent_tail = self.bone_by_index(parent)?.tail;
        let mut bone = Bone::from_endpoints(head, tail);
        // A bone that starts where its parent ends stays connected without
        // storing its own head.
        if head == parent_tail {
            bone.head = None;
        }
        bone.name = name.into();
        self.bone_by_index_mut(parent)?.children.push(bone);

        self.apply_bone_map(&insertion_map(count, start, 1));
        Some(start)
    }

    /// Removes the bone at `index`, together with its children.
    ///
    /// Every removed bone is also dropped from the point weight lists, which
    /// are rescaled so each still totals what it did before, and its animation
    /// keyframes are dropped. Bones after the removed subtree shift down; the
    /// asset's references are rewritten to match. A [`Pose`](crate::Pose) kept
    /// alongside the asset must be remapped with the same [`removal_map`].
    pub fn remove_bone(&mut self, index: BoneIndex) -> bool {
        let Some(len) = self.bone_subtree_len(index) else {
            return false;
        };
        let count = self.bone_count();
        if take_bone(self, index).is_none() {
            return false;
        }
        self.apply_bone_map(&removal_map(count, index, len));
        true
    }

    /// Moves `source` next to `target` in the target's sibling list.
    ///
    /// Points are absolute, so reordering never moves the rig. `after` inserts
    /// the source below the target in the list. Returns the bone's new index,
    /// or `None` when either index is invalid or the move would put a bone
    /// inside itself.
    pub fn move_bone_near(
        &mut self,
        source: BoneIndex,
        target: BoneIndex,
        after: bool,
    ) -> Option<BoneIndex> {
        if source == target || self.is_ancestor_or_self(source, target) {
            return None;
        }
        let count = self.bone_count();
        let len = self.bone_subtree_len(source)?;
        self.bone_subtree_len(target)?;
        // Where the target sits once the source has been lifted out.
        let target_after_removal = removal_map(count, source, len)
            .get(target)
            .copied()
            .flatten()?;

        let bone = take_bone_materialized(self, source)?;
        let (parent, position) = locate(self, target_after_removal)?;

        let parent_tail = parent
            .and_then(|parent| self.bone_by_index(parent))
            .map(|bone| bone.tail);
        let siblings = match parent {
            Some(parent) => &mut self.bone_by_index_mut(parent)?.children,
            None => &mut self.bones,
        };
        let index = (position + usize::from(after)).min(siblings.len());
        siblings.insert(index, bone);
        // Reordering a connected child materializes its head; drop it again so
        // the bone keeps sharing the parent's tail joint.
        if let Some(bone) = siblings.get_mut(index) {
            omit_coincident_head(bone, parent_tail);
        }

        // The block's index in the post-removal tree is what `move_map`
        // expects.
        let landed = flat_index_at(self, parent, index)?;
        self.apply_bone_map(&move_map(count, source, len, landed));
        Some(landed)
    }

    /// Parents `source` to `target` as its last child.
    pub fn move_bone_into(&mut self, source: BoneIndex, target: BoneIndex) -> Option<BoneIndex> {
        if source == target || self.is_ancestor_or_self(source, target) {
            return None;
        }
        let count = self.bone_count();
        let len = self.bone_subtree_len(source)?;
        self.bone_subtree_len(target)?;
        let target_after_removal = removal_map(count, source, len)
            .get(target)
            .copied()
            .flatten()?;

        let bone = take_bone_materialized(self, source)?;

        let parent_tail = self.bone_by_index(target_after_removal)?.tail;
        let children = &mut self.bone_by_index_mut(target_after_removal)?.children;
        let index = children.len();
        children.push(bone);
        // Dropping a connected child onto its own parent must not turn the
        // shared joint into a stored head that later drifts away.
        if let Some(child) = children.get_mut(index) {
            omit_coincident_head(child, Some(parent_tail));
        }

        let landed = flat_index_at(self, Some(target_after_removal), index)?;
        self.apply_bone_map(&move_map(count, source, len, landed));
        Some(landed)
    }

    /// The editable name of a bone.
    pub fn bone_name(&self, index: BoneIndex) -> Option<&str> {
        Some(self.bone_by_index(index)?.name.as_str())
    }

    /// Renames a bone.
    ///
    /// Names are descriptive only: weights and animation targets refer to
    /// bones by index, so renaming does not touch them. Returns `false` when
    /// the bone does not exist.
    pub fn rename_bone(&mut self, index: BoneIndex, name: String) -> bool {
        let Some(bone) = self.bone_by_index_mut(index) else {
            return false;
        };
        bone.name = name;
        true
    }

    /// Moves one endpoint of a bone to `world_point` while a drag is live.
    ///
    /// The edit is local to the dragged bone: its body and the other endpoints
    /// of the rig keep their rest geometry. Children connected at the moved
    /// joint stay attached: an omitted head already follows the parent's tail,
    /// and a stored head resting on the old tail is folded back into an omitted
    /// head so the shared joint does not come apart. Their fixed pose pivots
    /// keep the bones' own far endpoints where they were.
    /// Snapping and reparenting happen when the drag is released.
    pub fn drag_bone_endpoint(
        &mut self,
        index: BoneIndex,
        end: BoneEnd,
        world_point: Point,
    ) -> bool {
        let Some(bone) = self.bone_by_index_mut(index) else {
            return false;
        };
        match end {
            // An omitted head is not its own handle; dragging it does nothing.
            BoneEnd::Head => {
                if bone.head.is_some() {
                    bone.head = Some(world_point);
                }
            }
            BoneEnd::Tail => {
                let old_tail = bone.tail;
                bone.tail = world_point;
                // A child whose stored head sits on the old tail shares this
                // joint with its parent, so it must follow the tail rather than
                // be left behind as a separate handle.
                for child in &mut bone.children {
                    if child.head == Some(old_tail) {
                        child.head = None;
                    }
                }
            }
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pre_order_bone_indices_match_a_depth_first_walk() {
        let asset = Asset {
            bones: vec![
                Bone {
                    name: "A".into(),
                    children: vec![
                        Bone {
                            name: "A1".into(),
                            ..Default::default()
                        },
                        Bone {
                            name: "A2".into(),
                            children: vec![Bone {
                                name: "A2a".into(),
                                ..Default::default()
                            }],
                            ..Default::default()
                        },
                    ],
                    ..Default::default()
                },
                Bone {
                    name: "B".into(),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };

        let names: Vec<&str> = (0..asset.bone_count())
            .map(|index| asset.bone_by_index(index).unwrap().name.as_str())
            .collect();
        assert_eq!(names, vec!["A", "A1", "A2", "A2a", "B"]);
        assert_eq!(asset.bone_subtree_len(0), Some(4));
        assert_eq!(asset.bone_subtree_len(2), Some(2));
        assert_eq!(asset.bone_subtree_len(4), Some(1));
        assert_eq!(asset.bone_by_index(5), None);
    }

    #[test]
    fn bone_index_maps_reposition_the_expected_bones() {
        // Moving bones 1..3 to the front of a five-bone rig.
        assert_eq!(
            move_map(5, 1, 2, 0),
            vec![Some(2), Some(0), Some(1), Some(3), Some(4)]
        );
        // Moving bones 0..2 to the end.
        assert_eq!(
            move_map(5, 0, 2, 3),
            vec![Some(3), Some(4), Some(0), Some(1), Some(2)]
        );
    }

    #[test]
    fn bones_are_found_by_name_in_pre_order() {
        let asset = Asset {
            bones: vec![
                Bone {
                    name: "Root".into(),
                    children: vec![Bone {
                        name: "Child".into(),
                        ..Default::default()
                    }],
                    ..Default::default()
                },
                Bone {
                    name: "Sibling".into(),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };

        assert_eq!(asset.bone_index_by_name("Root"), Some(0));
        assert_eq!(asset.bone_index_by_name("Child"), Some(1));
        assert_eq!(asset.bone_index_by_name("Sibling"), Some(2));
        assert_eq!(asset.bone_index_by_name("Missing"), None);
    }
}
