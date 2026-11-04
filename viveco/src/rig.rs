//! Skeleton tree editing and bone manipulation.
//!
//! Bones are addressed by their flat pre-order [`BoneIndex`], the same index
//! that weights and animation keyframes store. Bones form a tree of absolute
//! start/end points; moving a bone rewrites the points of its whole subtree, so
//! the hierarchy stays rigid.
//!
//! Because add, remove, and reorder shift the flat indices of later bones, those
//! operations rewrite the document's weights and animation targets with the
//! remap helpers from [`crate::model`]. Anything else that keeps bone-indexed
//! data alongside a document (such as a [`Pose`]) must apply the same remap.
//!
//! These operations only touch the document object model. Transient poses come
//! from [`crate::pose`], and interactive editing gesture state belongs to the
//! application using this crate.

use crate::model::{Bone, BoneIndex, Document, Point, insertion_map, move_map, removal_map};
use crate::pose::{Pose, Similarity};

/// One of a bone's two draggable endpoints.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BoneEnd {
    /// The origin end.
    Head,
    /// The far end.
    Tail,
}

// ---------------------------------------------------------------------------
// Tree addressing
// ---------------------------------------------------------------------------

/// The parent and sibling position of the bone at `index`.
///
/// `None` when no bone has that index; a root bone yields `(None, position)`.
fn locate(document: &Document, index: BoneIndex) -> Option<(Option<BoneIndex>, usize)> {
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
    walk(&document.bones, None, &mut cursor, index)
}

/// The parent of the bone at `index`, or `None` for a root or missing bone.
pub fn parent_of(document: &Document, index: BoneIndex) -> Option<BoneIndex> {
    locate(document, index)?.0
}

/// Whether `ancestor` is `index` itself or one of its ancestors.
///
/// A bone's subtree occupies the flat indices
/// `ancestor..ancestor + subtree_len`, so this is a range test.
pub fn is_ancestor_or_self(document: &Document, ancestor: BoneIndex, index: BoneIndex) -> bool {
    document
        .bone_subtree_len(ancestor)
        .is_some_and(|len| (ancestor..ancestor + len).contains(&index))
}

fn take_bone(document: &mut Document, index: BoneIndex) -> Option<Bone> {
    let (parent, position) = locate(document, index)?;
    let siblings = match parent {
        Some(parent) => &mut document.bone_by_index_mut(parent)?.children,
        None => &mut document.bones,
    };
    (position < siblings.len()).then(|| siblings.remove(position))
}

/// Removes a bone, materializing an omitted head so its absolute geometry is
/// preserved once it is detached from its parent.
fn take_bone_materialized(document: &mut Document, index: BoneIndex) -> Option<Bone> {
    let parent = parent_tail(document, index);
    let mut bone = take_bone(document, index)?;
    if bone.head.is_none() {
        bone.head = Some(bone.head_with_parent(parent));
    }
    Some(bone)
}

/// The flat index of the bone at `position` among the children of `parent`
/// (`None` for the top level) in the document's current tree.
///
/// Used after an insertion to learn where the moved block landed, which is what
/// [`move_map`] expects as its target.
fn flat_index_at(
    document: &Document,
    parent: Option<BoneIndex>,
    position: usize,
) -> Option<BoneIndex> {
    match parent {
        None => {
            document.bones.get(position)?;
            Some(
                document.bones[..position]
                    .iter()
                    .map(Bone::subtree_len)
                    .sum(),
            )
        }
        Some(parent) => {
            let children = &document.bone_by_index(parent)?.children;
            children.get(position)?;
            let before: usize = children[..position].iter().map(Bone::subtree_len).sum();
            Some(parent + 1 + before)
        }
    }
}

/// The tail of the bone's parent, which an omitted head follows.
pub fn parent_tail(document: &Document, index: BoneIndex) -> Option<Point> {
    let parent = parent_of(document, index)?;
    Some(document.bone_by_index(parent)?.tail)
}

/// The document-space positions of a bone's head and tail, resolving an
/// omitted head to the parent's tail.
pub fn bone_endpoints(document: &Document, index: BoneIndex) -> Option<(Point, Point)> {
    let bone = document.bone_by_index(index)?;
    let head = bone.head_with_parent(parent_tail(document, index));
    Some((head, bone.tail))
}

// ---------------------------------------------------------------------------
// Pose transforms
// ---------------------------------------------------------------------------

