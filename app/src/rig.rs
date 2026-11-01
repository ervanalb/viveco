//! Rig mode: skeleton model operations, bone editing, and overlay drawing.
//!
//! A [`BoneAddress`] names a bone by the chain of child indices leading to it,
//! mirroring how [`crate::editor::TreeAddress`] names shapes. Bones form a tree
//! of absolute start/end points; moving a bone rewrites the points of its whole
//! subtree, so the hierarchy stays rigid.
//!
//! The select tool moves a bone by dragging its body and reshapes it by
//! dragging either endpoint. The pen tool places connected bone chains, each
//! new bone parented to the previous one. Weight painting is a later stage.

use bevy::prelude::*;
use bevy::window::PrimaryWindow;
use bevy_vello::prelude::vello;
use vello::Scene;
use vello::kurbo::{Affine, BezPath, Stroke as KurboStroke};
use vello::peniko::{Color, Fill};

use crate::editor::{
    CONTENT_TOP, CanvasInput, DocumentBounds, EditorMode, EditorState, SIDEBAR_WIDTH, Tool,
    ToolState, ViewMapping, Viewport, over_ui,
};
use crate::menu_bar::MenuState;
use crate::model::{Bone, Document, Point};
use crate::render::to_kurbo_point;
use crate::weight_paint::{WeightPaintBrush, paint_document};

/// Extra distance, in screen pixels, within which a click counts as hitting a
/// bone's body.
const BONE_HIT_SLACK: f32 = 7.0;

/// Radius, in screen pixels, within which a press grabs a bone joint.
const BONE_HANDLE_HIT_RADIUS: f32 = 12.0;

/// Radius, in screen pixels, within which a joint belonging to another bone
/// still wins over the body under the cursor.
///
/// This matches the drawn diamond, so a joint is only grabbed when the cursor
/// is actually on its handle; a click elsewhere on a bone's body selects that
/// bone instead of a neighbour's nearby joint.
const BONE_HANDLE_CORE_RADIUS: f32 = 8.0;

/// Distance, in screen pixels, within which dragging a head onto any bone's
/// tail snaps the head there: the dragged bone is attached under that bone and
/// its head is omitted.
const BONE_CONNECT_SNAP: f32 = 10.0;

/// Distance, in screen pixels, an endpoint must move before releasing counts
/// as a drag that can snap. A plain click on a joint leaves the rig alone.
const BONE_ENDPOINT_CLICK_SLOP: f32 = 3.0;

/// Shortest bone, in screen pixels, the pen tool can create.
const MIN_BONE_SCREEN_LENGTH: f32 = 4.0;

/// Bone overlay dimensions, in screen pixels.
const OVERLAY_BONE_WIDTH: f32 = 1.8;
const OVERLAY_BONE_SELECTED_WIDTH: f32 = 2.4;
const OVERLAY_DIAMOND_RADIUS: f32 = 6.0;
const OVERLAY_DIAMOND_SELECTED_RADIUS: f32 = 8.0;
const OVERLAY_DIAMOND_OUTLINE_WIDTH: f32 = 1.6;

/// Fraction of the bone's length where the kite is widest.
const KITE_SHOULDER_FRACTION: f32 = 0.18;

/// Half-width of the kite at its widest point, as a fraction of the length.
const KITE_WIDTH_FRACTION: f32 = 0.14;

/// Bone overlay colors.
const BONE_FILL: Color = Color::from_rgba8(0x4a, 0x6b, 0x88, 0xcc);
const BONE_STROKE: Color = Color::from_rgba8(0x9a, 0xb6, 0xcc, 0xff);
const BONE_SELECTED_FILL: Color = Color::from_rgba8(0x4c, 0xc9, 0xf0, 0xdd);
const BONE_SELECTED_STROKE: Color = Color::from_rgba8(0xaf, 0xe9, 0xff, 0xff);
const BONE_HANDLE_FILL: Color = Color::from_rgba8(0xf2, 0xf8, 0xff, 0xff);

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

/// An in-progress inline rename of a bone row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BoneRename {
    /// The bone whose name is being edited.
    pub address: BoneAddress,
    /// The text typed so far.
    pub buffer: String,
}

/// A bone drag in progress.
#[derive(Clone, Debug)]
enum BoneDrag {
    /// Moving the whole bone and its descendants through its body.
    Bulk { address: BoneAddress, last: Point },
    /// Reshaping one endpoint.
    Endpoint {
        address: BoneAddress,
        end: BoneEnd,
        /// Where the press landed, so a click alone does not snap.
        origin: Point,
    },
}

/// A pen chain in progress: the last joint placed and the bone the next one
/// parents to.
#[derive(Clone, Debug)]
struct PenChain {
    last: Point,
    parent: Option<BoneAddress>,
}

/// Transient rig-editing state.
///
/// This lives inside [`EditorState`] alongside the draw tool's transient state
/// so that every mode's in-progress gesture is cleared by the same document
/// and selection operations.
#[derive(Default)]
pub struct RigEditing {
    /// The bone drag in progress, if any.
    drag: Option<BoneDrag>,
    /// The pen chain being built, if any.
    chain: Option<PenChain>,
    /// The line previewing the next pen bone, from the last joint to the cursor.
    preview: Option<(Point, Point)>,
}

impl RigEditing {
    /// Drops any in-progress drag or pen chain, for example when switching
    /// tools or modes.
    pub fn cancel_editing(&mut self) {
        self.drag = None;
        self.chain = None;
        self.preview = None;
    }

    /// The line previewing the next pen bone, if a chain is in progress.
    pub fn preview(&self) -> Option<(Point, Point)> {
        self.preview
    }
}

pub struct RigPlugin;

impl Plugin for RigPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<WeightPaintBrush>().add_systems(
            Update,
            (
                handle_bone_input,
                // Inline renames use the shared tree widget's keyboard handler.
                crate::tree::handle_rename_input::<BoneAddress>,
            )
                .chain()
                .after(CanvasInput),
        );
    }
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

/// The address of the bone's parent, if it has one.
fn parent_address(address: &BoneAddress) -> Option<BoneAddress> {
    let chain = address.chain();
    (chain.len() > 1).then(|| BoneAddress {
        bones: chain[..chain.len() - 1].to_vec(),
    })
}

/// The tail of the bone's parent, which an omitted head follows.
fn parent_tail(document: &Document, address: &BoneAddress) -> Option<Point> {
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
// Geometry
// ---------------------------------------------------------------------------

fn distance(a: Point, b: Point) -> f32 {
    (a[0] - b[0]).hypot(a[1] - b[1])
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

/// Renames a bone, updating any weights bound to it.
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
    }
    true
}

