//! Transient bone poses and the deformation they apply to the artwork.
//!
//! A pose gives every bone a local rotation, translation, and scale on top of
//! its rest geometry. Poses are editor state, not document data: they are
//! never saved, and they only affect what the rig page shows. The draw page
//! always edits and displays the rest pose.
//!
//! Each bone's local transform is a [`Similarity`] (uniform scale, rotation,
//! and translation) that pivots about the bone's rest head. Local transforms
//! stack down the hierarchy: a bone's world transform is its parent's world
//! transform composed with its own, so rotating a parent carries its children
//! along.
//!
//! The artwork is moved with linear blend skinning. A curve point weighted to
//! several bones moves to the blend of where each bone's world transform puts
//! it, with the point's unused weight `1 - sum` keeping it in place.

use crate::model::{Bone, BoneIndex, Document, Path, PathEnds, Point, Shape, Weights};

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
/// The vector always has one entry per bone in the matching [`Document`];
/// bones without a pose entry in a freshly built `Pose` are at rest. Because
/// indices shift whenever the rig is edited, whoever keeps a `Pose` alongside a
/// document must apply the same index remap to it; [`Self::apply_bone_map`]
/// does that.
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
}

/// The world transform of every bone, ordered by [`BoneIndex`].
pub fn worlds(document: &Document, pose: &Pose) -> Vec<Similarity> {
    fn walk(
        bones: &[Bone],
        parent_world: Similarity,
        parent_tail: Option<Point>,
        pose: &Pose,
        cursor: &mut usize,
        worlds: &mut [Similarity],
    ) {
        for bone in bones {
            let index = *cursor;
            *cursor += 1;
            let rest_head = bone.head_with_parent(parent_tail);
            let local = pose.get(index).local(rest_head, bone.head.is_none());
            let world = Similarity::compose(parent_world, local);
            worlds[index] = world;
            walk(&bone.children, world, Some(bone.tail), pose, cursor, worlds);
        }
    }

    let mut worlds = vec![Similarity::IDENTITY; document.bone_count()];
    let mut cursor = 0;
    walk(
        &document.bones,
        Similarity::IDENTITY,
        None,
        pose,
        &mut cursor,
        &mut worlds,
    );
    worlds
}

/// The rest head of every bone, ordered by [`BoneIndex`].
///
/// A connected bone has no stored head, so its head is its parent's tail. The
/// animation sampler needs this to pivot a posed bone about the same joint the
/// rig page uses.
pub fn rest_heads(document: &Document) -> Vec<Point> {
    fn walk(bones: &[Bone], parent_tail: Option<Point>, cursor: &mut usize, heads: &mut [Point]) {
        for bone in bones {
            let index = *cursor;
            *cursor += 1;
            heads[index] = bone.head_with_parent(parent_tail);
            walk(&bone.children, Some(bone.tail), cursor, heads);
        }
    }

    let mut heads = vec![[0.0, 0.0]; document.bone_count()];
    let mut cursor = 0;
    walk(&document.bones, None, &mut cursor, &mut heads);
    heads
}

/// Returns a copy of `document` with every curve point skinned by `pose`.
///
/// This is what the rig page paints. The document itself is never modified, so
/// the rest pose stays the single source of truth for editing and saving.
pub fn deform_document(document: &Document, pose: &Pose) -> Document {
    if pose.is_identity() {
        return document.clone();
    }
    let worlds = worlds(document, pose);
    let mut deformed = document.clone();
    for shape in &mut deformed.shapes {
        deform_shape(shape, &worlds);
    }
    deformed
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
        path.ends = PathEnds::Open(deform_point(start, &path.start_weights, worlds));
    }
    for segment in &mut path.segments {
        if let Some(ctrl1) = segment.ctrl1.as_mut() {
            *ctrl1 = deform_point(*ctrl1, &segment.ctrl1_weights, worlds);
        }
        if let Some(ctrl2) = segment.ctrl2.as_mut() {
            *ctrl2 = deform_point(*ctrl2, &segment.ctrl2_weights, worlds);
        }
        segment.to = deform_point(segment.to, &segment.to_weights, worlds);
    }
}

