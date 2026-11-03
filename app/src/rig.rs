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
//!
//! Animate mode reuses the pose tools: dragging a bone there keys its rotation,
//! shift-drag keys its translation, and ctrl-drag keys its scale, writing the
//! values into the selected animation at the playhead.

use bevy::prelude::*;
use bevy::window::PrimaryWindow;
use bevy_vello::prelude::vello;
use vello::Scene;
use vello::kurbo::{Affine, BezPath, Stroke as KurboStroke};
use vello::peniko::{Color, Fill};

use crate::animate::{sample_pose, set_bone_keyframe};
use crate::editor::{
    CONTENT_TOP, CanvasInput, DocumentBounds, EditorMode, EditorState, SIDEBAR_WIDTH, Tool,
    ToolState, ViewMapping, Viewport, over_ui,
};
use crate::menu_bar::MenuState;
use crate::model::{Bone, BoneProperty, Document, Point};
use crate::pose::{BonePose, Pose, Similarity};
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

/// Smallest scale a pose drag can shrink a bone to, so it never collapses to
/// zero and becomes impossible to grab again.
const MIN_POSE_SCALE: f32 = 0.05;

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
    Bulk {
        address: BoneAddress,
        last: Point,
        /// The inverse of the bone's world transform, which maps a world-space
        /// drag back into the rest frame the document stores.
        inverse: Similarity,
    },
    /// Reshaping one endpoint.
    Endpoint {
        address: BoneAddress,
        end: BoneEnd,
        /// Where the press landed, so a click alone does not snap.
        origin: Point,
    },
}

/// What a pose drag edits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PoseKind {
    /// Turn the bone about its head.
    Rotate,
    /// Offset the bone in its parent's frame.
    Translate,
    /// Grow or shrink the bone about its head.
    Scale,
}

