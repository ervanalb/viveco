//! Transient bone poses and the deformation they apply to the artwork.
//!
//! A pose gives every bone a local rotation, translation, and scale on top of
//! its rest geometry. Poses are editor state, not asset data: they are never
//! saved, and they only affect what a rig view shows. Editing can always work
//! against the rest pose.
//!
//! Each bone's local transform is a [`Similarity`] (uniform scale, rotation,
//! and translation) that pivots about the bone's rest head. Local transforms
//! stack down the hierarchy: a bone's world transform is its parent's world
//! transform composed with its own, so rotating a parent carries its children
//! along.
//!
//! [`Pose`] stores those local transforms. [`WorldPose`] resolves them into one
//! world transform per bone, which is what the renderer skins with at draw
//! time. Bones are stored in topological order, so that resolution is a single
//! linear pass over the flat [`BoneIndex`] order.
//!
//! The artwork is moved with linear blend skinning. A curve point weighted to
//! several bones moves to the blend of where each bone's world transform puts
//! it, with the point's unused weight `1 - sum` keeping it in place.
//!
//! [`ShapePose`] is the non-bone counterpart: it carries the offsets animation
//! curves add to shape properties. For now that is a Z offset, which changes a
//! shape's painting order rather than its geometry.

use crate::asset::Asset;
use crate::bone::{Bone, BoneIndex};
use crate::shape::{Path, PathEnds, Point, Shape, ShapeIndex, Weights};

/// A uniform scale, rotation, and translation.
///
/// Points transform as `translation + scale * rotate(point, rotation)`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Similarity {
    pub scale: f32,
    /// Rotation in radians.
    pub rotation: f32,
    pub translation: Point,
}

impl Default for Similarity {
    fn default() -> Self {
        Self::IDENTITY
    }
}

impl Similarity {
    /// The identity transform.
    pub const IDENTITY: Self = Self {
        scale: 1.0,
        rotation: 0.0,
        translation: [0.0, 0.0],
    };

    /// Applies the transform to a position.
    pub fn apply(self, point: Point) -> Point {
        let rotated = rotate(point, self.rotation);
        [
            self.translation[0] + self.scale * rotated[0],
            self.translation[1] + self.scale * rotated[1],
        ]
    }

    /// Applies only the linear part, ignoring the translation.
    pub fn apply_vector(self, vector: Point) -> Point {
        let rotated = rotate(vector, self.rotation);
        [self.scale * rotated[0], self.scale * rotated[1]]
    }

    /// The transform that undoes this one.
    pub fn inverse(self) -> Self {
        let scale = 1.0 / self.scale;
        let rotation = -self.rotation;
        let rotated = rotate(self.translation, rotation);
        Self {
            scale,
            rotation,
            translation: [-scale * rotated[0], -scale * rotated[1]],
        }
    }

    /// The transform that applies `inner` first and then `outer`.
    pub fn compose(outer: Self, inner: Self) -> Self {
        Self {
            scale: outer.scale * inner.scale,
            rotation: outer.rotation + inner.rotation,
            translation: add(outer.translation, outer.apply_vector(inner.translation)),
        }
    }
}

/// A single bone's local pose, relative to its rest geometry.
///
/// `rotation` turns about the bone's head, `translation` offsets it in its
/// parent's frame, and `scale` grows it about the head.
///
/// The pivot is chosen per bone. A bone with a stored head owns that geometry,
/// so its pivot is frozen at the bone's rest head when the pose is edited; this
/// keeps a later Select-mode head drag from silently warping the pose. A
/// connected bone has no head of its own—its head is the parent's live tail
/// joint—so it pivots about the current rest head, which keeps its body joined
/// to the parent when that shared joint moves.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BonePose {
    /// Rotation about `pivot`, in radians.
    pub rotation: f32,
    /// Offset in the parent's rest frame.
    pub translation: Point,
    pub scale: f32,
    /// The rest-space point the rotation and scale pivot about.
    pub pivot: Point,
}