// ---------------------------------------------------------------------------
// Endpoint editing
// ---------------------------------------------------------------------------

/// The tail of the nearest bone to `point` within `tolerance`, ignoring the
/// bone named by `exclude` and everything below it.
///
/// This is the target a dragged head snaps to, so it can attach to any bone
/// that would not put it inside itself.
fn nearest_tail(
    document: &Document,
    point: Point,
    tolerance: f32,
    exclude: &BoneAddress,
) -> Option<BoneAddress> {
    fn walk(
        bones: &[Bone],
        prefix: &mut Vec<usize>,
        point: Point,
        tolerance: f32,
        exclude: &BoneAddress,
        best: &mut Option<(f32, BoneAddress)>,
    ) {
        for (index, bone) in bones.iter().enumerate() {
            prefix.push(index);
            let address = BoneAddress {
                bones: prefix.clone(),
            };
            // A bone cannot attach to itself or to one of its descendants.
            if !address_is_inside(exclude, &address) {
                let distance = distance(point, bone.tail);
                if distance <= tolerance && best.as_ref().is_none_or(|(best, _)| distance < *best) {
                    *best = Some((distance, address.clone()));
                }
            }
            walk(&bone.children, prefix, point, tolerance, exclude, best);
            prefix.pop();
        }
    }

    let mut best = None;
    walk(
        &document.bones,
        &mut Vec::new(),
        point,
        tolerance,
        exclude,
        &mut best,
    );
    best.map(|(_, address)| address)
}

/// The stored head of the nearest bone to `point` within `tolerance`.
///
/// Only bones that store their own head are candidates: an omitted head is the
/// same joint as its parent's tail, so `nearest_tail` covers it. A bone that is
/// `exclude` or one of its ancestors is skipped, since making it a child of the
/// dragged bone would form a cycle.
fn nearest_head(
    document: &Document,
    point: Point,
    tolerance: f32,
    exclude: &BoneAddress,
) -> Option<(BoneAddress, Point)> {
    fn walk(
        bones: &[Bone],
        prefix: &mut Vec<usize>,
        parent_tail: Option<Point>,
        point: Point,
        tolerance: f32,
        exclude: &BoneAddress,
        best: &mut Option<(f32, BoneAddress, Point)>,
    ) {
        for (index, bone) in bones.iter().enumerate() {
            prefix.push(index);
            let address = BoneAddress {
                bones: prefix.clone(),
            };
            // A head resting on its parent's tail is that shared joint, not a
            // separate head: the parent's tail owns it, so it must not be the
            // target of a tail-to-head connect.
            if let Some(head) = bone.head
                && Some(head) != parent_tail
                && !address_is_inside(&address, exclude)
            {
                let distance = distance(point, head);
                if distance <= tolerance
                    && best.as_ref().is_none_or(|(best, _, _)| distance < *best)
                {
                    *best = Some((distance, address.clone(), head));
                }
            }
            walk(
                &bone.children,
                prefix,
                Some(bone.tail),
                point,
                tolerance,
                exclude,
                best,
            );
            prefix.pop();
        }
    }

    let mut best = None;
    walk(
        &document.bones,
        &mut Vec::new(),
        None,
        point,
        tolerance,
        exclude,
        &mut best,
    );
    best.map(|(_, address, head)| (address, head))
}

/// Moves one endpoint of a bone to `world_point` while a drag is live.
///
/// A tail carries its connected children with it; a stored head moves alone.
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
            // Children whose stored head sat exactly on this tail were sharing
            // the joint, so they stay connected instead of being left behind.
            for child in &mut bone.children {
                if child.head == Some(old_tail) {
                    child.head = None;
                }
            }
        }
    }
    true
}

/// Joins a released endpoint to a matching joint, so dragging a tail onto a
/// head (or a head onto a tail) connects the two bones.
///
/// Returns whether the tree changed, so callers can bump the structure
/// revision.
pub fn finish_endpoint_drag(
    editor: &mut EditorState,
    address: &BoneAddress,
    end: BoneEnd,
    world_point: Point,
    snap: f32,
) -> bool {
    match end {
        BoneEnd::Head => {
            // A head snaps onto a tail, becoming that bone's child.
            let Some(target) = nearest_tail(&editor.document, world_point, snap, address) else {
                return false;
            };
            if parent_address(address).as_ref() == Some(&target) {
                // Reconnecting to the current parent only omits the head.
                let Some(bone) = bone_at_mut(&mut editor.document, address) else {
                    return false;
                };
                if bone.head.is_none() {
                    return false;
                }
                bone.head = None;
                return false;
            }
            let Some(new_address) = move_bone_into(&mut editor.document, address, &target) else {
                return false;
            };
            if let Some(bone) = bone_at_mut(&mut editor.document, &new_address) {
                bone.head = None;
            }
            editor.select_bone(Some(new_address));
            true
        }
        BoneEnd::Tail => {
            // A tail snaps onto a stored head, taking that bone as a child.
            let Some((target, head)) = nearest_head(&editor.document, world_point, snap, address)
            else {
                return false;
            };
            // Removing the target can shift the dragged bone's own index when
            // they are siblings, so capture its slot first.
            let source_slot = bone_slot(&editor.document, &target);
            let dragged_slot = bone_slot(&editor.document, address);
            if let Some(bone) = bone_at_mut(&mut editor.document, address) {
                bone.tail = head;
            }
            let Some(new_address) = move_bone_into(&mut editor.document, &target, address) else {
                return false;
            };
            if let Some(bone) = bone_at_mut(&mut editor.document, &new_address) {
                bone.head = None;
            }
            if let (Some(dragged_slot), Some(source_slot)) = (dragged_slot, source_slot) {
                let dragged_after = shift_slot_after_removal(dragged_slot, &source_slot);
                editor.select_bone(Some(BoneAddress {
                    bones: dragged_after.chain,
                }));
            }
            true
        }
    }
}

// ---------------------------------------------------------------------------
// Hit testing
// ---------------------------------------------------------------------------

/// Distance from `point` to the segment `a`–`b`.
fn distance_to_segment(point: Point, a: Point, b: Point) -> f32 {
    let ab = [b[0] - a[0], b[1] - a[1]];
    let length_squared = ab[0] * ab[0] + ab[1] * ab[1];
    let t = if length_squared <= f32::EPSILON {
        0.0
    } else {
        (((point[0] - a[0]) * ab[0] + (point[1] - a[1]) * ab[1]) / length_squared).clamp(0.0, 1.0)
    };
    let closest = [a[0] + ab[0] * t, a[1] + ab[1] * t];
    ((point[0] - closest[0]).powi(2) + (point[1] - closest[1]).powi(2)).sqrt()
}

