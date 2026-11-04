//! Skeleton tree addressing and bone manipulation.
//!
//! A [`BoneAddress`] names a bone by the chain of child indices leading to it.
//! Bones form a tree of absolute start/end points; moving a bone rewrites the
//! points of its whole subtree, so the hierarchy stays rigid.
//!
//! These operations only touch the document object model. Transient poses come
//! from [`crate::pose`], and interactive editing gesture state belongs to the
//! application using this crate.

use crate::model::{Bone, Document, Point};
use crate::pose::{Pose, Similarity};

/// A node in the skeleton tree, addressed by a chain of bone indices.
///
/// `[i]` addresses `document.bones[i]`, `[i, j]` addresses its `children[j]`,
/// and so on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BoneAddress {
    pub bones: Vec<usize>,
}

impl BoneAddress {
    /// The chain of indices leading to this bone.
    pub fn chain(&self) -> &[usize] {
        &self.bones
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
// Tree addressing
// ---------------------------------------------------------------------------

fn bones_at<'a>(mut bones: &'a [Bone], indices: &[usize]) -> Option<&'a [Bone]> {
    for &index in indices {
        bones = &bones.get(index)?.children;
    }
    Some(bones)
}

/// The mutable list containing the bones at the end of `parents`.
fn bone_list_at_mut<'a>(
    document: &'a mut Document,
    parents: &[usize],
) -> Option<&'a mut Vec<Bone>> {
    let mut bones = &mut document.bones;
    for &index in parents {
        bones = &mut bones.get_mut(index)?.children;
    }
    Some(bones)
}

/// Resolves a chain of bone indices to the bone it names.
pub fn bone_at<'a>(document: &'a Document, address: &BoneAddress) -> Option<&'a Bone> {
    let (last, parents) = address.chain().split_last()?;
    bones_at(&document.bones, parents)?.get(*last)
}

pub fn bone_at_mut<'a>(document: &'a mut Document, address: &BoneAddress) -> Option<&'a mut Bone> {
    let (last, parents) = address.chain().split_last()?;
    let mut bones = &mut document.bones;
    for &index in parents {
        bones = &mut bones.get_mut(index)?.children;
    }
    bones.get_mut(*last)
}

/// A bone's position in the tree: the chain of indices down to the bone.
#[derive(Clone, Debug, PartialEq, Eq)]
struct BoneSlot {
    chain: Vec<usize>,
}

impl BoneSlot {
    fn parents(&self) -> &[usize] {
        &self.chain[..self.chain.len() - 1]
    }

    fn index(&self) -> usize {
        *self.chain.last().expect("a bone slot is never empty")
    }
}

fn bone_slot(document: &Document, address: &BoneAddress) -> Option<BoneSlot> {
    let (last, parents) = address.chain().split_last()?;
    let siblings = bones_at(&document.bones, parents)?;
    if *last >= siblings.len() {
        return None;
    }
    Some(BoneSlot {
        chain: address.chain().to_vec(),
    })
}

fn shift_slot_after_removal(mut slot: BoneSlot, removed: &BoneSlot) -> BoneSlot {
    let depth = removed.parents().len();
    if slot.chain.len() > depth
        && slot.chain[..depth] == removed.parents()[..]
        && slot.chain[depth] > removed.index()
    {
        slot.chain[depth] -= 1;
    }
    slot
}

/// Whether `outer` names a bone that is `inner` itself or one of its
/// ancestors.
pub fn address_is_inside(outer: &BoneAddress, inner: &BoneAddress) -> bool {
    let outer = outer.chain();
    let inner = inner.chain();
    inner.len() >= outer.len() && inner[..outer.len()] == outer[..]
}

fn take_bone(document: &mut Document, address: &BoneAddress) -> Option<Bone> {
    let slot = bone_slot(document, address)?;
    let siblings = bone_list_at_mut(document, slot.parents())?;
    if slot.index() >= siblings.len() {
        return None;
    }
    Some(siblings.remove(slot.index()))
}

/// Removes a bone, materializing an omitted head so its absolute geometry is
/// preserved once it is detached from its parent.
fn take_bone_materialized(document: &mut Document, address: &BoneAddress) -> Option<Bone> {
    let parent = parent_tail(document, address);
    let mut bone = take_bone(document, address)?;
    if bone.head.is_none() {
        bone.head = Some(bone.head_with_parent(parent));
    }
    Some(bone)
}