impl Default for BonePose {
    fn default() -> Self {
        Self {
            rotation: 0.0,
            translation: [0.0, 0.0],
            scale: 1.0,
            pivot: [0.0, 0.0],
        }
    }
}

impl BonePose {
    /// Whether this pose leaves the bone where its rest geometry puts it.
    pub fn is_identity(self) -> bool {
        const EPSILON: f32 = 1e-6;
        self.rotation.abs() <= EPSILON
            && self.translation[0].abs() <= EPSILON
            && self.translation[1].abs() <= EPSILON
            && (self.scale - 1.0).abs() <= EPSILON
    }

    /// The local transform this pose applies.
    ///
    /// `rest_head` is the bone's current head and `connected` says whether that
    /// head is the parent's tail joint (an omitted head). A connected bone
    /// pivots about `rest_head`; a bone with a stored head pivots about its
    /// frozen [`pivot`](Self::pivot).
    pub fn local(self, rest_head: Point, connected: bool) -> Similarity {
        let pivot = if connected { rest_head } else { self.pivot };
        let linear = Similarity {
            scale: self.scale,
            rotation: self.rotation,
            translation: [0.0, 0.0],
        };
        // `pivot + translation + scale*rotate(p - pivot, rotation)` expands to
        // `(pivot + translation - linear(pivot)) + linear(p)`.
        let rotated_pivot = linear.apply_vector(pivot);
        Similarity {
            scale: self.scale,
            rotation: self.rotation,
            translation: [
                pivot[0] + self.translation[0] - rotated_pivot[0],
                pivot[1] + self.translation[1] - rotated_pivot[1],
            ],
        }
    }

    /// Returns this pose with its pivot moved to `pivot`, preserving the same
    /// transform.
    ///
    /// Re-anchoring changes the raw translation offset needed to keep the
    /// transform identical: `t_new = t + (I - SR)(p_old - p_new)`.
    pub fn with_pivot(self, pivot: Point) -> Self {
        let linear = Similarity {
            scale: self.scale,
            rotation: self.rotation,
            translation: [0.0, 0.0],
        };
        let delta = [self.pivot[0] - pivot[0], self.pivot[1] - pivot[1]];
        let scaled = linear.apply_vector(delta);
        Self {
            translation: [
                self.translation[0] + delta[0] - scaled[0],
                self.translation[1] + delta[1] - scaled[1],
            ],
            pivot,
            ..self
        }
    }
}

/// Every bone's transient pose, ordered by [`BoneIndex`].
///
/// The vector always has one entry per bone in the matching [`Asset`]; bones
/// without a pose entry in a freshly built `Pose` are at rest. Because indices
/// shift whenever the rig is edited, whoever keeps a `Pose` alongside an asset
/// must apply the same index remap to it; [`Self::apply_bone_map`] does that.
#[derive(Clone, Debug, Default)]
pub struct Pose {
    bones: Vec<BonePose>,
}

impl Pose {
    /// An all-rest pose with one entry per bone.
    pub fn new(bone_count: usize) -> Self {
        Self {
            bones: vec![BonePose::default(); bone_count],
        }
    }

    /// The pose of the indexed bone, or the identity when it has none.
    pub fn get(&self, index: BoneIndex) -> BonePose {
        self.bones.get(index).copied().unwrap_or_default()
    }

    /// Stores a bone's pose.
    ///
    /// A pose set beyond the current length grows the vector so the entry is
    /// never silently lost.
    pub fn set(&mut self, index: BoneIndex, pose: BonePose) {
        if index >= self.bones.len() {
            self.bones.resize(index + 1, BonePose::default());
        }
        self.bones[index] = pose;
    }

    /// Rewrites every entry through `map`, dropping entries for bones the map
    /// deletes.
    pub fn apply_bone_map(&mut self, map: &[Option<BoneIndex>]) {
        let mut remapped = vec![BonePose::default(); map.iter().flatten().count()];
        for (old, new) in map.iter().enumerate() {
            if let Some(new) = *new
                && let Some(pose) = self.bones.get(old)
            {
                remapped[new] = *pose;
            }
        }
        self.bones = remapped;
    }