/// The four corners of a bone's kite: head, widest left, tail, widest right.
fn kite_points(head: Point, tail: Point) -> Option<[Point; 4]> {
    let length = distance(head, tail);
    if length <= f32::EPSILON {
        return None;
    }
    let dir = [(tail[0] - head[0]) / length, (tail[1] - head[1]) / length];
    let perp = [-dir[1], dir[0]];
    let shoulder = [
        head[0] + dir[0] * length * KITE_SHOULDER_FRACTION,
        head[1] + dir[1] * length * KITE_SHOULDER_FRACTION,
    ];
    let half_width = length * KITE_WIDTH_FRACTION;
    let left = [
        shoulder[0] + perp[0] * half_width,
        shoulder[1] + perp[1] * half_width,
    ];
    let right = [
        shoulder[0] - perp[0] * half_width,
        shoulder[1] - perp[1] * half_width,
    ];
    Some([head, left, tail, right])
}

/// Whether `point` lies inside the convex quad `quad`.
fn point_in_quad(point: Point, quad: [Point; 4]) -> bool {
    let mut sign = 0.0;
    for index in 0..4 {
        let a = quad[index];
        let b = quad[(index + 1) % 4];
        let cross = (b[0] - a[0]) * (point[1] - a[1]) - (b[1] - a[1]) * (point[0] - a[0]);
        if cross.abs() <= f32::EPSILON {
            continue;
        }
        if sign == 0.0 {
            sign = cross.signum();
        } else if cross.signum() != sign {
            return false;
        }
    }
    true
}

/// Distance from `point` to a bone's kite: zero inside it, otherwise the
/// distance to its outline. Falls back to the centre line for a zero-length
/// bone.
fn distance_to_kite(point: Point, head: Point, tail: Point) -> f32 {
    let Some(quad) = kite_points(head, tail) else {
        return distance_to_segment(point, head, tail);
    };
    if point_in_quad(point, quad) {
        return 0.0;
    }
    (0..4)
        .map(|index| distance_to_segment(point, quad[index], quad[(index + 1) % 4]))
        .fold(f32::INFINITY, f32::min)
}

/// Returns the bone whose body is nearest to `point`, within `tolerance`.
pub fn bone_near(document: &Document, point: Point, tolerance: f32) -> Option<BoneAddress> {
    fn walk(
        bones: &[Bone],
        prefix: &mut Vec<usize>,
        parent_tail: Option<Point>,
        point: Point,
        tolerance: f32,
        best: &mut Option<(f32, Vec<usize>)>,
    ) {
        for (index, bone) in bones.iter().enumerate() {
            let head = bone.head_with_parent(parent_tail);
            let distance = distance_to_kite(point, head, bone.tail);
            prefix.push(index);
            if distance <= tolerance && best.as_ref().is_none_or(|(best, _)| distance < *best) {
                *best = Some((distance, prefix.clone()));
            }
            walk(
                &bone.children,
                prefix,
                Some(bone.tail),
                point,
                tolerance,
                best,
            );
            prefix.pop();
        }
    }

    let mut best = None;
    walk(
        &document.bones,
        &mut Vec::new(),
        None,
        point,
        tolerance,
        &mut best,
    );
    best.map(|(_, bones)| BoneAddress { bones })
}

/// Returns the joint nearest to `point` within `radius`, as the bone and end
/// that owns it, together with the joint's position.
///
/// Every bone owns its tail joint. A bone also owns its head joint when it
/// stores one; an omitted head shares its parent's tail joint, so a connected
/// child contributes no head handle of its own. A stored head resting on the
/// parent's tail is that same shared joint, so it resolves to the parent's
/// tail rather than to a separate head.
pub fn endpoint_near(
    document: &Document,
    point: Point,
    radius: f32,
) -> Option<(BoneAddress, BoneEnd, Point)> {
    fn walk(
        bones: &[Bone],
        prefix: &mut Vec<usize>,
        parent_tail: Option<Point>,
        point: Point,
        radius: f32,
        best: &mut Option<(f32, BoneAddress, BoneEnd, Point)>,
    ) {
        for (index, bone) in bones.iter().enumerate() {
            prefix.push(index);
            let address = BoneAddress {
                bones: prefix.clone(),
            };
            // Tails win ties so a shared joint resolves to the parent, which
            // is the bone that actually owns the point.
            let tail_distance = distance(point, bone.tail);
            if tail_distance <= radius
                && best
                    .as_ref()
                    .is_none_or(|(best, _, _, _)| tail_distance < *best)
            {
                *best = Some((tail_distance, address.clone(), BoneEnd::Tail, bone.tail));
            }
            if let Some(head) = bone.head
                && Some(head) != parent_tail
            {
                let head_distance = distance(point, head);
                if head_distance <= radius
                    && best
                        .as_ref()
                        .is_none_or(|(best, _, _, _)| head_distance < *best)
                {
                    *best = Some((head_distance, address, BoneEnd::Head, head));
                }
            }
            walk(&bone.children, prefix, Some(bone.tail), point, radius, best);
            prefix.pop();
        }
    }

    let mut best = None;
    walk(
        &document.bones,
        &mut Vec::new(),
        None,
        point,
        radius,
        &mut best,
    );
    best.map(|(_, address, end, at)| (address, end, at))
}

// ---------------------------------------------------------------------------
// Renaming
// ---------------------------------------------------------------------------

/// Starts renaming the bone at `address`.
pub fn begin_bone_rename(editor: &mut EditorState, address: BoneAddress) {
    let buffer = bone_name_at(&editor.document, &address)
        .unwrap_or_default()
        .to_owned();
    editor.selected_bone = Some(address.clone());
    editor.bone_renaming = Some(BoneRename { address, buffer });
}

/// Commits the in-progress bone rename, if any.
pub fn commit_bone_rename(editor: &mut EditorState) {
    let Some(renaming) = editor.bone_renaming.take() else {
        return;
    };
    let name = renaming.buffer.trim().to_owned();
    if rename_bone_at(&mut editor.document, &renaming.address, name) {
        editor.structure_revision += 1;
    }
}

/// Abandons the in-progress bone rename, if any.
pub fn cancel_bone_rename(editor: &mut EditorState) {
    editor.bone_renaming = None;
}

/// Deletes the selected bone and its children. Returns whether anything was
/// removed.
pub fn delete_selected_bone(editor: &mut EditorState) -> bool {
    let Some(address) = editor.selected_bone.take() else {
        return false;
    };
    editor.bone_renaming = None;
    if remove_bone(&mut editor.document, &address) {
        editor.structure_revision += 1;
        true
    } else {
        false
    }
}