/// The world transform of the bone at `target` under `pose`.
///
/// Each bone contributes its local pose about its rest head, composed on top of
/// its parent's world transform, so a child inherits everything above it.
fn world_at(document: &Document, pose: &Pose, target: BoneIndex) -> Similarity {
    fn walk(
        bones: &[Bone],
        parent_world: Similarity,
        parent_tail: Option<Point>,
        pose: &Pose,
        cursor: &mut usize,
        target: BoneIndex,
    ) -> Option<Similarity> {
        for bone in bones {
            let index = *cursor;
            *cursor += 1;
            let rest_head = bone.head_with_parent(parent_tail);
            let local = pose.get(index).local(rest_head, bone.head.is_none());
            let world = Similarity::compose(parent_world, local);
            if index == target {
                return Some(world);
            }
            if let Some(found) = walk(&bone.children, world, Some(bone.tail), pose, cursor, target)
            {
                return Some(found);
            }
        }
        None
    }

    let mut cursor = 0;
    walk(
        &document.bones,
        Similarity::IDENTITY,
        None,
        pose,
        &mut cursor,
        target,
    )
    .unwrap_or(Similarity::IDENTITY)
}

/// A bone's world transform under `pose`.
pub fn bone_world(document: &Document, pose: &Pose, index: BoneIndex) -> Similarity {
    world_at(document, pose, index)
}

/// The world transform of a bone's parent under `pose`, or the identity for a
/// root bone.
///
/// Document edits move rest geometry within the parent's frame, so this is the
/// transform that maps those rest-space changes into world space.
pub fn parent_world(document: &Document, pose: &Pose, index: BoneIndex) -> Similarity {
    match parent_of(document, index) {
        Some(parent) => world_at(document, pose, parent),
        None => Similarity::IDENTITY,
    }
}

// ---------------------------------------------------------------------------
// Geometry
// ---------------------------------------------------------------------------

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