    /// Whether every bone is at rest.
    pub fn is_identity(&self) -> bool {
        self.bones.iter().all(|pose| pose.is_identity())
    }

    /// Returns every bone to rest, keeping one entry per bone.
    pub fn clear(&mut self) {
        self.bones.fill(BonePose::default());
    }

    /// Adds several weighted poses together.
    ///
    /// The result starts at the rest pose and each sample contributes its
    /// offset from rest, scaled by its weight: rotations and translations add,
    /// and scales add their deviation from `1`. Weights are not normalized, so
    /// two full-weight layers both contribute fully; weights that sum to `1`
    /// behave like a weighted average.
    ///
    /// Non-finite and non-positive weights are ignored, and if nothing remains
    /// the result is the rest pose.
    ///
    /// Samples must share a pivot for each bone, or their rotations and
    /// translations will refer to different origins; the first sample's pivot
    /// is used. [`Asset::sample_pose`](crate::Asset::sample_pose) produces such
    /// poses.
    pub fn blend(bone_count: usize, samples: &[(&Pose, f32)]) -> Self {
        let mut bones = vec![BonePose::default(); bone_count];
        let mut sampled = false;

        for (pose, weight) in samples {
            if !weight.is_finite() || *weight <= 0.0 {
                continue;
            }
            let weight = *weight;

            if !sampled {
                // Pivots are shared by construction, so take them from the
                // first contributing pose.
                for (index, bone) in bones.iter_mut().enumerate() {
                    bone.pivot = pose.get(index).pivot;
                }
                sampled = true;
            }

            for (index, accumulated) in bones.iter_mut().enumerate() {
                let bone = pose.get(index);
                accumulated.rotation += bone.rotation * weight;
                accumulated.translation[0] += bone.translation[0] * weight;
                accumulated.translation[1] += bone.translation[1] * weight;
                accumulated.scale += (bone.scale - 1.0) * weight;
            }
        }

        if !sampled {
            return Self::new(bone_count);
        }
        Self { bones }
    }

    /// Resolves this pose into the world-space transforms of `asset`'s bones.
    ///
    /// This is the representation the renderer skins geometry with.
    pub fn to_world(&self, asset: &Asset) -> WorldPose {
        WorldPose::new(asset, self)
    }
}

/// Animation-driven shape offsets, ordered by [`ShapeIndex`].
///
/// A shape pose stores only deltas from the shapes' authored values. For now
/// the only offset is [`z`](Self::z_offsets), added to
/// [`Shape::z`](crate::Shape::z) when shapes are painted. Like [`Pose`], this
/// is transient sampling state rather than document data: it is produced by
/// [`Animation::sample_shape_pose`](crate::Animation::sample_shape_pose) and
/// never saved.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ShapePose {
    z: Vec<f32>,
}

impl ShapePose {
    /// An all-rest shape pose with one entry per shape.
    pub fn new(shape_count: usize) -> Self {
        Self {
            z: vec![0.0; shape_count],
        }
    }

    /// The Z offset of the indexed shape, or zero when it has no entry.
    pub fn get(&self, index: ShapeIndex) -> f32 {
        self.z.get(index).copied().unwrap_or(0.0)
    }

    /// Stores a shape's Z offset.
    ///
    /// A pose set beyond the current length grows the vector so the entry is
    /// never silently lost.
    pub fn set(&mut self, index: ShapeIndex, z: f32) {
        if index >= self.z.len() {
            self.z.resize(index + 1, 0.0);
        }
        self.z[index] = z;
    }

    /// The Z offsets, ordered by [`ShapeIndex`].
    pub fn z_offsets(&self) -> &[f32] {
        &self.z
    }