// ---------------------------------------------------------------------------
// Input
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
fn handle_bone_input(
    mode: Res<EditorMode>,
    mut tool: ResMut<ToolState>,
    mouse: Res<ButtonInput<MouseButton>>,
    keys: Res<ButtonInput<KeyCode>>,
    windows: Query<&Window, With<PrimaryWindow>>,
    bounds: Res<DocumentBounds>,
    viewport: Res<Viewport>,
    menu: Option<Res<MenuState>>,
    brush: Res<WeightPaintBrush>,
    mut editor: ResMut<EditorState>,
    mut paint_last: Local<Option<Point>>,
) {
    if *mode != EditorMode::Rig {
        return;
    }

    // A dropdown owns the pointer while it is open.
    if menu.is_some_and(|menu| menu.is_open()) {
        if mouse.just_released(MouseButton::Left) {
            editor.rig.drag = None;
        }
        return;
    }

    // While a bone row is being renamed, the keyboard belongs to the text
    // field. A click elsewhere commits the edit and then behaves normally, but
    // presses inside the skeleton tree belong to the tree: the second press of
    // a double-click starts a new rename, and it must not be committed by this
    // system on the same frame.
    if editor.is_renaming() {
        if !mouse.just_pressed(MouseButton::Left) {
            return;
        }
        let over_sidebar = windows.single().is_ok_and(|window| {
            window.cursor_position().is_some_and(|cursor| {
                cursor.x >= window.width() - SIDEBAR_WIDTH && cursor.y >= CONTENT_TOP
            })
        });
        if over_sidebar {
            return;
        }
        commit_bone_rename(&mut editor);
    }

    if keys.just_pressed(KeyCode::Escape) {
        // Escape leaves the pen and weight-paint tools, abandoning any chain,
        // matching how the drawing pen behaves.
        editor.rig.cancel_editing();
        *paint_last = None;
        if matches!(tool.0, Tool::Pen | Tool::WeightPaint) {
            tool.0 = Tool::Select;
        } else {
            editor.selected_bone = None;
        }
        return;
    }

    if keys.just_pressed(KeyCode::Delete) && tool.0 == Tool::Select {
        editor.rig.cancel_editing();
        delete_selected_bone(&mut editor);
        return;
    }

    let Ok(window) = windows.single() else {
        return;
    };
    let Some(cursor) = window.cursor_position() else {
        return;
    };

    let view = ViewMapping::new(bounds.0, window.width(), window.height(), &viewport);
    let point = view.screen_to_document(cursor);
    let pixel = view.document_per_pixel();
    let over = over_ui(cursor, window);

    match tool.0 {
        Tool::Select => handle_bone_select(&mouse, &mut editor, point, pixel, over),
        Tool::Pen => handle_bone_pen(&mouse, &mut editor, point, pixel, over),
        Tool::WeightPaint => handle_weight_paint(
            &mouse,
            &mut editor,
            point,
            pixel,
            over,
            &brush,
            &mut paint_last,
        ),
    }
}

/// Weight-paint tool: drag over curve points to repaint their bone weights.
///
/// The brush paints at the cursor's starting position and again whenever the
/// cursor moves, so a fast drag leaves a continuous stroke rather than gaps.
/// Geometry is never moved; only weight lists change.
fn handle_weight_paint(
    mouse: &ButtonInput<MouseButton>,
    editor: &mut EditorState,
    point: Point,
    pixel: f32,
    over: bool,
    brush: &WeightPaintBrush,
    paint_last: &mut Option<Point>,
) {
    if over || !mouse.pressed(MouseButton::Left) {
        *paint_last = None;
        return;
    }
    // Paint on the initial press and on every frame the cursor actually moves.
    let moved = *paint_last != Some(point);
    if !mouse.just_pressed(MouseButton::Left) && !moved {
        return;
    }
    *paint_last = Some(point);

    // Borrow the name so the document can be edited afterwards.
    let selected = editor
        .selected_bone
        .as_ref()
        .and_then(|address| bone_name_at(&editor.document, address))
        .map(str::to_owned);
    let radius = brush.size * pixel;
    paint_document(
        &mut editor.document,
        point,
        radius,
        selected.as_deref(),
        brush,
    );
}

/// Select tool: click to select, drag the body to move, drag an endpoint to
/// reshape.
fn handle_bone_select(
    mouse: &ButtonInput<MouseButton>,
    editor: &mut EditorState,
    point: Point,
    pixel: f32,
    over: bool,
) {
    if over {
        // The UI handles clicks here. Still finish a drag released over a
        // panel.
        if mouse.just_released(MouseButton::Left) {
            editor.rig.drag = None;
        }
        return;
    }

    if mouse.just_pressed(MouseButton::Left) {
        let joint = endpoint_near(&editor.document, point, pixel * BONE_HANDLE_HIT_RADIUS);
        let body = bone_near(&editor.document, point, pixel * BONE_HIT_SLACK);
        // A joint is grabbed when the press is on its handle. One belonging to
        // the bone under the cursor always wins, so the shared tail of a bone
        // can be dragged without selecting a child. A joint belonging to a
        // different bone only wins when the cursor is right on top of it;
        // otherwise the clicked body wins, so a single click selects the bone
        // the user aimed at instead of a neighbour's nearby joint.
        let grab_joint = match (&joint, &body) {
            (Some((address, _, at)), Some(body)) => {
                address == body || distance(point, *at) <= pixel * BONE_HANDLE_CORE_RADIUS
            }
            (Some(_), None) => true,
            _ => false,
        };

        if grab_joint && let Some((address, end, _)) = joint {
            // Selecting clears any previous gesture, so the new drag is set
            // after it.
            editor.select_bone(Some(address.clone()));
            editor.rig.drag = Some(BoneDrag::Endpoint {
                address,
                end,
                origin: point,
            });
            return;
        }

        // Clicking a bone's body selects it and starts moving it.
        if let Some(address) = body {
            editor.select_bone(Some(address.clone()));
            editor.rig.drag = Some(BoneDrag::Bulk {
                address,
                last: point,
            });
            return;
        }

        editor.select_bone(None);
        editor.rig.drag = None;
    }

    if mouse.pressed(MouseButton::Left) {
        match editor.rig.drag.clone() {
            Some(BoneDrag::Bulk { address, last }) => {
                let delta = [point[0] - last[0], point[1] - last[1]];
                translate_bone(&mut editor.document, &address, delta);
                editor.rig.drag = Some(BoneDrag::Bulk {
                    address,
                    last: point,
                });
            }
            Some(BoneDrag::Endpoint { address, end, .. }) => {
                drag_bone_endpoint(&mut editor.document, &address, end, point);
            }
            None => {}
        }
    }

    if mouse.just_released(MouseButton::Left) {
        // Joining the released endpoint to a nearby joint connects the bones.
        // A press that never moved is just a click, so it does not snap.
        if let Some(BoneDrag::Endpoint {
            address,
            end,
            origin,
        }) = editor.rig.drag.take()
            && distance(origin, point) > pixel * BONE_ENDPOINT_CLICK_SLOP
            && finish_endpoint_drag(editor, &address, end, point, pixel * BONE_CONNECT_SNAP)
        {
            editor.structure_revision += 1;
        }
        editor.rig.drag = None;
    }
}