/// A pose drag in progress.
#[derive(Clone, Debug)]
struct PoseDrag {
    address: BoneAddress,
    kind: PoseKind,
    /// The bone's pose when the press landed; each frame's pose is derived
    /// from it so a drag does not accumulate rounding error.
    start_pose: BonePose,
    /// The inverse of the parent's world transform, mapping a world-space
    /// translation back into the parent frame the pose stores.
    parent_inverse: Similarity,
    /// The posed head, the pivot for rotation and scale.
    pivot: Point,
    /// The cursor at the press, the reference for the drag's angle and
    /// distance.
    start_cursor: Point,
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
    /// The pose drag in progress, if any.
    pose_drag: Option<PoseDrag>,
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
        self.pose_drag = None;
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
fn parent_world(document: &Document, pose: &Pose, address: &BoneAddress) -> Similarity {
    match address.chain().split_last() {
        Some((_, parents)) if !parents.is_empty() => world_at(document, pose, parents),
        _ => Similarity::IDENTITY,
    }
}

// ---------------------------------------------------------------------------
// Geometry
// ---------------------------------------------------------------------------

fn distance(a: Point, b: Point) -> f32 {
    (a[0] - b[0]).hypot(a[1] - b[1])
}

/// The vector from `from` to `to`.
fn offset(from: Point, to: Point) -> Point {
    [to[0] - from[0], to[1] - from[1]]
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

/// Records the live pivot of every connected bone in a subtree on its stored
/// pose, leaving the drawn transform unchanged.
///
/// A connected bone pivots about its current rest head and ignores the stored
/// [`pivot`](BonePose::pivot). A body drag materializes heads—the dragged
/// bone's own and those of children it detaches—switching these bones to the
/// frozen-pivot rule. Storing the live pivot first means the switch keeps the
/// transform identical instead of snapping the bone to a stale pivot.
fn freeze_connected_pivots(editor: &mut EditorState, address: &BoneAddress) {
    fn collect(bones: &[Bone], parent_tail: Option<Point>, out: &mut Vec<(String, Point)>) {
        for bone in bones {
            if bone.head.is_none() && !bone.name.is_empty() {
                out.push((bone.name.clone(), bone.head_with_parent(parent_tail)));
            }
            collect(&bone.children, Some(bone.tail), out);
        }
    }

    let Some(bone) = bone_at(&editor.document, address) else {
        return;
    };
    let mut targets = Vec::new();
    if bone.head.is_none() && !bone.name.is_empty() {
        targets.push((
            bone.name.clone(),
            bone.head_with_parent(parent_tail(&editor.document, address)),
        ));
    }
    collect(&bone.children, Some(bone.tail), &mut targets);

    for (name, rest_head) in targets {
        let mut pose = editor.pose.get(&name);
        if pose.pivot != rest_head {
            pose.pivot = rest_head;
            editor.pose.set(name, pose);
        }
    }
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
        document.rename_bone_in_animations(&old, &name);
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
    pose: &Pose,
    point: Point,
    tolerance: f32,
    exclude: &BoneAddress,
) -> Option<BoneAddress> {
    #[allow(clippy::too_many_arguments)]
    fn walk(
        bones: &[Bone],
        prefix: &mut Vec<usize>,
        parent_tail: Option<Point>,
        parent_world: Similarity,
        point: Point,
        tolerance: f32,
        exclude: &BoneAddress,
        pose: &Pose,
        best: &mut Option<(f32, BoneAddress)>,
    ) {
        for (index, bone) in bones.iter().enumerate() {
            prefix.push(index);
            let address = BoneAddress {
                bones: prefix.clone(),
            };
            let rest_head = bone.head_with_parent(parent_tail);
            let local = pose.get(&bone.name).local(rest_head, bone.head.is_none());
            let world = Similarity::compose(parent_world, local);
            // A bone cannot attach to itself or to one of its descendants.
            if !address_is_inside(exclude, &address) {
                // Snap against where the joint is drawn, not its rest position.
                let distance = distance(point, world.apply(bone.tail));
                if distance <= tolerance && best.as_ref().is_none_or(|(best, _)| distance < *best) {
                    *best = Some((distance, address.clone()));
                }
            }
            walk(
                &bone.children,
                prefix,
                Some(bone.tail),
                world,
                point,
                tolerance,
                exclude,
                pose,
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
        Similarity::IDENTITY,
        point,
        tolerance,
        exclude,
        pose,
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
    pose: &Pose,
    point: Point,
    tolerance: f32,
    exclude: &BoneAddress,
) -> Option<(BoneAddress, Point)> {
    #[allow(clippy::too_many_arguments)]
    fn walk(
        bones: &[Bone],
        prefix: &mut Vec<usize>,
        parent_tail: Option<Point>,
        parent_world: Similarity,
        point: Point,
        tolerance: f32,
        exclude: &BoneAddress,
        pose: &Pose,
        best: &mut Option<(f32, BoneAddress, Point)>,
    ) {
        for (index, bone) in bones.iter().enumerate() {
            prefix.push(index);
            let address = BoneAddress {
                bones: prefix.clone(),
            };
            let rest_head = bone.head_with_parent(parent_tail);
            let local = pose.get(&bone.name).local(rest_head, bone.head.is_none());
            let world = Similarity::compose(parent_world, local);
            // A head resting on its parent's tail is that shared joint, not a
            // separate head: the parent's tail owns it, so it must not be the
            // target of a tail-to-head connect.
            if let Some(head) = bone.head
                && Some(head) != parent_tail
                && !address_is_inside(&address, exclude)
            {
                // Snap against where the head is drawn, not its rest position.
                let distance = distance(point, world.apply(head));
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
                world,
                point,
                tolerance,
                exclude,
                pose,
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
        Similarity::IDENTITY,
        point,
        tolerance,
        exclude,
        pose,
        &mut best,
    );
    best.map(|(_, address, head)| (address, head))
}

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

/// Maps a posed cursor position back onto the rest geometry an endpoint edits.
///
/// The bone's rest endpoints are each moved by its full world transform, so
/// inverting that transform makes the dragged endpoint land precisely where the
/// cursor is without scaling the drag. The transform is fixed for the duration
/// of the drag—a stored-head bone pivots about its frozen pivot and a connected
/// bone about its head (the parent's tail, which the drag does not move)—so the
/// other endpoint stays put.
fn endpoint_rest_point(
    document: &Document,
    pose: &Pose,
    address: &BoneAddress,
    posed: Point,
) -> Point {
    bone_world(document, pose, address).inverse().apply(posed)
}

/// Prepares a body drag of `address`, returning the inverse transform that
/// maps a world-space drag back into the rest frame.
///
/// The drag materializes connected heads, so their live pivots are frozen
/// first; otherwise a posed bone would snap to a stale pivot. The bone's own
/// world transform then moves its rest geometry rigidly, so its inverse is the
/// exact mapping back to rest.
fn begin_bulk_drag(editor: &mut EditorState, address: &BoneAddress) -> Similarity {
    freeze_connected_pivots(editor, address);
    bone_world(&editor.document, &editor.pose, address).inverse()
}

/// One bone's drawn head and tail under a pose.
#[derive(Clone, Copy, Debug)]
struct PosedEnds {
    head: Point,
    tail: Point,
}

/// Records the drawn head and tail of every bone in `bones`, in pre-order.
fn capture_posed(
    bones: &[Bone],
    pose: &Pose,
    parent_tail: Option<Point>,
    parent_world: Similarity,
    out: &mut Vec<PosedEnds>,
) {
    for bone in bones {
        let rest_head = bone.head_with_parent(parent_tail);
        let local = pose.get(&bone.name).local(rest_head, bone.head.is_none());
        let world = Similarity::compose(parent_world, local);
        out.push(PosedEnds {
            head: world.apply(rest_head),
            tail: world.apply(bone.tail),
        });
        capture_posed(&bone.children, pose, Some(bone.tail), world, out);
    }
}

/// The drawn endpoints of the subtree at `address`, plus the parent frame they
/// hang from, so compensation can be driven from the same frame afterwards.
fn capture_subtree(
    document: &Document,
    pose: &Pose,
    address: &BoneAddress,
) -> Option<(Vec<PosedEnds>, Option<Point>, Similarity)> {
    let frame_tail = parent_tail(document, address);
    let frame_world = parent_world(document, pose, address);
    let bone = bone_at(document, address)?;
    let mut posed = Vec::new();
    capture_posed(
        std::slice::from_ref(bone),
        pose,
        frame_tail,
        frame_world,
        &mut posed,
    );
    Some((posed, frame_tail, frame_world))
}

/// Restores the drawn endpoints listed in `old` by editing rest geometry only.
///
/// `old` lists the subtree in pre-order, matching the order of `bones`, so each
/// entry names the same bone before and after a reparent. The transient pose is
/// never written: a stored head is moved to place the bone's drawn head, and
/// any bone's tail is moved to place its drawn tail.
///
/// A connected head is the parent's live tail joint and cannot be moved from
/// here, but preserving the parent's drawn tail keeps it put too. That is why
/// the recursion is top-down: once a parent's drawn tail is restored, its
/// connected children's heads—and hence their worlds—are stable, so their own
/// tails can be restored in turn.
///
/// `skip_head` and `skip_tail` mark the subtree root's endpoints that the edit
/// is allowed to move: the dragged joint or a joint that just became shared.
#[allow(clippy::too_many_arguments)]
fn compensate_posed(
    bones: &mut [Bone],
    pose: &Pose,
    parent_tail: Option<Point>,
    parent_world: Similarity,
    old: &[PosedEnds],
    cursor: &mut usize,
    skip_head: bool,
    skip_tail: bool,
) {
    for bone in bones {
        let index = *cursor;
        *cursor += 1;
        let rest_head = bone.head_with_parent(parent_tail);
        let local = pose.get(&bone.name).local(rest_head, bone.head.is_none());
        let world = Similarity::compose(parent_world, local);

        if let Some(old) = old.get(index).copied() {
            let keep_tail = index == 0 && skip_tail;
            let keep_head = index == 0 && skip_head;
            if !keep_tail {
                bone.tail = world.inverse().apply(old.tail);
            }
            // A connected head cannot be placed from here; it is the parent's
            // tail joint, which the top-down order has already restored.
            if bone.head.is_some() && !keep_head {
                bone.head = Some(world.inverse().apply(old.head));
            }
        }

        compensate_posed(
            &mut bone.children,
            pose,
            Some(bone.tail),
            world,
            old,
            cursor,
            false,
            false,
        );
    }
}

/// Applies [`compensate_posed`] to the subtree rooted at `address`.
#[allow(clippy::too_many_arguments)]
fn compensate_subtree(
    document: &mut Document,
    pose: &Pose,
    address: &BoneAddress,
    parent_tail: Option<Point>,
    parent_world: Similarity,
    old: &[PosedEnds],
    skip_head: bool,
    skip_tail: bool,
) {
    let Some(bone) = bone_at_mut(document, address) else {
        return;
    };
    let mut cursor = 0;
    compensate_posed(
        std::slice::from_mut(bone),
        pose,
        parent_tail,
        parent_world,
        old,
        &mut cursor,
        skip_head,
        skip_tail,
    );
}

/// Reshapes one endpoint, keeping every other drawn endpoint where it was.
///
/// A stored head belongs to the bone itself: its world transform never depends
/// on the head, so moving the head moves nothing else and needs no
/// compensation. A tail is shared with connected children, so moving it swings
/// their bodies; afterwards the subtree's saved drawn endpoints are restored by
/// rewriting rest geometry—never the transient pose.
fn drag_endpoint_keeping_children(
    editor: &mut EditorState,
    address: &BoneAddress,
    end: BoneEnd,
    rest: Point,
) {
    match end {
        BoneEnd::Head => {
            drag_bone_endpoint(&mut editor.document, address, end, rest);
        }
        BoneEnd::Tail => {
            let Some((old, frame_tail, frame_world)) =
                capture_subtree(&editor.document, &editor.pose, address)
            else {
                return;
            };
            if !drag_bone_endpoint(&mut editor.document, address, end, rest) {
                return;
            }
            compensate_subtree(
                &mut editor.document,
                &editor.pose,
                address,
                frame_tail,
                frame_world,
                &old,
                false,
                true,
            );
        }
    }
}

/// Joins a released endpoint to a matching joint, so dragging a tail onto a
/// head (or a head onto a tail) connects the two bones.
///
/// `cursor` is in posed world space: the nearest joint is found where the pose
/// draws it, so snapped bones meet at the joint the user sees even when the rig
/// is posed. The reparenting itself still edits the rest geometry.
///
/// Returns whether the tree changed, so callers can bump the structure
/// revision.
pub fn finish_endpoint_drag(
    editor: &mut EditorState,
    address: &BoneAddress,
    end: BoneEnd,
    cursor: Point,
    snap: f32,
    pose: &Pose,
) -> bool {
    match end {
        BoneEnd::Head => {
            // A head snaps onto a tail, becoming that bone's child.
            let Some(target) = nearest_tail(&editor.document, pose, cursor, snap, address) else {
                return false;
            };
            if parent_address(address).as_ref() == Some(&target) {
                // Reconnecting to the current parent only omits the head. The
                // bone switches from its frozen pivot to the parent's live tail
                // joint, so its drawn tail would jump; restore it in rest
                // geometry before returning.
                if bone_at(&editor.document, address).is_some_and(|bone| bone.head.is_none()) {
                    return false;
                }
                let Some((old, frame_tail, frame_world)) =
                    capture_subtree(&editor.document, pose, address)
                else {
                    return false;
                };
                let Some(bone) = bone_at_mut(&mut editor.document, address) else {
                    return false;
                };
                bone.head = None;
                compensate_subtree(
                    &mut editor.document,
                    pose,
                    address,
                    frame_tail,
                    frame_world,
                    &old,
                    true,
                    false,
                );
                return false;
            }
            // Re-parenting the bone under the target points its head at the
            // target's tail joint, moving its whole subtree. Capture the drawn
            // endpoints first so rest geometry can be adjusted to keep them.
            let Some((old, _, _)) = capture_subtree(&editor.document, pose, address) else {
                return false;
            };
            let Some(new_address) = move_bone_into(&mut editor.document, address, &target) else {
                return false;
            };
            if let Some(bone) = bone_at_mut(&mut editor.document, &new_address) {
                bone.head = None;
            }
            let frame_tail = parent_tail(&editor.document, &new_address);
            let frame_world = parent_world(&editor.document, pose, &new_address);
            compensate_subtree(
                &mut editor.document,
                pose,
                &new_address,
                frame_tail,
                frame_world,
                &old,
                true,
                false,
            );
            editor.select_bone(Some(new_address));
            true
        }
        BoneEnd::Tail => {
            // A tail snaps onto a stored head, taking that bone as a child.
            let Some((target, _)) = nearest_head(&editor.document, pose, cursor, snap, address)
            else {
                return false;
            };
            // The target's drawn head is the joint the cursor landed on. The
            // dragged tail should meet it there, not at its rest position.
            let Some((target_old, _, _)) = capture_subtree(&editor.document, pose, &target) else {
                return false;
            };
            let world_joint = target_old.first().map(|ends| ends.head).unwrap_or(cursor);
            // Capture the dragged subtree before its tail moves, so its own
            // children can be kept in place.
            let Some((dragged_old, dragged_tail, dragged_world)) =
                capture_subtree(&editor.document, pose, address)
            else {
                return false;
            };
            // Placing the rest tail here puts the drawn tail on the joint.
            // Removing the target can shift the dragged bone's own index when
            // they are siblings, so capture its slot first.
            let source_slot = bone_slot(&editor.document, &target);
            let dragged_slot = bone_slot(&editor.document, address);
            let new_tail = bone_world(&editor.document, pose, address)
                .inverse()
                .apply(world_joint);
            if !drag_bone_endpoint(&mut editor.document, address, BoneEnd::Tail, new_tail) {
                return false;
            }
            compensate_subtree(
                &mut editor.document,
                pose,
                address,
                dragged_tail,
                dragged_world,
                &dragged_old,
                false,
                true,
            );

            let Some(new_target) = move_bone_into(&mut editor.document, &target, address) else {
                return false;
            };
            if let Some(bone) = bone_at_mut(&mut editor.document, &new_target) {
                bone.head = None;
            }
            // The target now pivots about the dragged bone's tail joint, so
            // restore its drawn geometry from the endpoints sampled earlier.
            let frame_tail = parent_tail(&editor.document, &new_target);
            let frame_world = parent_world(&editor.document, pose, &new_target);
            compensate_subtree(
                &mut editor.document,
                pose,
                &new_target,
                frame_tail,
                frame_world,
                &target_old,
                true,
                false,
            );
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

/// Returns the bone whose posed body is nearest to `point`, within
/// `tolerance`.
pub fn bone_near(
    document: &Document,
    pose: &Pose,
    point: Point,
    tolerance: f32,
) -> Option<BoneAddress> {
    #[allow(clippy::too_many_arguments)]
    fn walk(
        bones: &[Bone],
        prefix: &mut Vec<usize>,
        parent_tail: Option<Point>,
        parent_world: Similarity,
        point: Point,
        tolerance: f32,
        pose: &Pose,
        best: &mut Option<(f32, Vec<usize>)>,
    ) {
        for (index, bone) in bones.iter().enumerate() {
            let rest_head = bone.head_with_parent(parent_tail);
            let local = pose.get(&bone.name).local(rest_head, bone.head.is_none());
            let world = Similarity::compose(parent_world, local);
            let head = world.apply(rest_head);
            let tail = world.apply(bone.tail);
            let distance = distance_to_kite(point, head, tail);
            prefix.push(index);
            if distance <= tolerance && best.as_ref().is_none_or(|(best, _)| distance < *best) {
                *best = Some((distance, prefix.clone()));
            }
            walk(
                &bone.children,
                prefix,
                Some(bone.tail),
                world,
                point,
                tolerance,
                pose,
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
        Similarity::IDENTITY,
        point,
        tolerance,
        pose,
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
    pose: &Pose,
    point: Point,
    radius: f32,
) -> Option<(BoneAddress, BoneEnd, Point)> {
    #[allow(clippy::too_many_arguments)]
    fn walk(
        bones: &[Bone],
        prefix: &mut Vec<usize>,
        parent_tail: Option<Point>,
        parent_world: Similarity,
        point: Point,
        radius: f32,
        pose: &Pose,
        best: &mut Option<(f32, BoneAddress, BoneEnd, Point)>,
    ) {
        for (index, bone) in bones.iter().enumerate() {
            prefix.push(index);
            let address = BoneAddress {
                bones: prefix.clone(),
            };
            let rest_head = bone.head_with_parent(parent_tail);
            let local = pose.get(&bone.name).local(rest_head, bone.head.is_none());
            let world = Similarity::compose(parent_world, local);
            let posed_tail = world.apply(bone.tail);
            // Tails win ties so a shared joint resolves to the parent, which
            // is the bone that actually owns the point.
            let tail_distance = distance(point, posed_tail);
            if tail_distance <= radius
                && best
                    .as_ref()
                    .is_none_or(|(best, _, _, _)| tail_distance < *best)
            {
                *best = Some((tail_distance, address.clone(), BoneEnd::Tail, posed_tail));
            }
            // Only a stored head is its own handle; an omitted head is the
            // parent's tail joint. A head resting on the parent's tail is that
            // same shared joint, so it resolves to the parent.
            if let Some(head) = bone.head
                && Some(head) != parent_tail
            {
                let posed_head = world.apply(head);
                let head_distance = distance(point, posed_head);
                if head_distance <= radius
                    && best
                        .as_ref()
                        .is_none_or(|(best, _, _, _)| head_distance < *best)
                {
                    *best = Some((head_distance, address, BoneEnd::Head, posed_head));
                }
            }
            walk(
                &bone.children,
                prefix,
                Some(bone.tail),
                world,
                point,
                radius,
                pose,
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
        Similarity::IDENTITY,
        point,
        radius,
        pose,
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
    let old = bone_name_at(&editor.document, &renaming.address).map(str::to_owned);
    if rename_bone_at(&mut editor.document, &renaming.address, name.clone()) {
        // The pose is keyed by bone name, so it must follow the rename or the
        // bone would silently snap back to rest.
        if let Some(old) = old
            && old != name
        {
            editor.pose.rename(&old, &name);
        }
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
    // Animate mode reuses the pose gestures, keying their result into the
    // selected animation instead of the transient rig pose.
    if *mode == EditorMode::Animate {
        if menu.is_some_and(|menu| menu.is_open()) {
            if mouse.just_released(MouseButton::Left) {
                editor.rig.pose_drag = None;
            }
            return;
        }
        if editor.is_renaming() {
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
        handle_animate_bone(&mouse, &keys, &mut editor, point, pixel, over);
        return;
    }

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
        Tool::Pose => handle_bone_pose(&mouse, &keys, &mut editor, point, pixel, over),
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
        &editor.pose,
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
        let joint = endpoint_near(
            &editor.document,
            &editor.pose,
            point,
            pixel * BONE_HANDLE_HIT_RADIUS,
        );
        let body = bone_near(
            &editor.document,
            &editor.pose,
            point,
            pixel * BONE_HIT_SLACK,
        );
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
            // after it. The cursor is mapped back onto rest geometry as the
            // drag proceeds, since the skeleton is drawn posed.
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
            let inverse = begin_bulk_drag(editor, &address);
            editor.select_bone(Some(address.clone()));
            editor.rig.drag = Some(BoneDrag::Bulk {
                address,
                last: point,
                inverse,
            });
            return;
        }

        editor.select_bone(None);
        editor.rig.drag = None;
    }

    if mouse.pressed(MouseButton::Left) {
        match editor.rig.drag.clone() {
            Some(BoneDrag::Bulk {
                address,
                last,
                inverse,
            }) => {
                let delta = [point[0] - last[0], point[1] - last[1]];
                let rest_delta = inverse.apply_vector(delta);
                translate_bone(&mut editor.document, &address, rest_delta);
                editor.rig.drag = Some(BoneDrag::Bulk {
                    address,
                    last: point,
                    inverse,
                });
            }
            Some(BoneDrag::Endpoint { address, end, .. }) => {
                // The posed joint is what the cursor follows, so map it back to
                // rest before reshaping the stored endpoints.
                let rest = endpoint_rest_point(&editor.document, &editor.pose, &address, point);
                drag_endpoint_keeping_children(editor, &address, end, rest);
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
        {
            // Snap against the posed joints the cursor is aiming at, not their
            // rest positions.
            let pose = editor.pose.clone();
            if finish_endpoint_drag(
                editor,
                &address,
                end,
                point,
                pixel * BONE_CONNECT_SNAP,
                &pose,
            ) {
                editor.structure_revision += 1;
            }
        }
        editor.rig.drag = None;
    }
}

/// Pose tool: drag a bone to rotate it about its head, shift-drag to translate
/// it, and ctrl-drag to scale it.
///
/// The pose lives on the editor rather than the document, so it deforms the
/// rig page only and is never saved. Each bone's local transform stacks on top
/// of its ancestors', so posing a parent carries its children.
fn handle_bone_pose(
    mouse: &ButtonInput<MouseButton>,
    keys: &ButtonInput<KeyCode>,
    editor: &mut EditorState,
    point: Point,
    pixel: f32,
    over: bool,
) {
    if over {
        if mouse.just_released(MouseButton::Left) {
            editor.rig.pose_drag = None;
        }
        return;
    }

    if mouse.just_pressed(MouseButton::Left) {
        let Some(address) = bone_near(
            &editor.document,
            &editor.pose,
            point,
            pixel * BONE_HIT_SLACK,
        ) else {
            editor.select_bone(None);
            return;
        };
        // Poses are keyed by bone name, so an unnamed bone cannot be posed.
        let Some(name) = bone_name_at(&editor.document, &address) else {
            return;
        };
        if name.is_empty() {
            return;
        }

        let kind = pose_kind(keys);
        // The pose pivots about the bone's head. A stored head is the bone's
        // own geometry, so re-anchor the frozen pivot there; a connected head
        // is the parent's live tail and already pivots about the current head,
        // so its pose must not be re-anchored.
        let connected = bone_at(&editor.document, &address).is_some_and(|bone| bone.head.is_none());
        let (rest_head, pivot) = bone_endpoints(&editor.document, &address)
            .map(|(head, _)| {
                let posed = bone_world(&editor.document, &editor.pose, &address).apply(head);
                (head, posed)
            })
            .unwrap_or((point, point));
        let start_pose = if connected {
            editor.pose.get(name)
        } else {
            editor.pose.get(name).with_pivot(rest_head)
        };
        let parent_inverse = parent_world(&editor.document, &editor.pose, &address).inverse();

        // Selecting clears any previous gesture, so the drag is stored after it.
        editor.select_bone(Some(address.clone()));
        editor.rig.pose_drag = Some(PoseDrag {
            address,
            kind,
            start_pose,
            parent_inverse,
            pivot,
            start_cursor: point,
        });
        return;
    }

    if mouse.pressed(MouseButton::Left)
        && let Some(drag) = editor.rig.pose_drag.clone()
    {
        let pose = dragged_pose(&drag, point);
        if let Some(name) = bone_name_at(&editor.document, &drag.address) {
            let name = name.to_owned();
            editor.pose.set(name, pose);
        }
    }

    if mouse.just_released(MouseButton::Left) {
        editor.rig.pose_drag = None;
    }
}

/// Animate mode: drag a bone to key its pose at the playhead.
///
/// A plain drag keys rotation, shift-drag keys translation, and ctrl-drag keys
/// scale. A press that never moves only selects the bone, so a click does not
/// silently add keyframes.
fn handle_animate_bone(
    mouse: &ButtonInput<MouseButton>,
    keys: &ButtonInput<KeyCode>,
    editor: &mut EditorState,
    point: Point,
    pixel: f32,
    over: bool,
) {
    if over {
        if mouse.just_released(MouseButton::Left) {
            editor.rig.pose_drag = None;
        }
        return;
    }

    // The animate page shows the animation sampled at the playhead, so
    // hit-testing and drag origins use that pose rather than the rig pose.
    let pose = animate_pose(editor);

    if mouse.just_pressed(MouseButton::Left) {
        let Some(address) = bone_near(&editor.document, &pose, point, pixel * BONE_HIT_SLACK)
        else {
            editor.select_bone(None);
            return;
        };
        // Poses are keyed by bone name, so an unnamed bone cannot be animated.
        let Some(name) = bone_name_at(&editor.document, &address) else {
            return;
        };
        if name.is_empty() {
            return;
        }

        let kind = pose_kind(keys);
        let connected = bone_at(&editor.document, &address).is_some_and(|bone| bone.head.is_none());
        let (rest_head, pivot) = bone_endpoints(&editor.document, &address)
            .map(|(head, _)| {
                let posed = bone_world(&editor.document, &pose, &address).apply(head);
                (head, posed)
            })
            .unwrap_or((point, point));
        let start_pose = if connected {
            pose.get(name)
        } else {
            pose.get(name).with_pivot(rest_head)
        };
        let parent_inverse = parent_world(&editor.document, &pose, &address).inverse();

        // Selecting clears any previous gesture, so store the drag after it.
        editor.select_bone(Some(address.clone()));
        editor.rig.pose_drag = Some(PoseDrag {
            address,
            kind,
            start_pose,
            parent_inverse,
            pivot,
            start_cursor: point,
        });
        return;
    }

    if mouse.pressed(MouseButton::Left)
        && let Some(drag) = editor.rig.pose_drag.clone()
        && distance(drag.start_cursor, point) > pixel * BONE_ENDPOINT_CLICK_SLOP
    {
        let pose = dragged_pose(&drag, point);
        key_dragged_pose(editor, &drag, pose);
    }

    if mouse.just_released(MouseButton::Left) {
        editor.rig.pose_drag = None;
    }
}

/// The pose the animate page shows: the selected animation at the playhead.
fn animate_pose(editor: &EditorState) -> Pose {
    crate::animate::selected_animation(editor)
        .map(|animation| sample_pose(&editor.document, animation, editor.playhead))
        .unwrap_or_default()
}

/// Writes the properties `drag` affects into the selected animation at the
/// playhead, replacing an existing keyframe for the same target there.
fn key_dragged_pose(editor: &mut EditorState, drag: &PoseDrag, pose: BonePose) {
    let Some(index) = editor.selected_animation else {
        return;
    };
    let Some(name) = bone_name_at(&editor.document, &drag.address) else {
        return;
    };
    let name = name.to_owned();
    let time = editor.playhead;
    let Some(animation) = editor.document.animations.get_mut(index) else {
        return;
    };
    match drag.kind {
        PoseKind::Rotate => {
            set_bone_keyframe(animation, &name, BoneProperty::Angle, time, pose.rotation)
        }
        PoseKind::Translate => {
            set_bone_keyframe(animation, &name, BoneProperty::X, time, pose.translation[0]);
            set_bone_keyframe(animation, &name, BoneProperty::Y, time, pose.translation[1]);
        }
        PoseKind::Scale => {
            set_bone_keyframe(animation, &name, BoneProperty::Scale, time, pose.scale)
        }
    }
}

/// Which component a pose press edits, from the modifier keys.
fn pose_kind(keys: &ButtonInput<KeyCode>) -> PoseKind {
    let ctrl = keys.pressed(KeyCode::ControlLeft) || keys.pressed(KeyCode::ControlRight);
    let shift = keys.pressed(KeyCode::ShiftLeft) || keys.pressed(KeyCode::ShiftRight);
    if ctrl {
        PoseKind::Scale
    } else if shift {
        PoseKind::Translate
    } else {
        PoseKind::Rotate
    }
}

/// The pose `drag` produces with the cursor at `cursor`, derived from the pose
/// at the press so repeated frames do not accumulate error.
fn dragged_pose(drag: &PoseDrag, cursor: Point) -> BonePose {
    let mut pose = drag.start_pose;
    match drag.kind {
        PoseKind::Rotate => {
            let turn = angle(drag.pivot, cursor) - angle(drag.pivot, drag.start_cursor);
            pose.rotation = drag.start_pose.rotation + turn;
        }
        PoseKind::Translate => {
            let rest = drag
                .parent_inverse
                .apply_vector(offset(drag.start_cursor, cursor));
            pose.translation = [
                drag.start_pose.translation[0] + rest[0],
                drag.start_pose.translation[1] + rest[1],
            ];
        }
        PoseKind::Scale => {
            let start = distance(drag.pivot, drag.start_cursor);
            if start > f32::EPSILON {
                let factor = distance(drag.pivot, cursor) / start;
                pose.scale = (drag.start_pose.scale * factor).max(MIN_POSE_SCALE);
            }
        }
    }
    pose
}

/// The angle, in radians, from `origin` to `point`.
fn angle(origin: Point, point: Point) -> f32 {
    (point[1] - origin[1]).atan2(point[0] - origin[0])
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
    pose: &Pose,
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
        Similarity::IDENTITY,
        pose,
        selected,
        scale,
    );

    // Redraw the selection last so it is never hidden behind another bone.
    if let Some(address) = selected
        && let Some((rest_head, rest_tail)) = bone_endpoints(document, address)
    {
        let world = bone_world(document, pose, address);
        let head = world.apply(rest_head);
        let tail = world.apply(rest_tail);
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

#[allow(clippy::too_many_arguments)]
fn draw_bone_list(
    scene: &mut Scene,
    bones: &[Bone],
    prefix: &mut Vec<usize>,
    parent_tail: Option<Point>,
    parent_world: Similarity,
    pose: &Pose,
    selected: Option<&BoneAddress>,
    scale: f32,
) {
    for (index, bone) in bones.iter().enumerate() {
        prefix.push(index);
        let address = BoneAddress {
            bones: prefix.clone(),
        };
        let rest_head = bone.head_with_parent(parent_tail);
        let local = pose.get(&bone.name).local(rest_head, bone.head.is_none());
        let world = Similarity::compose(parent_world, local);
        let head = world.apply(rest_head);
        let tail = world.apply(bone.tail);
        // The selected bone is drawn later, on top.
        if selected != Some(&address) {
            draw_bone(
                scene,
                head,
                tail,
                scale,
                &BONE_FILL,
                &BONE_STROKE,
                OVERLAY_BONE_WIDTH,
            );
            // A connected bone shares its head joint with the parent's tail,
            // so only a stored head gets a handle of its own.
            draw_diamond(scene, tail, scale, OVERLAY_DIAMOND_RADIUS, &BONE_STROKE);
            if bone.head.is_some() && Some(rest_head) != parent_tail {
                draw_diamond(scene, head, scale, OVERLAY_DIAMOND_RADIUS, &BONE_STROKE);
            }
        }
        draw_bone_list(
            scene,
            &bone.children,
            prefix,
            Some(bone.tail),
            world,
            pose,
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
    use crate::model::{
        Animation, BoneProperty, FORMAT_VERSION, Keyframe, KeyframeTarget, Path, Shape,
    };

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
            animations: Vec::new(),
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

        // A connected child shares the parent's tail joint, so moving the tail
        // carries the child's head with it while its own tail stays put.
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
            endpoint_near(&document, &Pose::default(), [10.0, 0.0], 1.0),
            Some((parent.clone(), BoneEnd::Tail, [10.0, 0.0]))
        );
    }

    #[test]
    fn a_loose_head_is_its_own_handle() {
        let mut document = Document::new();
        add_bone(&mut document, [0.0, 0.0], [10.0, 0.0], "Bone");

        assert_eq!(
            endpoint_near(&document, &Pose::default(), [0.0, 0.0], 1.0),
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
            2.0,
            &Pose::default()
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
            2.0,
            &Pose::default()
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
            2.0,
            &Pose::default()
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
    fn snapping_a_tail_keeps_the_attached_bones_drawn_tail_fixed() {
        let mut editor = EditorState::new(Document::new());
        let dragged = add_bone(&mut editor.document, [0.0, 0.0], [10.0, 0.0], "Dragged");
        let host = add_bone(&mut editor.document, [20.0, 0.0], [30.0, 0.0], "Host");
        // Pose both so only the shared joint may move, not the attached body.
        editor.pose.set(
            "Dragged",
            BonePose {
                rotation: 0.3,
                ..Default::default()
            },
        );
        editor.pose.set(
            "Host",
            BonePose {
                rotation: 0.8,
                scale: 1.2,
                ..Default::default()
            },
        );

        let (host_head, host_tail) = bone_endpoints(&editor.document, &host).unwrap();
        let host_world = bone_world(&editor.document, &editor.pose, &host);
        let drawn_tail_before = host_world.apply(host_tail);
        let cursor = host_world.apply(host_head);

        let pose = editor.pose.clone();
        assert!(finish_endpoint_drag(
            &mut editor,
            &dragged,
            BoneEnd::Tail,
            cursor,
            1.0,
            &pose
        ));

        // The host is now a child of the dragged bone, but its drawn tail must
        // stay where it was; only rest geometry changed.
        let attached = address(&[0, 0]);
        assert_eq!(bone_name_at(&editor.document, &attached), Some("Host"));
        let (_, tail) = bone_endpoints(&editor.document, &attached).unwrap();
        let world = bone_world(&editor.document, &editor.pose, &attached);
        assert_point(world.apply(tail), drawn_tail_before);
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
        assert_eq!(
            bone_near(&document, &Pose::default(), [18.0, 10.0], 1.0),
            Some(address)
        );
        // Far outside the drawn shape nothing is hit.
        assert_eq!(
            bone_near(&document, &Pose::default(), [18.0, 30.0], 1.0),
            None
        );
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

        // Every connected child shares the moved joint, so all their heads
        // follow it and the joint never comes apart.
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
        // A stored head sitting on the old tail is the shared joint, so it is
        // folded back into an omitted head and follows the tail.
        assert_eq!(bone_at(&document, &child).unwrap().head, None);
        let (head, _) = bone_endpoints(&document, &child).unwrap();
        assert_point(head, [20.0, 0.0]);
    }

    #[test]
    fn dragging_a_scaled_bones_head_follows_the_cursor() {
        let mut editor = EditorState::new(Document::new());
        let bone = add_bone(&mut editor.document, [0.0, 0.0], [10.0, 0.0], "Bone");
        // Scaling about the head must not damp the head drag.
        editor.pose.set(
            "Bone",
            BonePose {
                scale: 2.0,
                ..Default::default()
            },
        );

        let rest = endpoint_rest_point(&editor.document, &editor.pose, &bone, [4.0, 3.0]);
        drag_bone_endpoint(&mut editor.document, &bone, BoneEnd::Head, rest);

        // The posed head lands exactly on the cursor and the posed tail stays
        // where the scale put it.
        let posed = |point: Point| bone_world(&editor.document, &editor.pose, &bone).apply(point);
        let (head, tail) = bone_endpoints(&editor.document, &bone).unwrap();
        assert_point(posed(head), [4.0, 3.0]);
        assert_point(posed(tail), [20.0, 0.0]);
    }

    #[test]
    fn dragging_a_posed_shared_joint_keeps_the_chain_connected() {
        let mut editor = EditorState::new(Document::new());
        let parent = add_bone(&mut editor.document, [0.0, 0.0], [10.0, 0.0], "Parent");
        let child = add_child_bone(
            &mut editor.document,
            &parent,
            [10.0, 0.0],
            [20.0, 0.0],
            "Child",
        )
        .unwrap();
        // Pose both bones so the child's frozen body must still track the joint.
        editor.pose.set(
            "Parent",
            BonePose {
                rotation: 0.4,
                ..Default::default()
            },
        );
        editor.pose.set(
            "Child",
            BonePose {
                rotation: 0.7,
                ..Default::default()
            },
        );

        // The parent's tail is the child's shared head joint.
        assert!(drag_bone_endpoint(
            &mut editor.document,
            &parent,
            BoneEnd::Tail,
            [12.0, 3.0]
        ));

        let posed = |address: &BoneAddress, point: Point| {
            bone_world(&editor.document, &editor.pose, address).apply(point)
        };
        let (_, parent_tail) = bone_endpoints(&editor.document, &parent).unwrap();
        let (child_head, _) = bone_endpoints(&editor.document, &child).unwrap();
        // The child's drawn head stays on the parent's drawn tail.
        assert_point(posed(&child, child_head), posed(&parent, parent_tail));
    }

    #[test]
    fn dragging_a_posed_shared_joint_keeps_the_childs_tail_fixed() {
        let mut editor = EditorState::new(Document::new());
        let parent = add_bone(&mut editor.document, [0.0, 0.0], [10.0, 0.0], "Parent");
        let child = add_child_bone(
            &mut editor.document,
            &parent,
            [10.0, 0.0],
            [10.0, 10.0],
            "Child",
        )
        .unwrap();
        editor.pose.set(
            "Parent",
            BonePose {
                rotation: 0.4,
                ..Default::default()
            },
        );
        editor.pose.set(
            "Child",
            BonePose {
                rotation: 0.7,
                ..Default::default()
            },
        );

        let posed = |editor: &EditorState, address: &BoneAddress, point: Point| {
            bone_world(&editor.document, &editor.pose, address).apply(point)
        };
        let (_, child_tail) = bone_endpoints(&editor.document, &child).unwrap();
        let before = posed(&editor, &child, child_tail);

        // Drag the shared joint through the editor path, which edits the
        // child's rest geometry to compensate.
        drag_endpoint_keeping_children(&mut editor, &parent, BoneEnd::Tail, [12.0, 3.0]);

        let (_, parent_tail) = bone_endpoints(&editor.document, &parent).unwrap();
        let (child_head, child_tail) = bone_endpoints(&editor.document, &child).unwrap();
        // The head stays on the moved joint while the tail stays drawn where it
        // was, so no other endpoint moved.
        assert_point(
            posed(&editor, &child, child_head),
            posed(&editor, &parent, parent_tail),
        );
        assert_point(posed(&editor, &child, child_tail), before);
    }

    #[test]
    fn bulk_dragging_a_posed_connected_child_moves_only_that_bone() {
        let mut editor = EditorState::new(Document::new());
        let parent = add_bone(&mut editor.document, [0.0, 0.0], [10.0, 0.0], "Parent");
        let child = add_child_bone(
            &mut editor.document,
            &parent,
            [10.0, 0.0],
            [10.0, 10.0],
            "Child",
        )
        .unwrap();
        editor.pose.set(
            "Parent",
            BonePose {
                rotation: 0.4,
                ..Default::default()
            },
        );
        editor.pose.set(
            "Child",
            BonePose {
                rotation: 0.9,
                scale: 1.5,
                ..Default::default()
            },
        );

        let posed = |editor: &EditorState, point: Point| {
            bone_world(&editor.document, &editor.pose, &child).apply(point)
        };
        let (head, tail) = bone_endpoints(&editor.document, &child).unwrap();
        let head_before = posed(&editor, head);
        let tail_before = posed(&editor, tail);

        // Starting the drag must not change the drawn bone.
        let inverse = begin_bulk_drag(&mut editor, &child);
        assert_point(posed(&editor, head), head_before);
        assert_point(posed(&editor, tail), tail_before);

        // A world-space drag then moves both drawn endpoints by exactly that
        // vector, rather than flinging the bone to a stale pose pivot.
        let world_delta = [3.0, -4.0];
        let rest_delta = inverse.apply_vector(world_delta);
        translate_bone(&mut editor.document, &child, rest_delta);

        let (head, tail) = bone_endpoints(&editor.document, &child).unwrap();
        assert_point(
            posed(&editor, head),
            [
                head_before[0] + world_delta[0],
                head_before[1] + world_delta[1],
            ],
        );
        assert_point(
            posed(&editor, tail),
            [
                tail_before[0] + world_delta[0],
                tail_before[1] + world_delta[1],
            ],
        );
    }

    #[test]
    fn snapping_uses_posed_joint_positions() {
        let mut editor = EditorState::new(Document::new());
        let dragged = add_bone(&mut editor.document, [40.0, 40.0], [50.0, 40.0], "Dragged");
        add_bone(&mut editor.document, [0.0, 0.0], [10.0, 0.0], "Host");
        // A quarter turn about the host's head draws its tail at [0, 10].
        editor.pose.set(
            "Host",
            BonePose {
                rotation: std::f32::consts::FRAC_PI_2,
                ..Default::default()
            },
        );

        // A cursor on the drawn tail must still find it, even though the host's
        // rest tail is at [10, 0].
        let pose = editor.pose.clone();
        assert!(finish_endpoint_drag(
            &mut editor,
            &dragged,
            BoneEnd::Head,
            [0.5, 9.5],
            2.0,
            &pose
        ));

        // Removing the dragged bone shifts the host to the front, so the
        // dragged bone lands as its first child.
        let attached = editor
            .selected_bone
            .clone()
            .expect("the bone stays selected");
        assert_eq!(attached.chain(), &[0, 0]);
        assert_eq!(bone_at(&editor.document, &attached).unwrap().head, None);
        // The head joins the host's drawn tail, while the dragged bone's own
        // drawn tail stays where it was; only rest geometry changed to make
        // that so.
        let world = bone_world(&editor.document, &editor.pose, &attached);
        let (head, tail) = bone_endpoints(&editor.document, &attached).unwrap();
        assert_point(world.apply(head), [0.0, 10.0]);
        assert_point(world.apply(tail), [50.0, 40.0]);
    }

    #[test]
    fn dragging_a_posed_parents_tail_leaves_the_child_posed_where_it_was() {
        let mut editor = EditorState::new(Document::new());
        let parent = add_bone(&mut editor.document, [0.0, 0.0], [10.0, 0.0], "Parent");
        let child = add_child_bone(
            &mut editor.document,
            &parent,
            [10.0, 0.0],
            [10.0, 10.0],
            "Child",
        )
        .unwrap();
        editor.pose.set(
            "Parent",
            BonePose {
                rotation: std::f32::consts::FRAC_PI_2,
                ..Default::default()
            },
        );

        let posed_child_tail = |document: &Document, pose: &Pose| {
            let (_, tail) = bone_endpoints(document, &child).unwrap();
            bone_world(document, pose, &child).apply(tail)
        };
        let before = posed_child_tail(&editor.document, &editor.pose);

        assert!(drag_bone_endpoint(
            &mut editor.document,
            &parent,
            BoneEnd::Tail,
            [0.0, 10.0]
        ));

        // The child keeps its rest geometry, so its posed tail stays put even
        // though its parent's tail moved.
        let after = posed_child_tail(&editor.document, &editor.pose);
        assert_point(after, before);
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
            endpoint_near(&document, &Pose::default(), [10.0, 0.0], 1.0),
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
            1.0,
            &Pose::default()
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
            inverse: Similarity::IDENTITY,
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

    /// Rest and posed head/tail of every bone, in document order.
    fn posed_endpoints(document: &Document, pose: &Pose) -> Vec<(Point, Point)> {
        fn walk(
            document: &Document,
            bones: &[Bone],
            prefix: &mut Vec<usize>,
            pose: &Pose,
            out: &mut Vec<(Point, Point)>,
        ) {
            for (index, bone) in bones.iter().enumerate() {
                prefix.push(index);
                let address = BoneAddress {
                    bones: prefix.clone(),
                };
                let world = bone_world(document, pose, &address);
                let (head, tail) = bone_endpoints(document, &address).unwrap();
                out.push((head, world.apply(head)));
                out.push((tail, world.apply(tail)));
                walk(document, &bone.children, prefix, pose, out);
                prefix.pop();
            }
        }
        let mut out = Vec::new();
        walk(document, &document.bones, &mut Vec::new(), pose, &mut out);
        out
    }

    /// Dragging `end` of `address` through the editor path must leave every
    /// posed endpoint fixed except the dragged shared joint.
    fn assert_only_dragged_joint_moves(
        editor: &mut EditorState,
        address: &BoneAddress,
        end: BoneEnd,
        cursor: Point,
    ) {
        let dragged_rest = {
            let (head, tail) = bone_endpoints(&editor.document, address).unwrap();
            match end {
                BoneEnd::Head => head,
                BoneEnd::Tail => tail,
            }
        };
        let before = posed_endpoints(&editor.document, &editor.pose);
        // A real drag runs for many frames, so apply the edit in steps the way
        // the handler does and make sure the compensation does not drift.
        let start = {
            let (head, tail) = bone_endpoints(&editor.document, address).unwrap();
            bone_world(&editor.document, &editor.pose, address).apply(match end {
                BoneEnd::Head => head,
                BoneEnd::Tail => tail,
            })
        };
        const STEPS: usize = 8;
        for step in 1..=STEPS {
            let t = step as f32 / STEPS as f32;
            let at = [
                start[0] + (cursor[0] - start[0]) * t,
                start[1] + (cursor[1] - start[1]) * t,
            ];
            let rest = endpoint_rest_point(&editor.document, &editor.pose, address, at);
            drag_endpoint_keeping_children(editor, address, end, rest);
        }
        let after = posed_endpoints(&editor.document, &editor.pose);
        for (index, ((rest, b), (_, a))) in before.into_iter().zip(after).enumerate() {
            // The dragged joint, and every endpoint structurally sharing it,
            // is allowed to move.
            if distance(rest, dragged_rest) < 1e-3 {
                continue;
            }
            assert!(
                (a[0] - b[0]).abs() < 1e-3 && (a[1] - b[1]).abs() < 1e-3,
                "endpoint {index} moved: expected {b:?}, got {a:?} (dragged rest joint {dragged_rest:?})"
            );
        }
    }

    #[test]
    fn dragging_a_posed_chain_tail_keeps_other_endpoints_fixed() {
        let mut editor = EditorState::new(Document::new());
        let root = add_bone(&mut editor.document, [0.0, 0.0], [10.0, 0.0], "Root");
        let child = add_child_bone(
            &mut editor.document,
            &root,
            [10.0, 0.0],
            [10.0, 10.0],
            "Child",
        )
        .unwrap();
        let _grandchild = add_child_bone(
            &mut editor.document,
            &child,
            [10.0, 10.0],
            [20.0, 10.0],
            "Grand",
        )
        .unwrap();
        editor.pose.set(
            "Root",
            BonePose {
                rotation: 0.4,
                ..Default::default()
            },
        );
        editor.pose.set(
            "Child",
            BonePose {
                rotation: 0.7,
                scale: 1.2,
                ..Default::default()
            },
        );
        editor.pose.set(
            "Grand",
            BonePose {
                rotation: -0.3,
                ..Default::default()
            },
        );

        let cursor = bone_world(&editor.document, &editor.pose, &root).apply([12.0, 3.0]);
        assert_only_dragged_joint_moves(&mut editor, &root, BoneEnd::Tail, cursor);
    }

    #[test]
    fn dragging_a_posed_mid_tail_keeps_other_endpoints_fixed() {
        let mut editor = EditorState::new(Document::new());
        let root = add_bone(&mut editor.document, [0.0, 0.0], [10.0, 0.0], "Root");
        let child = add_child_bone(
            &mut editor.document,
            &root,
            [10.0, 0.0],
            [10.0, 10.0],
            "Child",
        )
        .unwrap();
        add_child_bone(
            &mut editor.document,
            &child,
            [10.0, 10.0],
            [20.0, 10.0],
            "Grand",
        )
        .unwrap();
        editor.pose.set(
            "Root",
            BonePose {
                rotation: 0.4,
                ..Default::default()
            },
        );
        editor.pose.set(
            "Child",
            BonePose {
                rotation: 0.7,
                ..Default::default()
            },
        );
        editor.pose.set(
            "Grand",
            BonePose {
                rotation: -0.3,
                ..Default::default()
            },
        );

        let cursor = bone_world(&editor.document, &editor.pose, &child).apply([12.0, 12.0]);
        assert_only_dragged_joint_moves(&mut editor, &child, BoneEnd::Tail, cursor);
    }

    #[test]
    fn dragging_a_posed_head_keeps_other_endpoints_fixed() {
        let mut editor = EditorState::new(Document::new());
        let root = add_bone(&mut editor.document, [0.0, 0.0], [10.0, 0.0], "Root");
        let child = add_child_bone(
            &mut editor.document,
            &root,
            [10.0, 0.0],
            [10.0, 10.0],
            "Child",
        )
        .unwrap();
        add_child_bone(
            &mut editor.document,
            &child,
            [10.0, 10.0],
            [20.0, 10.0],
            "Grand",
        )
        .unwrap();
        editor.pose.set(
            "Root",
            BonePose {
                rotation: 0.4,
                ..Default::default()
            },
        );
        editor.pose.set(
            "Child",
            BonePose {
                rotation: 0.7,
                ..Default::default()
            },
        );
        editor.pose.set(
            "Grand",
            BonePose {
                rotation: -0.3,
                ..Default::default()
            },
        );

        // Give the child a loose head to drag.
        bone_at_mut(&mut editor.document, &child).unwrap().head = Some([14.0, 2.0]);
        let cursor = bone_world(&editor.document, &editor.pose, &child).apply([18.0, 5.0]);
        assert_only_dragged_joint_moves(&mut editor, &child, BoneEnd::Head, cursor);
    }

    #[test]
    fn dragging_a_tail_past_a_posed_stored_head_child_keeps_endpoints_fixed() {
        let mut editor = EditorState::new(Document::new());
        let root = add_bone(&mut editor.document, [0.0, 0.0], [10.0, 0.0], "Root");
        let child = add_child_bone(
            &mut editor.document,
            &root,
            [10.0, 0.0],
            [10.0, 10.0],
            "Child",
        )
        .unwrap();
        add_child_bone(
            &mut editor.document,
            &child,
            [10.0, 10.0],
            [20.0, 10.0],
            "Grand",
        )
        .unwrap();
        // Stand in for an older file that stored the head on the tail.
        bone_at_mut(&mut editor.document, &child).unwrap().head = Some([10.0, 0.0]);
        editor.pose.set(
            "Root",
            BonePose {
                rotation: 0.4,
                ..Default::default()
            },
        );
        editor.pose.set(
            "Child",
            BonePose {
                rotation: 0.7,
                ..Default::default()
            },
        );
        editor.pose.set(
            "Grand",
            BonePose {
                rotation: -0.3,
                ..Default::default()
            },
        );

        let cursor = bone_world(&editor.document, &editor.pose, &root).apply([12.0, 3.0]);
        assert_only_dragged_joint_moves(&mut editor, &root, BoneEnd::Tail, cursor);
    }

    #[test]
    fn dragging_a_posed_tail_with_two_children_keeps_endpoints_fixed() {
        let mut editor = EditorState::new(Document::new());
        let root = add_bone(&mut editor.document, [0.0, 0.0], [10.0, 0.0], "Root");
        add_child_bone(&mut editor.document, &root, [10.0, 0.0], [10.0, 10.0], "A").unwrap();
        add_child_bone(&mut editor.document, &root, [10.0, 0.0], [10.0, -10.0], "B").unwrap();
        editor.pose.set(
            "Root",
            BonePose {
                rotation: 0.4,
                ..Default::default()
            },
        );
        editor.pose.set(
            "A",
            BonePose {
                rotation: 0.7,
                ..Default::default()
            },
        );
        editor.pose.set(
            "B",
            BonePose {
                rotation: -0.5,
                scale: 0.8,
                ..Default::default()
            },
        );

        let cursor = bone_world(&editor.document, &editor.pose, &root).apply([12.0, 3.0]);
        assert_only_dragged_joint_moves(&mut editor, &root, BoneEnd::Tail, cursor);
    }

    /// Minimal deterministic RNG so the stress test is reproducible.
    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> f32 {
            self.0 = self
                .0
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            ((self.0 >> 33) as f32) / (u32::MAX >> 1) as f32
        }

        fn range(&mut self, low: f32, high: f32) -> f32 {
            low + (high - low) * self.next()
        }
    }

    fn random_rig(rng: &mut Rng, editor: &mut EditorState, depth: usize) -> Vec<BoneAddress> {
        let mut addresses = Vec::new();
        let parent: Option<BoneAddress> = None;
        let _ = parent;
        let roots = 1 + (rng.next() * 2.0) as usize;
        for _ in 0..roots {
            let head = [rng.range(-20.0, 20.0), rng.range(-20.0, 20.0)];
            let tail = [rng.range(-20.0, 20.0), rng.range(-20.0, 20.0)];
            let name = unique_bone_name(&editor.document);
            let address = add_bone(&mut editor.document, head, tail, name);
            addresses.push(address.clone());
            grow_random_rig(rng, editor, &address, depth, &mut addresses);
        }
        addresses
    }

    fn grow_random_rig(
        rng: &mut Rng,
        editor: &mut EditorState,
        parent: &BoneAddress,
        depth: usize,
        addresses: &mut Vec<BoneAddress>,
    ) {
        if depth == 0 {
            return;
        }
        let children = (rng.next() * 3.0) as usize;
        for _ in 0..children {
            let (head, tail) = bone_endpoints(&editor.document, parent).unwrap();
            let connected = rng.next() < 0.6;
            let start = if connected {
                tail
            } else {
                [rng.range(-20.0, 20.0), rng.range(-20.0, 20.0)]
            };
            let end = [rng.range(-20.0, 20.0), rng.range(-20.0, 20.0)];
            let name = unique_bone_name(&editor.document);
            if let Some(child) = add_child_bone(&mut editor.document, parent, start, end, name) {
                addresses.push(child.clone());
                grow_random_rig(rng, editor, &child, depth - 1, addresses);
            }
            let _ = head;
        }
    }

    #[test]
    fn randomized_endpoint_drags_keep_other_posed_endpoints_fixed() {
        let mut rng = Rng(0x1234_5678_9abc_def0);
        for _ in 0..200 {
            let mut editor = EditorState::new(Document::new());
            let addresses = random_rig(&mut rng, &mut editor, 3);
            for address in &addresses {
                if let Some(name) = bone_name_at(&editor.document, address).map(str::to_owned) {
                    let pose = BonePose {
                        rotation: rng.range(-1.5, 1.5),
                        translation: [rng.range(-3.0, 3.0), rng.range(-3.0, 3.0)],
                        scale: rng.range(0.6, 1.6),
                        pivot: bone_endpoints(&editor.document, address).unwrap().0,
                    };
                    editor.pose.set(name, pose);
                }
            }
            for address in &addresses {
                for end in [BoneEnd::Head, BoneEnd::Tail] {
                    let mut trial = EditorState::new(editor.document.clone());
                    trial.pose = editor.pose.clone();
                    let target = [rng.range(-25.0, 25.0), rng.range(-25.0, 25.0)];
                    let cursor = bone_world(&trial.document, &trial.pose, address).apply(target);
                    assert_only_dragged_joint_moves(&mut trial, address, end, cursor);
                }
            }
        }
    }

    #[test]
    fn reconnecting_a_posed_stored_head_keeps_its_tail_fixed() {
        let mut editor = EditorState::new(Document::new());
        let parent = add_bone(&mut editor.document, [0.0, 0.0], [10.0, 0.0], "Parent");
        let child = add_bone(&mut editor.document, [20.0, 0.0], [20.0, 10.0], "Child");
        let child = move_bone_into(&mut editor.document, &child, &parent).unwrap();
        editor.pose.set(
            "Parent",
            BonePose {
                rotation: 0.4,
                ..Default::default()
            },
        );
        editor.pose.set(
            "Child",
            BonePose {
                rotation: 0.7,
                ..Default::default()
            },
        );

        let (_, tail) = bone_endpoints(&editor.document, &child).unwrap();
        let before = bone_world(&editor.document, &editor.pose, &child).apply(tail);

        // Reconnect the stored head to the parent's drawn tail.
        let pose = editor.pose.clone();
        let joint = bone_world(&editor.document, &editor.pose, &parent)
            .apply(bone_endpoints(&editor.document, &parent).unwrap().1);
        assert!(!finish_endpoint_drag(
            &mut editor,
            &child,
            BoneEnd::Head,
            joint,
            1.0,
            &pose
        ));

        let (_, tail) = bone_endpoints(&editor.document, &child).unwrap();
        let after = bone_world(&editor.document, &editor.pose, &child).apply(tail);
        assert_point(after, before);
    }

    #[test]
    fn dragging_a_posed_tail_leaves_a_translated_childs_offset_intact() {
        let mut editor = EditorState::new(Document::new());
        let parent = add_bone(&mut editor.document, [0.0, 0.0], [10.0, 0.0], "Parent");
        let child = add_child_bone(
            &mut editor.document,
            &parent,
            [10.0, 0.0],
            [10.0, 10.0],
            "Child",
        )
        .unwrap();
        editor.pose.set(
            "Parent",
            BonePose {
                rotation: 0.4,
                ..Default::default()
            },
        );
        // Shift-drag the child away from its parent's tail.
        editor.pose.set(
            "Child",
            BonePose {
                translation: [3.0, -2.0],
                ..Default::default()
            },
        );

        let posed_child = |editor: &EditorState, point: Point| {
            bone_world(&editor.document, &editor.pose, &child).apply(point)
        };
        let (child_head, child_tail) = bone_endpoints(&editor.document, &child).unwrap();
        let head_before = posed_child(&editor, child_head);
        let tail_before = posed_child(&editor, child_tail);
        let joint_before = bone_world(&editor.document, &editor.pose, &parent)
            .apply(bone_endpoints(&editor.document, &parent).unwrap().1);
        let offset_before = [
            head_before[0] - joint_before[0],
            head_before[1] - joint_before[1],
        ];

        let cursor = bone_world(&editor.document, &editor.pose, &parent).apply([12.0, 3.0]);
        let rest = endpoint_rest_point(&editor.document, &editor.pose, &parent, cursor);
        drag_endpoint_keeping_children(&mut editor, &parent, BoneEnd::Tail, rest);

        // The child's far endpoint must stay put, and its head keeps the same
        // offset from the joint it had before the drag. Compensation edits rest
        // geometry, so re-read the endpoints before transforming them.
        let joint_after = bone_world(&editor.document, &editor.pose, &parent)
            .apply(bone_endpoints(&editor.document, &parent).unwrap().1);
        let (child_head_after, child_tail_after) =
            bone_endpoints(&editor.document, &child).unwrap();
        assert_point(posed_child(&editor, child_tail_after), tail_before);
        let head_after = posed_child(&editor, child_head_after);
        assert_point(
            [
                head_after[0] - joint_after[0],
                head_after[1] - joint_after[1],
            ],
            offset_before,
        );
    }

    #[test]
    fn renaming_a_bone_renames_it_in_weights() {
        let mut editor = EditorState::new(Document::new());
        let bone = add_bone(&mut editor.document, [0.0, 0.0], [1.0, 0.0], "Arm");
        editor
            .document
            .shapes
            .push(shape_with_weights(&[("Arm", 0.5)], &[("Arm", 1.0)]));
        let mut animation = Animation::new(1.0);
        animation.insert_keyframe(Keyframe::new(
            0.0,
            0.0,
            KeyframeTarget::Bone {
                bone: "Arm".into(),
                property: BoneProperty::X,
            },
        ));
        editor.document.animations.push(animation);

        assert!(rename_bone_at(
            &mut editor.document,
            &bone,
            "Limb".to_owned()
        ));

        let path = &editor.document.shapes[0].paths[0];
        assert_eq!(path.start_weights.get("Arm"), None);
        assert_eq!(path.start_weights.get("Limb"), Some(0.5));
        assert_eq!(path.segments[0].to_weights.get("Limb"), Some(1.0));

        let target = &editor.document.animations[0].keyframes[0].target;
        match target {
            KeyframeTarget::Bone { bone, .. } => assert_eq!(bone, "Limb"),
            KeyframeTarget::Shape { .. } => unreachable!(),
        }
    }

    fn animatable_editor() -> EditorState {
        let mut document = document_with_bones();
        document.bones[0].name = "Arm".into();
        document.animations.push(Animation::new(4.0));
        let mut editor = EditorState::new(document);
        editor.selected_animation = Some(0);
        editor.playhead = 1.0;
        editor
    }

    /// Presses on the bone, then drags to `to` with the pointer held.
    fn drag_bone(editor: &mut EditorState, keys: &ButtonInput<KeyCode>, from: Point, to: Point) {
        let mut mouse = ButtonInput::<MouseButton>::default();
        mouse.press(MouseButton::Left);
        handle_animate_bone(&mouse, keys, editor, from, 1.0, false);
        // A second frame keeps the button held but drops `just_pressed`.
        mouse.clear();
        handle_animate_bone(&mouse, keys, editor, to, 1.0, false);
    }

    fn keyed_value(animation: &Animation, property: BoneProperty) -> Option<f32> {
        animation
            .keyframes
            .iter()
            .find_map(|keyframe| match &keyframe.target {
                KeyframeTarget::Bone {
                    bone,
                    property: keyed,
                } if bone == "Arm" && *keyed == property => Some(keyframe.value),
                _ => None,
            })
    }

    #[test]
    fn animate_drags_key_the_bone_at_the_playhead() {
        let mut editor = animatable_editor();
        let keys = ButtonInput::<KeyCode>::default();

        drag_bone(&mut editor, &keys, [5.0, 0.0], [5.0, 5.0]);

        assert_eq!(editor.selected_bone, Some(address(&[0])));
        let animation = &editor.document.animations[0];
        assert_eq!(animation.keyframes.len(), 1);
        assert_eq!(animation.keyframes[0].timestamp, 1.0);
        let angle = keyed_value(animation, BoneProperty::Angle).unwrap();
        assert!((angle - std::f32::consts::FRAC_PI_4).abs() < 1e-4);
    }

    #[test]
    fn animate_clicks_alone_do_not_key_the_bone() {
        let mut editor = animatable_editor();
        let keys = ButtonInput::<KeyCode>::default();

        drag_bone(&mut editor, &keys, [5.0, 0.0], [5.0, 1.0]);

        // The pointer stayed inside the click slop, so nothing was keyed.
        assert_eq!(editor.selected_bone, Some(address(&[0])));
        assert!(editor.document.animations[0].keyframes.is_empty());
    }

    #[test]
    fn animate_shift_drags_key_translation() {
        let mut editor = animatable_editor();
        let mut keys = ButtonInput::<KeyCode>::default();
        keys.press(KeyCode::ShiftLeft);

        drag_bone(&mut editor, &keys, [5.0, 0.0], [5.0, 5.0]);

        let animation = &editor.document.animations[0];
        assert_eq!(animation.keyframes.len(), 2);
        assert_eq!(keyed_value(animation, BoneProperty::X), Some(0.0));
        assert_eq!(keyed_value(animation, BoneProperty::Y), Some(5.0));
    }

    #[test]
    fn animate_ctrl_drags_key_scale() {
        let mut editor = animatable_editor();
        let mut keys = ButtonInput::<KeyCode>::default();
        keys.press(KeyCode::ControlLeft);

        drag_bone(&mut editor, &keys, [5.0, 0.0], [10.0, 0.0]);

        let animation = &editor.document.animations[0];
        assert_eq!(animation.keyframes.len(), 1);
        assert!((keyed_value(animation, BoneProperty::Scale).unwrap() - 2.0).abs() < 1e-4);
    }

    #[test]
    fn animate_drags_replace_the_keyframe_at_the_playhead() {
        let mut editor = animatable_editor();
        editor.document.animations[0].insert_keyframe(Keyframe::new(
            1.0,
            0.0,
            KeyframeTarget::Bone {
                bone: "Arm".into(),
                property: BoneProperty::Angle,
            },
        ));
        let keys = ButtonInput::<KeyCode>::default();

        drag_bone(&mut editor, &keys, [5.0, 0.0], [5.0, 5.0]);

        let animation = &editor.document.animations[0];
        assert_eq!(animation.keyframes.len(), 1);
        let angle = keyed_value(animation, BoneProperty::Angle).unwrap();
        assert!((angle - std::f32::consts::FRAC_PI_4).abs() < 1e-4);
    }
}