    /// Whether every shape is at its authored Z.
    pub fn is_identity(&self) -> bool {
        const EPSILON: f32 = 1e-6;
        self.z.iter().all(|z| z.abs() <= EPSILON)
    }

    /// Returns every shape to its authored Z, keeping one entry per shape.
    pub fn clear(&mut self) {
        self.z.fill(0.0);
    }

    /// Rewrites every entry through `map`, dropping entries for shapes the map
    /// deletes.
    pub fn apply_shape_map(&mut self, map: &[Option<ShapeIndex>]) {
        let mut remapped = vec![0.0; map.iter().flatten().count()];
        for (old, new) in map.iter().enumerate() {
            if let Some(new) = *new
                && let Some(z) = self.z.get(old)
            {
                remapped[new] = *z;
            }
        }
        self.z = remapped;
    }

    /// Adds several weighted shape poses together.
    ///
    /// Each sample contributes its offset scaled by its weight, so weights
    /// summing to `1` behave like a weighted average. Non-finite and
    /// non-positive weights are ignored, and if nothing remains the result is
    /// a rest pose.
    pub fn blend(shape_count: usize, samples: &[(&ShapePose, f32)]) -> Self {
        let mut z = vec![0.0; shape_count];
        let mut sampled = false;

        for (pose, weight) in samples {
            if !weight.is_finite() || *weight <= 0.0 {
                continue;
            }
            sampled = true;
            for (index, offset) in z.iter_mut().enumerate() {
                *offset += pose.get(index) * *weight;
            }
        }

        if !sampled {
            return Self::new(shape_count);
        }
        Self { z }
    }
}

/// Every bone's world-space transform under a [`Pose`].
///
/// The transforms are ordered by [`BoneIndex`]. Assets store their bones in
/// topological order, with every parent before its children, so building this
/// value composes each bone on top of its already-resolved parent in one
/// linear pass; no bone is looked up by searching the skeleton.
///
/// Unlike [`Pose`], which is local editing state, a `WorldPose` is the
/// resolved input to skinning. [`WorldPose::deform`] can therefore move a
/// point without consulting the asset, which lets the renderer skin geometry
/// while it draws instead of cloning and deforming the whole asset first.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct WorldPose {
    worlds: Vec<Similarity>,
}

impl WorldPose {
    /// Every bone's world transform under `pose`.
    pub fn new(asset: &Asset, pose: &Pose) -> Self {
        Self {
            worlds: asset.bone_worlds(pose),
        }
    }

    /// Every bone at its rest world transform in `asset`.
    pub fn rest(asset: &Asset) -> Self {
        Self {
            worlds: vec![Similarity::IDENTITY; asset.bone_count()],
        }
    }

    /// Wraps an already resolved list of bone world transforms, ordered by
    /// [`BoneIndex`].
    ///
    /// This is the escape hatch for callers that compute world transforms
    /// themselves (a custom solver, for example) rather than through a
    /// [`Pose`].
    pub fn from_worlds(worlds: Vec<Similarity>) -> Self {
        Self { worlds }
    }

    /// The world transforms, ordered by [`BoneIndex`].
    pub fn worlds(&self) -> &[Similarity] {
        &self.worlds
    }

    /// A bone's world transform, or the identity when it has none.
    pub fn get(&self, index: BoneIndex) -> Similarity {
        self.worlds
            .get(index)
            .copied()
            .unwrap_or(Similarity::IDENTITY)
    }

    /// Whether every bone is at its rest world transform.
    pub fn is_identity(&self) -> bool {
        self.worlds
            .iter()
            .all(|world| *world == Similarity::IDENTITY)
    }

    /// Skins one weighted point with this pose.
    pub fn deform(&self, weights: &Weights, position: Point) -> Point {
        weights.deform(position, &self.worlds)
    }
}