/// Pen tool: each click places a joint; consecutive joints become a chain of
/// parented bones.
fn handle_bone_pen(
    mouse: &ButtonInput<MouseButton>,
    editor: &mut EditorState,
    point: Point,
    pixel: f32,
    over: bool,
) {
    if over {
        editor.rig.preview = None;
        return;
    }

    if mouse.just_pressed(MouseButton::Left) {
        match editor.rig.chain.take() {
            None => {
                // First joint of a new chain.
                editor.selected_bone = None;
                editor.bone_renaming = None;
                editor.rig.chain = Some(PenChain {
                    last: point,
                    parent: None,
                });
            }
            Some(chain) => {
                if distance(chain.last, point) > pixel * MIN_BONE_SCREEN_LENGTH {
                    // Bone names double as weight ids, so each new bone gets a
                    // name no other bone in the rig uses.
                    let name = unique_bone_name(&editor.document);
                    let address = match &chain.parent {
                        Some(parent) => {
                            add_child_bone(&mut editor.document, parent, chain.last, point, name)
                        }
                        None => Some(add_bone(&mut editor.document, chain.last, point, name)),
                    };
                    if let Some(address) = address {
                        editor.selected_bone = Some(address.clone());
                        editor.structure_revision += 1;
                        editor.rig.chain = Some(PenChain {
                            last: point,
                            parent: Some(address),
                        });
                    } else {
                        editor.rig.chain = Some(chain);
                    }
                } else {
                    // A click on the same joint keeps the chain alive.
                    editor.rig.chain = Some(chain);
                }
            }
        }
    }

    editor.rig.preview = editor.rig.chain.as_ref().map(|chain| (chain.last, point));
}

// ---------------------------------------------------------------------------
// Drawing
// ---------------------------------------------------------------------------

/// Draws the skeleton over the document, highlighting the selected bone and
/// previewing the next pen joint.
pub fn draw_bone_overlay(
    scene: &mut Scene,
    document: &Document,
    selected: Option<&BoneAddress>,
    preview: Option<(Point, Point)>,
    view_scale: f32,
) {
    let scale = view_scale.max(f32::EPSILON);
    draw_bone_list(
        scene,
        &document.bones,
        &mut Vec::new(),
        None,
        selected,
        scale,
    );

    // Redraw the selection last so it is never hidden behind another bone.
    if let Some(address) = selected
        && let Some((head, tail)) = bone_endpoints(document, address)
    {
        draw_bone(
            scene,
            head,
            tail,
            scale,
            &BONE_SELECTED_FILL,
            &BONE_SELECTED_STROKE,
            OVERLAY_BONE_SELECTED_WIDTH,
        );
        draw_diamond(
            scene,
            head,
            scale,
            OVERLAY_DIAMOND_SELECTED_RADIUS,
            &BONE_SELECTED_STROKE,
        );
        draw_diamond(
            scene,
            tail,
            scale,
            OVERLAY_DIAMOND_SELECTED_RADIUS,
            &BONE_SELECTED_STROKE,
        );
    }

    // The in-progress pen bone is drawn as a kite, matching bones that have
    // already been placed.
    if let Some((start, end)) = preview {
        draw_bone(
            scene,
            start,
            end,
            scale,
            &BONE_SELECTED_FILL,
            &BONE_SELECTED_STROKE,
            OVERLAY_BONE_SELECTED_WIDTH,
        );
        draw_diamond(
            scene,
            end,
            scale,
            OVERLAY_DIAMOND_SELECTED_RADIUS,
            &BONE_SELECTED_STROKE,
        );
    }
}

fn draw_bone_list(
    scene: &mut Scene,
    bones: &[Bone],
    prefix: &mut Vec<usize>,
    parent_tail: Option<Point>,
    selected: Option<&BoneAddress>,
    scale: f32,
) {
    for (index, bone) in bones.iter().enumerate() {
        prefix.push(index);
        let address = BoneAddress {
            bones: prefix.clone(),
        };
        let head = bone.head_with_parent(parent_tail);
        // The selected bone is drawn later, on top.
        if selected != Some(&address) {
            draw_bone(
                scene,
                head,
                bone.tail,
                scale,
                &BONE_FILL,
                &BONE_STROKE,
                OVERLAY_BONE_WIDTH,
            );
            // A connected bone shares its head joint with the parent's tail,
            // so only a stored head gets a handle of its own.
            draw_diamond(
                scene,
                bone.tail,
                scale,
                OVERLAY_DIAMOND_RADIUS,
                &BONE_STROKE,
            );
            if bone.head.is_some() && Some(head) != parent_tail {
                draw_diamond(scene, head, scale, OVERLAY_DIAMOND_RADIUS, &BONE_STROKE);
            }
        }
        draw_bone_list(
            scene,
            &bone.children,
            prefix,
            Some(bone.tail),
            selected,
            scale,
        );
        prefix.pop();
    }
}

/// Fills and outlines a bone as a kite: a pointed quad widest just past the
/// head and tapering to the tail.
fn draw_bone(
    scene: &mut Scene,
    head: Point,
    tail: Point,
    scale: f32,
    fill: &Color,
    stroke: &Color,
    width: f32,
) {
    let Some(quad) = kite_points(head, tail) else {
        return;
    };

    let mut path = BezPath::new();
    path.move_to(to_kurbo_point(quad[0]));
    path.line_to(to_kurbo_point(quad[1]));
    path.line_to(to_kurbo_point(quad[2]));
    path.line_to(to_kurbo_point(quad[3]));
    path.close_path();

    scene.fill(Fill::NonZero, Affine::IDENTITY, fill, None, &path);
    scene.stroke(
        &KurboStroke::new(px(width, scale)),
        Affine::IDENTITY,
        stroke,
        None,
        &path,
    );
}