/// The tail of the bone's parent, which an omitted head follows.
pub fn parent_tail(document: &Document, address: &BoneAddress) -> Option<Point> {
    let (last, parents) = address.chain().split_last()?;
    let mut bones = &document.bones;
    let mut tail = None;
    for &index in parents {
        let bone = bones.get(index)?;
        tail = Some(bone.tail);
        bones = &bone.children;
    }
    bones.get(*last)?;
    tail
}

/// The document-space positions of a bone's head and tail, resolving an
/// omitted head to the parent's tail.
pub fn bone_endpoints(document: &Document, address: &BoneAddress) -> Option<(Point, Point)> {
    let bone = bone_at(document, address)?;
    let head = bone.head_with_parent(parent_tail(document, address));
    Some((head, bone.tail))
}

// ---------------------------------------------------------------------------
// Pose transforms
// ---------------------------------------------------------------------------

/// The world transform accumulated down the chain of bone indices `chain`
/// under `pose`.
///
/// Each bone contributes its local pose about its rest head, composed on top
/// of its parent's world transform, so a child inherits everything above it.
fn world_at(document: &Document, pose: &Pose, chain: &[usize]) -> Similarity {
    let mut world = Similarity::IDENTITY;
    let mut parent_tail = None;
    let mut bones = &document.bones;
    for &index in chain {
        let Some(bone) = bones.get(index) else {
            break;
        };
        let rest_head = bone.head_with_parent(parent_tail);
        let local = pose.get(&bone.name).local(rest_head, bone.head.is_none());
        world = Similarity::compose(world, local);
        parent_tail = Some(bone.tail);
        bones = &bone.children;
    }
    world
}

/// A bone's world transform under `pose`.
pub fn bone_world(document: &Document, pose: &Pose, address: &BoneAddress) -> Similarity {
    world_at(document, pose, address.chain())
}