/// Skins one point: a weighted blend of each bone's world transform, with the
/// unused share `1 - sum` left in place.
pub fn deform_point(position: Point, weights: &Weights, worlds: &[Similarity]) -> Point {
    let mut blended = [0.0, 0.0];
    let mut matched = 0.0;
    for weight in weights.iter() {
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
    use crate::model::FORMAT_VERSION;

    fn assert_point(actual: Point, expected: Point) {
        assert!(
            (actual[0] - expected[0]).abs() < 1e-4 && (actual[1] - expected[1]).abs() < 1e-4,
            "expected {expected:?}, got {actual:?}"
        );
    }

    /// A parent bone from the origin to [10, 0] with a child continuing to
    /// [10, 10]. The parent has index 0 and the child index 1.
    fn rigged_document(
        start_weights: &[(BoneIndex, f32)],
        anchor_weights: &[(BoneIndex, f32)],
    ) -> Document {
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
        Document {
            version: FORMAT_VERSION,
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
        let document = rigged_document(&[(0, 1.0)], &[(1, 1.0)]);
        let deformed = deform_document(&document, &Pose::default());
        assert_eq!(deformed.shapes, document.shapes);
    }

    #[test]
    fn rest_heads_resolve_connected_bones() {
        let document = rigged_document(&[], &[]);
        let heads = rest_heads(&document);

        assert_eq!(heads[0], [0.0, 0.0]);
        // The child omits its head, so it sits at the parent's tail.
        assert_eq!(heads[1], [10.0, 0.0]);
    }

    #[test]
    fn a_root_rotation_turns_points_about_the_head() {
        let document = rigged_document(&[(0, 1.0)], &[(0, 1.0)]);
        let mut pose = Pose::default();
        pose.set(
            0,
            BonePose {
                rotation: std::f32::consts::FRAC_PI_2,
                ..Default::default()
            },
        );

        let deformed = deform_document(&document, &pose);
        let path = &deformed.shapes[0].paths[0];
        // A quarter turn maps [10, 10] to [-10, 10] about the origin.
        assert_point(path.start().unwrap(), [0.0, 0.0]);
        assert_point(path.segments[0].to, [-10.0, 10.0]);
    }

    #[test]
    fn a_child_inherits_its_parents_transform() {
        let document = rigged_document(&[], &[(1, 1.0)]);
        let mut pose = Pose::default();
        pose.set(
            0,
            BonePose {
                rotation: std::f32::consts::FRAC_PI_2,
                ..Default::default()
            },
        );

        let deformed = deform_document(&document, &pose);
        let path = &deformed.shapes[0].paths[0];
        // The child's point follows the parent's rotation even though only the
        // parent is posed.
        assert_point(path.segments[0].to, [-10.0, 10.0]);
    }

    #[test]
    fn weights_blend_between_bones() {
        let document = rigged_document(&[(0, 0.5), (1, 0.5)], &[]);
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

        let deformed = deform_document(&document, &pose);
        let path = &deformed.shapes[0].paths[0];
        // The start is half on the rotated parent (origin, unmoved) and half
        // on the unposed child, so it stays put.
        assert_point(path.start().unwrap(), [0.0, 0.0]);
    }

    #[test]
    fn an_unweighted_point_stays_in_place() {
        let document = rigged_document(&[], &[(0, 1.0)]);
        let mut pose = Pose::default();
        pose.set(
            0,
            BonePose {
                translation: [5.0, 0.0],
                ..Default::default()
            },
        );

        let deformed = deform_document(&document, &pose);
        let path = &deformed.shapes[0].paths[0];
        assert_point(path.start().unwrap(), [0.0, 0.0]);
        assert_point(path.segments[0].to, [15.0, 10.0]);
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
        pose.apply_bone_map(&crate::model::removal_map(3, 0, 1));

        assert_eq!(pose.get(0), BonePose::default());
        assert!((pose.get(1).rotation - 0.5).abs() < 1e-6);
    }
}