/// The world transform of the bone at `target` under `pose`.
///
/// Each bone contributes its local pose about its rest head, composed on top of
/// its parent's world transform, so a child inherits everything above it.
fn world_at(asset: &Asset, pose: &Pose, target: BoneIndex) -> Similarity {
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
        &asset.bones,
        Similarity::IDENTITY,
        None,
        pose,
        &mut cursor,
        target,
    )
    .unwrap_or(Similarity::IDENTITY)
}

impl Asset {
    /// The world transform of every bone under `pose`, ordered by
    /// [`BoneIndex`].
    ///
    /// The flat [`BoneIndex`] order is depth-first pre-order, so it is
    /// topological: a bone's parent is always resolved before the bone itself.
    /// This walks that order once with an explicit stack, composing each bone
    /// from its parent's finished world transform. It never recurses and never
    /// searches for a parent.
    pub fn bone_worlds(&self, pose: &Pose) -> Vec<Similarity> {
        struct Frame<'a> {
            bones: &'a [Bone],
            next: usize,
            parent_world: Similarity,
            parent_tail: Option<Point>,
        }

        let mut worlds = vec![Similarity::IDENTITY; self.bone_count()];
        let mut stack = vec![Frame {
            bones: &self.bones,
            next: 0,
            parent_world: Similarity::IDENTITY,
            parent_tail: None,
        }];
        let mut index = 0;

        while !stack.is_empty() {
            let (bone, parent_world, parent_tail) = {
                let frame = stack.last_mut().expect("the stack is not empty");
                let Some(bone) = frame.bones.get(frame.next) else {
                    stack.pop();
                    continue;
                };
                frame.next += 1;
                (bone, frame.parent_world, frame.parent_tail)
            };

            let rest_head = bone.head_with_parent(parent_tail);
            let local = pose.get(index).local(rest_head, bone.head.is_none());
            let world = Similarity::compose(parent_world, local);
            // The tree's pre-order gives every bone the next flat index, and
            // parents are always visited first.
            debug_assert!(index < worlds.len(), "bones are stored topologically");
            worlds[index] = world;
            index += 1;

            if !bone.children.is_empty() {
                stack.push(Frame {
                    bones: &bone.children,
                    next: 0,
                    parent_world: world,
                    parent_tail: Some(bone.tail),
                });
            }
        }

        debug_assert_eq!(index, worlds.len(), "every bone gets one entry");
        worlds
    }

    /// A bone's world transform under `pose`.
    pub fn bone_world(&self, pose: &Pose, index: BoneIndex) -> Similarity {
        world_at(self, pose, index)
    }

    /// The world transform of a bone's parent under `pose`, or the identity for
    /// a root bone.
    ///
    /// Asset edits move rest geometry within the parent's frame, so this is the
    /// transform that maps those rest-space changes into world space.
    pub fn parent_world(&self, pose: &Pose, index: BoneIndex) -> Similarity {
        match self.parent_of(index) {
            Some(parent) => world_at(self, pose, parent),
            None => Similarity::IDENTITY,
        }
    }

    /// The rest head of every bone, ordered by [`BoneIndex`].
    ///
    /// A connected bone has no stored head, so its head is its parent's tail.
    /// The animation sampler needs this to pivot a posed bone about the same
    /// joint the rig view uses.
    pub fn rest_heads(&self) -> Vec<Point> {
        fn walk(
            bones: &[Bone],
            parent_tail: Option<Point>,
            cursor: &mut usize,
            heads: &mut [Point],
        ) {
            for bone in bones {
                let index = *cursor;
                *cursor += 1;
                heads[index] = bone.head_with_parent(parent_tail);
                walk(&bone.children, Some(bone.tail), cursor, heads);
            }
        }

        let mut heads = vec![[0.0, 0.0]; self.bone_count()];
        let mut cursor = 0;
        walk(&self.bones, None, &mut cursor, &mut heads);
        heads
    }

    /// Returns a copy of this asset with every curve point skinned by `pose`.
    ///
    /// This is what a rig view paints. The asset itself is never modified, so
    /// the rest pose stays the single source of truth for editing and saving.
    pub fn deformed(&self, pose: &Pose) -> Asset {
        if pose.is_identity() {
            return self.clone();
        }
        let world = WorldPose::new(self, pose);
        let mut deformed = self.clone();
        for shape in &mut deformed.shapes {
            deform_shape(shape, world.worlds());
        }
        deformed
    }
}