/// Draws a diamond-shaped handle centered on `center`.
fn draw_diamond(scene: &mut Scene, center: Point, scale: f32, radius: f32, color: &Color) {
    let r = radius / scale.max(f32::EPSILON);
    let [cx, cy] = center;
    let mut path = BezPath::new();
    path.move_to(to_kurbo_point([cx, cy - r]));
    path.line_to(to_kurbo_point([cx + r, cy]));
    path.line_to(to_kurbo_point([cx, cy + r]));
    path.line_to(to_kurbo_point([cx - r, cy]));
    path.close_path();

    scene.fill(
        Fill::NonZero,
        Affine::IDENTITY,
        &BONE_HANDLE_FILL,
        None,
        &path,
    );
    scene.stroke(
        &KurboStroke::new(px(OVERLAY_DIAMOND_OUTLINE_WIDTH, scale)),
        Affine::IDENTITY,
        color,
        None,
        &path,
    );
}

/// Converts a screen-pixel length into document units.
fn px(value: f32, scale: f32) -> f64 {
    f64::from(value / scale.max(f32::EPSILON))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{FORMAT_VERSION, Path, Shape};

    /// A shape whose only path carries the given start/anchor weights.
    fn shape_with_weights(start: &[(&str, f32)], anchor: &[(&str, f32)]) -> Shape {
        let mut path = Path::new([0.0, 0.0]).line_to([10.0, 0.0]);
        for &(bone, weight) in start {
            path.start_weights.set(bone, weight);
        }
        for &(bone, weight) in anchor {
            path.segments[0].to_weights.set(bone, weight);
        }
        Shape {
            paths: vec![path],
            ..Default::default()
        }
    }

    fn document_with_bones() -> Document {
        Document {
            version: FORMAT_VERSION,
            shapes: Vec::new(),
            bones: vec![Bone::from_endpoints([0.0, 0.0], [10.0, 0.0])],
        }
    }

    fn address(chain: &[usize]) -> BoneAddress {
        BoneAddress {
            bones: chain.to_vec(),
        }
    }

    fn assert_point(actual: Point, expected: Point) {
        assert!(
            (actual[0] - expected[0]).abs() < 1e-4 && (actual[1] - expected[1]).abs() < 1e-4,
            "expected {expected:?}, got {actual:?}"
        );
    }

    #[test]
    fn a_new_bone_runs_from_head_to_tail() {
        let mut document = Document::new();
        let address = add_bone(&mut document, [10.0, 20.0], [10.0, 50.0], "Bone");

        let bone = bone_at(&document, &address).unwrap();
        assert_eq!(bone.head, Some([10.0, 20.0]));
        assert_point(bone.tail, [10.0, 50.0]);
        assert_eq!(
            bone_endpoints(&document, &address),
            Some(([10.0, 20.0], [10.0, 50.0]))
        );
    }

    #[test]
    fn dragging_the_head_moves_only_the_head() {
        let mut document = document_with_bones();
        let address = address(&[0]);

        assert!(drag_bone_endpoint(
            &mut document,
            &address,
            BoneEnd::Head,
            [5.0, 5.0]
        ));
        assert_eq!(
            bone_endpoints(&document, &address),
            Some(([5.0, 5.0], [10.0, 0.0]))
        );
    }

    #[test]
    fn dragging_the_tail_moves_only_the_tail() {
        let mut document = document_with_bones();
        let address = address(&[0]);

        assert!(drag_bone_endpoint(
            &mut document,
            &address,
            BoneEnd::Tail,
            [0.0, 20.0]
        ));
        let (head, tail) = bone_endpoints(&document, &address).unwrap();
        assert_point(head, [0.0, 0.0]);
        assert_point(tail, [0.0, 20.0]);
    }

    #[test]
    fn dragging_a_parents_tail_carries_its_connected_child() {
        let mut document = Document::new();
        let parent = add_bone(&mut document, [0.0, 0.0], [10.0, 0.0], "Parent");
        let child =
            add_child_bone(&mut document, &parent, [10.0, 0.0], [10.0, 10.0], "Child").unwrap();

        // Joints are sticky: moving the parent's tail drags the connected
        // child's head with it, so the joint never separates.
        assert!(drag_bone_endpoint(
            &mut document,
            &parent,
            BoneEnd::Tail,
            [0.0, 10.0]
        ));

        let (head, tail) = bone_endpoints(&document, &child).unwrap();
        assert_point(head, [0.0, 10.0]);
        assert_point(tail, [10.0, 10.0]);
    }

    #[test]
    fn a_connected_childs_joint_is_its_parents_tail() {
        let mut document = Document::new();
        let parent = add_bone(&mut document, [0.0, 0.0], [10.0, 0.0], "Parent");
        add_child_bone(&mut document, &parent, [10.0, 0.0], [10.0, 10.0], "Child").unwrap();

        // The connected child has no head handle of its own: the joint at its
        // head is the parent's tail handle.
        assert_eq!(
            endpoint_near(&document, [10.0, 0.0], 1.0),
            Some((parent.clone(), BoneEnd::Tail, [10.0, 0.0]))
        );
    }

    #[test]
    fn a_loose_head_is_its_own_handle() {
        let mut document = Document::new();
        add_bone(&mut document, [0.0, 0.0], [10.0, 0.0], "Bone");

        assert_eq!(
            endpoint_near(&document, [0.0, 0.0], 1.0),
            Some((address(&[0]), BoneEnd::Head, [0.0, 0.0]))
        );
    }

    #[test]
    fn dragging_a_head_onto_another_bones_tail_reparents_it() {
        let mut editor = EditorState::new(Document::new());
        let dragged = add_bone(&mut editor.document, [40.0, 40.0], [50.0, 40.0], "Dragged");
        add_bone(&mut editor.document, [0.0, 0.0], [10.0, 0.0], "Host");

        // Releasing the head near the host's tail attaches it there.
        assert!(finish_endpoint_drag(
            &mut editor,
            &dragged,
            BoneEnd::Head,
            [10.5, 0.5],
            2.0
        ));

        let attached = editor
            .selected_bone
            .clone()
            .expect("the bone stays selected");
        assert_eq!(attached.chain(), &[0, 0]);
        assert_eq!(bone_at(&editor.document, &attached).unwrap().head, None);
        assert_eq!(
            bone_endpoints(&editor.document, &attached),
            Some(([10.0, 0.0], [50.0, 40.0]))
        );
    }

    #[test]
    fn dragging_a_tail_onto_another_bones_head_reparents_it() {
        let mut editor = EditorState::new(Document::new());
        let dragged = add_bone(&mut editor.document, [0.0, 0.0], [10.0, 0.0], "Dragged");
        add_bone(&mut editor.document, [20.0, 0.0], [30.0, 0.0], "Host");

        // Releasing the tail near the host's head takes the host as a child.
        assert!(finish_endpoint_drag(
            &mut editor,
            &dragged,
            BoneEnd::Tail,
            [20.5, 0.5],
            2.0
        ));

        let (head, tail) = bone_endpoints(&editor.document, &dragged).unwrap();
        assert_point(head, [0.0, 0.0]);
        assert_point(tail, [20.0, 0.0]);
        let host = address(&[0, 0]);
        assert_eq!(bone_at(&editor.document, &host).unwrap().head, None);
        assert_eq!(
            bone_endpoints(&editor.document, &host),
            Some(([20.0, 0.0], [30.0, 0.0]))
        );
    }

    #[test]
    fn snapping_a_tail_keeps_the_dragged_selection_valid() {
        let mut editor = EditorState::new(Document::new());
        // The target is an earlier sibling, so removing it shifts the dragged
        // bone down one index.
        add_bone(&mut editor.document, [20.0, 0.0], [30.0, 0.0], "First");
        let dragged = add_bone(&mut editor.document, [0.0, 0.0], [10.0, 0.0], "Second");
        editor.select_bone(Some(dragged.clone()));

        assert!(finish_endpoint_drag(
            &mut editor,
            &dragged,
            BoneEnd::Tail,
            [20.5, 0.5],
            2.0
        ));

        let selected = editor.selected_bone.clone().expect("a bone stays selected");
        assert_eq!(bone_name_at(&editor.document, &selected), Some("Second"));
        assert_eq!(selected.chain(), &[0]);
        assert_eq!(
            bone_at(&editor.document, &address(&[0, 0])).unwrap().name,
            "First"
        );
    }

    #[test]
    fn translating_a_bone_leaves_its_children_in_place() {
        let mut document = Document::new();
        let parent = add_bone(&mut document, [0.0, 0.0], [10.0, 0.0], "Parent");
        let child =
            add_child_bone(&mut document, &parent, [10.0, 0.0], [10.0, 10.0], "Child").unwrap();

        assert!(translate_bone(&mut document, &parent, [5.0, 2.0]));

        assert_eq!(
            bone_endpoints(&document, &parent),
            Some(([5.0, 2.0], [15.0, 2.0]))
        );
        let (head, tail) = bone_endpoints(&document, &child).unwrap();
        assert_point(head, [10.0, 0.0]);
        assert_point(tail, [10.0, 10.0]);
    }

    #[test]
    fn a_child_at_its_parents_tail_omits_its_head() {
        let mut document = Document::new();
        let parent = add_bone(&mut document, [0.0, 0.0], [10.0, 0.0], "Parent");
        let child =
            add_child_bone(&mut document, &parent, [10.0, 0.0], [20.0, 0.0], "Child").unwrap();

        // Connected at the parent's tail, so it stores no head of its own.
        assert_eq!(bone_at(&document, &child).unwrap().head, None);
        assert_eq!(
            bone_endpoints(&document, &child),
            Some(([10.0, 0.0], [20.0, 0.0]))
        );
    }

    #[test]
    fn a_click_on_the_wide_part_of_a_bone_hits_it() {
        let mut document = Document::new();
        let address = add_bone(&mut document, [0.0, 0.0], [100.0, 0.0], "Bone");

        // The kite is widest near its shoulder, well away from the centre
        // line, so clicking the painted body selects the bone.
        assert_eq!(bone_near(&document, [18.0, 10.0], 1.0), Some(address));
        // Far outside the drawn shape nothing is hit.
        assert_eq!(bone_near(&document, [18.0, 30.0], 1.0), None);
    }

    #[test]
    fn translating_by_zero_leaves_connected_children_attached() {
        let mut document = Document::new();
        let parent = add_bone(&mut document, [0.0, 0.0], [10.0, 0.0], "Parent");
        let child =
            add_child_bone(&mut document, &parent, [10.0, 0.0], [10.0, 10.0], "Child").unwrap();

        // The first frame of a body drag applies no movement; it must not
        // silently materialize the child's head and split the shared joint.
        assert!(translate_bone(&mut document, &parent, [0.0, 0.0]));
        assert_eq!(bone_at(&document, &child).unwrap().head, None);
    }

    #[test]
    fn reordering_a_connected_child_keeps_it_shared() {
        let mut document = Document::new();
        let parent = add_bone(&mut document, [0.0, 0.0], [10.0, 0.0], "Parent");
        let child =
            add_child_bone(&mut document, &parent, [10.0, 0.0], [10.0, 10.0], "Child").unwrap();
        assert_eq!(bone_at(&document, &child).unwrap().head, None);

        // Dropping a connected child back onto its own parent reorders it;
        // the shared joint must survive.
        let moved = move_bone_into(&mut document, &child, &parent).unwrap();
        assert_eq!(moved.chain(), &[0, 0]);
        assert_eq!(bone_at(&document, &moved).unwrap().head, None);
    }

    #[test]
    fn dragging_a_shared_tail_carries_every_connected_child() {
        let mut document = Document::new();
        let parent = add_bone(&mut document, [0.0, 0.0], [10.0, 0.0], "Parent");
        let a = add_child_bone(&mut document, &parent, [10.0, 0.0], [10.0, 10.0], "A").unwrap();
        let b = add_child_bone(&mut document, &parent, [10.0, 0.0], [10.0, -10.0], "B").unwrap();

        assert!(drag_bone_endpoint(
            &mut document,
            &parent,
            BoneEnd::Tail,
            [20.0, 0.0]
        ));

        for child in [&a, &b] {
            let (head, _) = bone_endpoints(&document, child).unwrap();
            assert_point(head, [20.0, 0.0]);
        }
    }

    #[test]
    fn dragging_a_tail_pulls_in_a_child_stored_on_it() {
        let mut document = Document::new();
        let parent = add_bone(&mut document, [0.0, 0.0], [10.0, 0.0], "Parent");
        let child = add_bone(&mut document, [10.0, 0.0], [10.0, 10.0], "Child");
        let child = move_bone_into(&mut document, &child, &parent).unwrap();
        assert_eq!(bone_at(&document, &child).unwrap().head, None);

        // Stand in for an older file that stored a redundant head on the
        // parent's tail instead of omitting it.
        bone_at_mut(&mut document, &child).unwrap().head = Some([10.0, 0.0]);

        assert!(drag_bone_endpoint(
            &mut document,
            &parent,
            BoneEnd::Tail,
            [20.0, 0.0]
        ));
        assert_eq!(bone_at(&document, &child).unwrap().head, None);
        let (head, _) = bone_endpoints(&document, &child).unwrap();
        assert_point(head, [20.0, 0.0]);
    }

    #[test]
    fn a_head_resting_on_the_parents_tail_resolves_to_the_tail() {
        let mut document = Document::new();
        let parent = add_bone(&mut document, [0.0, 0.0], [10.0, 0.0], "Parent");
        let child = add_bone(&mut document, [10.0, 0.0], [10.0, 10.0], "Child");
        let child = move_bone_into(&mut document, &child, &parent).unwrap();
        bone_at_mut(&mut document, &child).unwrap().head = Some([10.0, 0.0]);

        // The redundant head is the parent's tail joint, owned by the parent.
        assert_eq!(
            endpoint_near(&document, [10.0, 0.0], 1.0),
            Some((parent, BoneEnd::Tail, [10.0, 0.0]))
        );
    }

    #[test]
    fn dragging_a_head_onto_the_parents_tail_reconnects_it() {
        let mut editor = EditorState::new(Document::new());
        let parent = add_bone(&mut editor.document, [0.0, 0.0], [10.0, 0.0], "Parent");
        let child = add_bone(&mut editor.document, [20.0, 0.0], [20.0, 10.0], "Child");
        let child = move_bone_into(&mut editor.document, &child, &parent).unwrap();
        assert_eq!(
            bone_at(&editor.document, &child).unwrap().head,
            Some([20.0, 0.0])
        );

        // Reconnecting to the current parent only omits the head; the tree is
        // unchanged.
        assert!(!finish_endpoint_drag(
            &mut editor,
            &child,
            BoneEnd::Head,
            [10.5, 0.5],
            1.0
        ));
        assert_eq!(bone_at(&editor.document, &child).unwrap().head, None);
        assert_eq!(
            bone_endpoints(&editor.document, &child),
            Some(([10.0, 0.0], [20.0, 10.0]))
        );
    }

    #[test]
    fn reparenting_keeps_the_stored_points() {
        let mut document = Document::new();
        let parent = add_bone(&mut document, [0.0, 0.0], [10.0, 0.0], "Parent");
        let child = add_bone(&mut document, [40.0, 10.0], [50.0, 10.0], "Child");

        let moved = move_bone_into(&mut document, &child, &parent).unwrap();
        assert_eq!(moved.chain(), &[0, 0]);
        assert_eq!(
            bone_endpoints(&document, &moved),
            Some(([40.0, 10.0], [50.0, 10.0]))
        );

        // Rig-mode edits stay local: moving the parent leaves the child alone.
        translate_bone(&mut document, &parent, [5.0, 0.0]);
        assert_eq!(
            bone_endpoints(&document, &moved),
            Some(([40.0, 10.0], [50.0, 10.0]))
        );
    }

    #[test]
    fn a_bone_cannot_be_moved_inside_itself() {
        let mut document = Document::new();
        let parent = add_bone(&mut document, [0.0, 0.0], [10.0, 0.0], "Parent");
        let raw_child = add_bone(&mut document, [0.0, 0.0], [5.0, 0.0], "Child");
        let child = move_bone_into(&mut document, &raw_child, &parent).unwrap();

        assert!(move_bone_into(&mut document, &parent, &child).is_none());
    }

    #[test]
    fn bones_round_trip_through_json() {
        let mut document = document_with_bones();
        let parent = address(&[0]);
        let child = add_bone(&mut document, [10.0, 0.0], [20.0, 0.0], "Child");
        let child = move_bone_into(&mut document, &child, &parent).unwrap();
        assert_eq!(child.chain(), &[0, 0]);

        let json = serde_json::to_string(&document).unwrap();
        let loaded: Document = serde_json::from_str(&json).unwrap();
        assert_eq!(loaded, document);
    }

    #[test]
    fn replacing_the_document_clears_rig_editing() {
        let mut editor = EditorState::new(Document::new());
        editor.rig.chain = Some(PenChain {
            last: [0.0, 0.0],
            parent: None,
        });
        editor.rig.preview = Some(([0.0, 0.0], [10.0, 10.0]));

        editor.replace_document(Document::new());

        assert!(editor.rig.chain.is_none());
        assert!(editor.rig.preview().is_none());
    }

    #[test]
    fn selecting_a_bone_clears_any_in_progress_gesture() {
        let mut editor = EditorState::new(document_with_bones());
        editor.rig.drag = Some(BoneDrag::Bulk {
            address: address(&[0]),
            last: [0.0, 0.0],
        });

        editor.select_bone(Some(address(&[0])));

        assert!(editor.rig.drag.is_none());
    }

    #[test]
    fn new_bones_get_unique_names() {
        let mut document = Document::new();
        assert_eq!(unique_bone_name(&document), "Bone");

        let name = unique_bone_name(&document);
        add_bone(&mut document, [0.0, 0.0], [1.0, 0.0], name);
        assert_eq!(unique_bone_name(&document), "Bone 2");

        let name = unique_bone_name(&document);
        add_bone(&mut document, [0.0, 0.0], [1.0, 0.0], name);
        assert_eq!(unique_bone_name(&document), "Bone 3");
    }

    #[test]
    fn deleting_a_bone_drops_it_from_weights_and_keeps_the_total() {
        let mut editor = EditorState::new(Document::new());
        let bone = add_bone(&mut editor.document, [0.0, 0.0], [1.0, 0.0], "Arm");
        editor.document.shapes.push(shape_with_weights(
            &[("Arm", 0.6), ("Leg", 0.4)],
            &[("Arm", 1.0)],
        ));
        editor.select_bone(Some(bone));

        assert!(delete_selected_bone(&mut editor));

        let path = &editor.document.shapes[0].paths[0];
        let start = &path.start_weights;
        assert_eq!(start.get("Arm"), None);
        assert!((start.sum() - 1.0).abs() < 1e-6);
        assert!((start.get("Leg").unwrap() - 1.0).abs() < 1e-6);

        // A point weighted entirely by the removed bone has nothing left to
        // preserve, so it simply empties.
        assert!(path.segments[0].to_weights.is_empty());
    }

    #[test]
    fn renaming_a_bone_renames_it_in_weights() {
        let mut editor = EditorState::new(Document::new());
        let bone = add_bone(&mut editor.document, [0.0, 0.0], [1.0, 0.0], "Arm");
        editor
            .document
            .shapes
            .push(shape_with_weights(&[("Arm", 0.5)], &[("Arm", 1.0)]));

        assert!(rename_bone_at(
            &mut editor.document,
            &bone,
            "Limb".to_owned()
        ));

        let path = &editor.document.shapes[0].paths[0];
        assert_eq!(path.start_weights.get("Arm"), None);
        assert_eq!(path.start_weights.get("Limb"), Some(0.5));
        assert_eq!(path.segments[0].to_weights.get("Limb"), Some(1.0));
    }
}