/// Translates a bone's own endpoints. Connected children stay where they are.
/// Returns `false` when the index is stale.
pub fn translate_bone(document: &mut Document, index: BoneIndex, delta: Point) -> bool {
    // A press that has not moved yet must not detach anything: the initial
    // frame of a body drag has zero movement, and materializing connected
    // children's heads there would silently split every shared joint.
    if delta == [0.0, 0.0] {
        return document.bone_by_index(index).is_some();
    }
    let parent = parent_tail(document, index);
    let Some(bone) = document.bone_by_index_mut(index) else {
        return false;
    };
    // Detach a connected head so the bone keeps its shape as it moves instead
    // of being stretched back to its parent's tail.
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

// ---------------------------------------------------------------------------
// Creation and reparenting
// ---------------------------------------------------------------------------

/// A bone name not used anywhere in the rig, such as `Bone` or `Bone 2`.
pub fn unique_bone_name(document: &Document) -> String {
    fn collect(bones: &[Bone], used: &mut std::collections::HashSet<String>) {
        for bone in bones {
            used.insert(bone.name.clone());
            collect(&bone.children, used);
        }
    }

    let mut used = std::collections::HashSet::new();
    collect(&document.bones, &mut used);
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
pub fn add_bone(
    document: &mut Document,
    head: Point,
    tail: Point,
    name: impl Into<String>,
) -> BoneIndex {
    let index = document.bone_count();
    let mut bone = Bone::from_endpoints(head, tail);
    bone.name = name.into();
    document.bones.push(bone);
    index
}

/// Adds a bone as the last child of `parent` and returns its index.
///
/// The new bone lands inside the parent's subtree, so every later bone shifts
/// up by one; this rewrites the document's weights and animation targets to
/// match. A [`Pose`] kept alongside the document must be remapped with the same
/// [`insertion_map`], whose insertion point is the position right after the
/// parent's subtree.
pub fn add_child_bone(
    document: &mut Document,
    parent: BoneIndex,
    head: Point,
    tail: Point,
    name: impl Into<String>,
) -> Option<BoneIndex> {
    let start = parent + document.bone_subtree_len(parent)?;
    let count = document.bone_count();

    let parent_tail = document.bone_by_index(parent)?.tail;
    let mut bone = Bone::from_endpoints(head, tail);
    // A bone that starts where its parent ends stays connected without
    // storing its own head.
    if head == parent_tail {
        bone.head = None;
    }
    bone.name = name.into();
    document.bone_by_index_mut(parent)?.children.push(bone);

    document.apply_bone_map(&insertion_map(count, start, 1));
    Some(start)
}

/// Removes the bone at `index`, together with its children.
///
/// Every removed bone is also dropped from the point weight lists, which are
/// rescaled so each still totals what it did before, and its animation
/// keyframes are dropped. Bones after the removed subtree shift down; the
/// document's references are rewritten to match. A [`Pose`] kept alongside the
/// document must be remapped with the same [`removal_map`].
pub fn remove_bone(document: &mut Document, index: BoneIndex) -> bool {
    let Some(len) = document.bone_subtree_len(index) else {
        return false;
    };
    let count = document.bone_count();
    if take_bone(document, index).is_none() {
        return false;
    }
    document.apply_bone_map(&removal_map(count, index, len));
    true
}

/// Moves `source` next to `target` in the target's sibling list.
///
/// Points are absolute, so reordering never moves the rig. `after` inserts the
/// source below the target in the list. Returns the bone's new index, or `None`
/// when either index is invalid or the move would put a bone inside itself.
pub fn move_bone_near(
    document: &mut Document,
    source: BoneIndex,
    target: BoneIndex,
    after: bool,
) -> Option<BoneIndex> {
    if source == target || is_ancestor_or_self(document, source, target) {
        return None;
    }
    let count = document.bone_count();
    let len = document.bone_subtree_len(source)?;
    document.bone_subtree_len(target)?;
    // Where the target sits once the source has been lifted out.
    let target_after_removal = removal_map(count, source, len)
        .get(target)
        .copied()
        .flatten()?;

    let bone = take_bone_materialized(document, source)?;
    let (parent, position) = locate(document, target_after_removal)?;

    let parent_tail = parent
        .and_then(|parent| document.bone_by_index(parent))
        .map(|bone| bone.tail);
    let siblings = match parent {
        Some(parent) => &mut document.bone_by_index_mut(parent)?.children,
        None => &mut document.bones,
    };
    let index = (position + usize::from(after)).min(siblings.len());
    siblings.insert(index, bone);
    // Reordering a connected child materializes its head; drop it again so the
    // bone keeps sharing the parent's tail joint.
    if let Some(bone) = siblings.get_mut(index) {
        omit_coincident_head(bone, parent_tail);
    }

    // The block's index in the post-removal tree is what `move_map` expects.
    let landed = flat_index_at(document, parent, index)?;
    document.apply_bone_map(&move_map(count, source, len, landed));
    Some(landed)
}

/// Parents `source` to `target` as its last child.
pub fn move_bone_into(
    document: &mut Document,
    source: BoneIndex,
    target: BoneIndex,
) -> Option<BoneIndex> {
    if source == target || is_ancestor_or_self(document, source, target) {
        return None;
    }
    let count = document.bone_count();
    let len = document.bone_subtree_len(source)?;
    document.bone_subtree_len(target)?;
    let target_after_removal = removal_map(count, source, len)
        .get(target)
        .copied()
        .flatten()?;

    let bone = take_bone_materialized(document, source)?;

    let parent_tail = document.bone_by_index(target_after_removal)?.tail;
    let children = &mut document.bone_by_index_mut(target_after_removal)?.children;
    let index = children.len();
    children.push(bone);
    // Dropping a connected child onto its own parent must not turn the shared
    // joint into a stored head that later drifts away.
    if let Some(child) = children.get_mut(index) {
        omit_coincident_head(child, Some(parent_tail));
    }

    let landed = flat_index_at(document, Some(target_after_removal), index)?;
    document.apply_bone_map(&move_map(count, source, len, landed));
    Some(landed)
}

/// The editable name of a bone.
pub fn bone_name_at(document: &Document, index: BoneIndex) -> Option<&str> {
    Some(document.bone_by_index(index)?.name.as_str())
}

/// Renames a bone.
///
/// Names are descriptive only: weights and animation targets refer to bones by
/// index, so renaming does not touch them. Returns `false` when the bone does
/// not exist.
pub fn rename_bone_at(document: &mut Document, index: BoneIndex, name: String) -> bool {
    let Some(bone) = document.bone_by_index_mut(index) else {
        return false;
    };
    bone.name = name;
    true
}

// ---------------------------------------------------------------------------
// Endpoint editing
// ---------------------------------------------------------------------------

/// Moves one endpoint of a bone to `world_point` while a drag is live.
///
/// The edit is local to the dragged bone: its body and the other endpoints of
/// the rig keep their rest geometry. Children connected at the moved joint
/// stay attached: an omitted head already follows the parent's tail, and a
/// stored head resting on the old tail is folded back into an omitted head so
/// the shared joint does not come apart. Their fixed pose pivots keep the
/// bones' own far endpoints where they were.
/// Snapping and reparenting happen when the drag is released.
pub fn drag_bone_endpoint(
    document: &mut Document,
    index: BoneIndex,
    end: BoneEnd,
    world_point: Point,
) -> bool {
    let Some(bone) = document.bone_by_index_mut(index) else {
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
            // A child whose stored head sits on the old tail shares this joint
            // with its parent, so it must follow the tail rather than be left
            // behind as a separate handle.
            for child in &mut bone.children {
                if child.head == Some(old_tail) {
                    child.head = None;
                }
            }
        }
    }
    true
}