impl Weights {
    /// Skins one point: a weighted blend of each bone's world transform, with
    /// the unused share `1 - sum` left in place.
    pub fn deform(&self, position: Point, worlds: &[Similarity]) -> Point {
        let mut blended = [0.0, 0.0];
        let mut matched = 0.0;
        for weight in self.iter() {
            let Some(world) = worlds.get(weight.bone) else {
                continue;
            };
            let posed = world.apply(position);
            blended[0] += weight.weight * posed[0];
            blended[1] += weight.weight * posed[1];
            matched += weight.weight;
        }
        let rest = (1.0 - matched).max(0.0);
        [
            blended[0] + rest * position[0],
            blended[1] + rest * position[1],
        ]
    }
}

fn deform_shape(shape: &mut Shape, worlds: &[Similarity]) {
    for path in &mut shape.paths {
        deform_path(path, worlds);
    }
    for child in &mut shape.children {
        deform_shape(child, worlds);
    }
}

fn deform_path(path: &mut Path, worlds: &[Similarity]) {
    if let PathEnds::Open(start) = path.ends {
        path.ends = PathEnds::Open(path.start_weights.deform(start, worlds));
    }
    for segment in &mut path.segments {
        if let Some(ctrl1) = segment.ctrl1.as_mut() {
            *ctrl1 = segment.ctrl1_weights.deform(*ctrl1, worlds);
        }
        if let Some(ctrl2) = segment.ctrl2.as_mut() {
            *ctrl2 = segment.ctrl2_weights.deform(*ctrl2, worlds);
        }
        segment.to = segment.to_weights.deform(segment.to, worlds);
    }
}

fn rotate(vector: Point, rotation: f32) -> Point {
    let (sin, cos) = rotation.sin_cos();
    [
        cos * vector[0] - sin * vector[1],
        sin * vector[0] + cos * vector[1],
    ]
}