/// The world transform of a bone's parent under `pose`, or the identity for a
/// root bone.
///
/// Document edits move rest geometry within the parent's frame, so this is the
/// transform that maps those rest-space changes into world space.
pub fn parent_world(document: &Document, pose: &Pose, address: &BoneAddress) -> Similarity {
    match address.chain().split_last() {
        Some((_, parents)) if !parents.is_empty() => world_at(document, pose, parents),
        _ => Similarity::IDENTITY,
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
/// Returns `false` when the address is stale.
pub fn translate_bone(document: &mut Document, address: &BoneAddress, delta: Point) -> bool {
    // A press that has not moved yet must not detach anything: the initial
    // frame of a body drag has zero movement, and materializing connected
    // children's heads there would silently split every shared joint.
    if delta == [0.0, 0.0] {
        return bone_at(document, address).is_some();
    }
    let parent = parent_tail(document, address);
    let Some(bone) = bone_at_mut(document, address) else {
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
///
/// Weight lists refer to bones by name, so names must stay unique across the
/// whole skeleton, not just among siblings.
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

/// Adds a new top-level bone and returns its address.
pub fn add_bone(
    document: &mut Document,
    head: Point,
    tail: Point,
    name: impl Into<String>,
) -> BoneAddress {
    let index = document.bones.len();
    let mut bone = Bone::from_endpoints(head, tail);
    bone.name = name.into();
    document.bones.push(bone);
    BoneAddress { bones: vec![index] }
}

/// Adds a bone as the last child of `parent` and returns its address.
pub fn add_child_bone(
    document: &mut Document,
    parent: &BoneAddress,
    head: Point,
    tail: Point,
    name: impl Into<String>,
) -> Option<BoneAddress> {
    let parent_tail = bone_at_mut(document, parent)?.tail;
    let index = bone_at_mut(document, parent)?.children.len();
    let mut bone = Bone::from_endpoints(head, tail);
    // A bone that starts where its parent ends stays connected without
    // storing its own head.
    if head == parent_tail {
        bone.head = None;
    }
    bone.name = name.into();
    bone_at_mut(document, parent)?.children.push(bone);
    let mut chain = parent.chain().to_vec();
    chain.push(index);
    Some(BoneAddress { bones: chain })
}

/// Removes the bone named by `address`, together with its children.
///
/// Every removed bone is also dropped from the point weight lists, which are
/// rescaled so each still totals what it did before.
pub fn remove_bone(document: &mut Document, address: &BoneAddress) -> bool {
    let Some(bone) = take_bone(document, address) else {
        return false;
    };
    let mut names = Vec::new();
    collect_bone_names(&bone, &mut names);
    document.remove_bones_from_weights(&names);
    true
}

fn collect_bone_names(bone: &Bone, names: &mut Vec<String>) {
    if !bone.name.is_empty() {
        names.push(bone.name.clone());
    }
    for child in &bone.children {
        collect_bone_names(child, names);
    }
}

/// Moves `source` next to `target` in the target's sibling list.
///
/// Points are absolute, so reordering never moves the rig. `after` inserts the
/// source below the target in the list. Returns the bone's new address, or
/// `None` when either address is invalid or the move would put a bone inside
/// itself.
pub fn move_bone_near(
    document: &mut Document,
    source: &BoneAddress,
    target: &BoneAddress,
    after: bool,
) -> Option<BoneAddress> {
    if source == target || address_is_inside(source, target) {
        return None;
    }
    let source_slot = bone_slot(document, source)?;
    let target_slot = bone_slot(document, target)?;

    let bone = take_bone_materialized(document, source)?;
    let target_slot = shift_slot_after_removal(target_slot, &source_slot);

    let parent_chain = target_slot.parents().to_vec();
    let parent_tail = if parent_chain.is_empty() {
        None
    } else {
        bone_at(
            document,
            &BoneAddress {
                bones: parent_chain.clone(),
            },
        )
        .map(|bone| bone.tail)
    };
    let siblings = bone_list_at_mut(document, &parent_chain)?;
    let index = (target_slot.index() + usize::from(after)).min(siblings.len());
    siblings.insert(index, bone);
    // Reordering a connected child materializes its head; drop it again so the
    // bone keeps sharing the parent's tail joint.
    if let Some(bone) = siblings.get_mut(index) {
        omit_coincident_head(bone, parent_tail);
    }

    let mut chain = parent_chain;
    chain.push(index);
    Some(BoneAddress { bones: chain })
}

/// Parents `source` to `target` as its last child.
pub fn move_bone_into(
    document: &mut Document,
    source: &BoneAddress,
    target: &BoneAddress,
) -> Option<BoneAddress> {
    if source == target || address_is_inside(source, target) {
        return None;
    }
    let source_slot = bone_slot(document, source)?;
    let target_slot = bone_slot(document, target)?;

    let bone = take_bone_materialized(document, source)?;
    let target_slot = shift_slot_after_removal(target_slot, &source_slot);

    let parent_chain = target_slot.chain.clone();
    let parent_tail = bone_at(
        document,
        &BoneAddress {
            bones: parent_chain.clone(),
        },
    )
    .map(|bone| bone.tail);
    let children = bone_list_at_mut(document, &parent_chain)?;
    let index = children.len();
    children.push(bone);
    // Dropping a connected child onto its own parent must not turn the shared
    // joint into a stored head that later drifts away.
    if let Some(child) = children.get_mut(index) {
        omit_coincident_head(child, parent_tail);
    }

    let mut chain = parent_chain;
    chain.push(index);
    Some(BoneAddress { bones: chain })
}

/// The editable name of a bone.
pub fn bone_name_at<'a>(document: &'a Document, address: &BoneAddress) -> Option<&'a str> {
    Some(bone_at(document, address)?.name.as_str())
}

/// Renames a bone, updating any weights and animation targets bound to it.
///
/// Returns `false` when the bone does not exist.
pub fn rename_bone_at(document: &mut Document, address: &BoneAddress, name: String) -> bool {
    let old = {
        let Some(bone) = bone_at_mut(document, address) else {
            return false;
        };
        std::mem::replace(&mut bone.name, name.clone())
    };
    if old != name {
        document.rename_bone_in_weights(&old, &name);
        document.rename_bone_in_animations(&old, &name);
    }
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
    address: &BoneAddress,
    end: BoneEnd,
    world_point: Point,
) -> bool {
    let Some(bone) = bone_at_mut(document, address) else {
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