fn add(a: Point, b: Point) -> Point {
    [a[0] + b[0], a[1] + b[1]]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_point(actual: Point, expected: Point) {
        assert!(
            (actual[0] - expected[0]).abs() < 1e-4 && (actual[1] - expected[1]).abs() < 1e-4,
            "expected {expected:?}, got {actual:?}"
        );
    }

    /// A parent bone from the origin to [10, 0] with a child continuing to
    /// [10, 10]. The parent has index 0 and the child index 1.
    fn rigged_asset(
        start_weights: &[(BoneIndex, f32)],
        anchor_weights: &[(BoneIndex, f32)],
    ) -> Asset {
        let child = Bone {
            name: "Child".into(),
            head: None,
            tail: [10.0, 10.0],
            children: Vec::new(),
        };
        let parent = Bone {
            name: "Parent".into(),
            head: Some([0.0, 0.0]),
            tail: [10.0, 0.0],
            children: vec![child],
        };
        let mut path = Path::new([0.0, 0.0]).line_to([10.0, 10.0]);
        for &(bone, weight) in start_weights {
            path.start_weights.set(bone, weight);
        }
        for &(bone, weight) in anchor_weights {
            path.segments[0].to_weights.set(bone, weight);
        }
        Asset {
            animations: Vec::new(),
            shapes: vec![Shape {
                paths: vec![path],
                ..Default::default()
            }],
            bones: vec![parent],
        }
    }

    #[test]
    fn identity_pose_leaves_geometry_alone() {
        let asset = rigged_asset(&[(0, 1.0)], &[(1, 1.0)]);
        let deformed = asset.deformed(&Pose::default());
        assert_eq!(deformed.shapes, asset.shapes);
    }

    #[test]
    fn rest_heads_resolve_connected_bones() {
        let asset = rigged_asset(&[], &[]);
        let heads = asset.rest_heads();

        assert_eq!(heads[0], [0.0, 0.0]);
        // The child omits its head, so it sits at the parent's tail.
        assert_eq!(heads[1], [10.0, 0.0]);
    }

    #[test]
    fn a_root_rotation_turns_points_about_the_head() {
        let asset = rigged_asset(&[(0, 1.0)], &[(0, 1.0)]);
        let mut pose = Pose::default();
        pose.set(
            0,
            BonePose {
                rotation: std::f32::consts::FRAC_PI_2,
                ..Default::default()
            },
        );

        let deformed = asset.deformed(&pose);
        let path = &deformed.shapes[0].paths[0];
        // A quarter turn maps [10, 10] to [-10, 10] about the origin.
        assert_point(path.start().unwrap(), [0.0, 0.0]);
        assert_point(path.segments[0].to, [-10.0, 10.0]);
    }

    #[test]
    fn a_child_inherits_its_parents_transform() {
        let asset = rigged_asset(&[], &[(1, 1.0)]);
        let mut pose = Pose::default();
        pose.set(
            0,
            BonePose {
                rotation: std::f32::consts::FRAC_PI_2,
                ..Default::default()
            },
        );

        let deformed = asset.deformed(&pose);
        let path = &deformed.shapes[0].paths[0];
        // The child's point follows the parent's rotation even though only the
        // parent is posed.
        assert_point(path.segments[0].to, [-10.0, 10.0]);
    }

    #[test]
    fn weights_blend_between_bones() {
        let asset = rigged_asset(&[(0, 0.5), (1, 0.5)], &[]);
        let mut pose = Pose::default();
        pose.set(
            0,
            BonePose {
                translation: [0.0, 0.0],
                rotation: std::f32::consts::FRAC_PI_2,
                scale: 1.0,
                pivot: [0.0, 0.0],
            },
        );

        let deformed = asset.deformed(&pose);
        let path = &deformed.shapes[0].paths[0];
        // The start is half on the rotated parent (origin, unmoved) and half
        // on the unposed child, so it stays put.
        assert_point(path.start().unwrap(), [0.0, 0.0]);
    }

    #[test]
    fn an_unweighted_point_stays_in_place() {
        let asset = rigged_asset(&[], &[(0, 1.0)]);
        let mut pose = Pose::default();
        pose.set(
            0,
            BonePose {
                translation: [5.0, 0.0],
                ..Default::default()
            },
        );

        let deformed = asset.deformed(&pose);
        let path = &deformed.shapes[0].paths[0];
        assert_point(path.start().unwrap(), [0.0, 0.0]);
        assert_point(path.segments[0].to, [15.0, 10.0]);
    }

    #[test]
    fn world_poses_resolve_every_bone_in_one_linear_pass() {
        let asset = rigged_asset(&[], &[(1, 1.0)]);
        let mut pose = Pose::default();
        pose.set(
            0,
            BonePose {
                rotation: std::f32::consts::FRAC_PI_2,
                ..Default::default()
            },
        );

        let world = pose.to_world(&asset);
        assert!(!world.is_identity());
        assert_eq!(world.worlds().len(), 2);
        // The child inherits the parent's world transform even though only the
        // parent is posed.
        assert_point(world.get(1).apply([10.0, 10.0]), [-10.0, 10.0]);
        assert!(WorldPose::rest(&asset).is_identity());
    }

    #[test]
    fn similarities_compose_and_invert() {
        let a = Similarity {
            scale: 2.0,
            rotation: 0.5,
            translation: [3.0, -4.0],
        };
        let b = Similarity {
            scale: 0.5,
            rotation: -1.0,
            translation: [10.0, 2.0],
        };
        let point = [7.0, -3.0];

        assert_point(
            Similarity::compose(a, b).apply(point),
            a.apply(b.apply(point)),
        );

        let inverse = a.inverse();
        assert_point(inverse.apply(a.apply(point)), point);
    }

    #[test]
    fn setting_an_identity_pose_drops_it() {
        let mut pose = Pose::default();
        pose.set(
            0,
            BonePose {
                rotation: 0.3,
                ..Default::default()
            },
        );
        assert!(!pose.is_identity());

        pose.set(0, BonePose::default());
        assert!(pose.is_identity());
    }

    #[test]
    fn applying_a_bone_map_shifts_and_drops_poses() {
        let mut pose = Pose::default();
        pose.set(
            0,
            BonePose {
                rotation: 0.3,
                ..Default::default()
            },
        );
        pose.set(
            2,
            BonePose {
                rotation: 0.5,
                ..Default::default()
            },
        );

        // Remove bone 0; bone 2 shifts down to index 1.
        pose.apply_bone_map(&crate::bone::removal_map(3, 0, 1));

        assert_eq!(pose.get(0), BonePose::default());
        assert!((pose.get(1).rotation - 0.5).abs() < 1e-6);
    }

    #[test]
    fn blend_adds_full_weight_layers_without_normalizing() {
        let first = Pose::new(1);
        let second = Pose::new(1);

        // Two rest poses must stay at rest rather than doubling the scale.
        let blended = Pose::blend(1, &[(&first, 1.0), (&second, 1.0)]);
        assert!(blended.is_identity());

        let mut rotated = Pose::new(1);
        rotated.set(
            0,
            BonePose {
                rotation: 0.2,
                translation: [3.0, 4.0],
                scale: 1.5,
                pivot: [10.0, 20.0],
            },
        );
        let mut other = Pose::new(1);
        other.set(
            0,
            BonePose {
                rotation: 0.3,
                translation: [-1.0, 2.0],
                scale: 0.5,
                pivot: [10.0, 20.0],
            },
        );

        let blended = Pose::blend(1, &[(&rotated, 1.0), (&other, 1.0)]);
        let bone = blended.get(0);
        assert!((bone.rotation - 0.5).abs() < 1e-6);
        assert_eq!(bone.translation, [2.0, 6.0]);
        // Scale contributions are deltas from the rest scale of 1.
        assert!((bone.scale - 1.0).abs() < 1e-6);
        // Pivots are shared, so the first pose's pivot is kept.
        assert_eq!(bone.pivot, [10.0, 20.0]);

        // Weights summing to 1 still behave like a weighted average.
        let blended = Pose::blend(1, &[(&rotated, 0.25), (&Pose::new(1), 0.75)]);
        assert!((blended.get(0).rotation - 0.05).abs() < 1e-6);
        assert!((blended.get(0).scale - 1.125).abs() < 1e-6);
    }

    #[test]
    fn shape_poses_store_offsets_and_blend_by_weight() {
        let mut first = ShapePose::new(2);
        first.set(1, 4.0);
        let mut second = ShapePose::new(2);
        second.set(0, 8.0);

        let blended = ShapePose::blend(2, &[(&first, 0.25), (&second, 0.75)]);
        assert!((blended.get(0) - 6.0).abs() < 1e-6);
        assert!((blended.get(1) - 1.0).abs() < 1e-6);
        assert!(!blended.is_identity());

        // Two rest poses stay at rest rather than accumulating offsets.
        let blended = ShapePose::blend(2, &[(&ShapePose::new(2), 1.0); 2]);
        assert!(blended.is_identity());

        // With nothing contributing the result is a rest pose.
        assert!(ShapePose::blend(2, &[]).is_identity());
    }

    #[test]
    fn shape_poses_remap_and_drop_offsets() {
        let mut pose = ShapePose::new(3);
        pose.set(0, 1.0);
        pose.set(2, 3.0);

        // Remove shape 0; shape 2 shifts down to index 1.
        pose.apply_shape_map(&crate::bone::removal_map(3, 0, 1));

        assert_eq!(pose.get(0), 0.0);
        assert_eq!(pose.get(1), 3.0);
    }
}
