//! Animation layering, bone transforms, and linear-blend skinning.
//!
//! A [`Pose`] stores one local [`BonePose`] per bone. It starts at rest and is
//! built by summing every animation layer's curves. Each curve contributes
//! `weight * (value - rest)` to its bone's property, so a single layer at
//! weight one reproduces the curve exactly, several layers blend additively,
//! and any unused weight leaves the bone at rest.
//!
//! Local transforms are turned into world transforms by walking bones in
//! storage order (parents always precede children) and composing with the
//! parent's world transform. A bone's local transform pivots about its rest
//! head, matching the authoring app.

use crate::anim::sample_curve;
use crate::format::{BoneProperty, Viveco};

/// One animation to blend, at a given weight and local time.
///
/// `animation` indexes the document's animation array (names are omitted from
/// the binary). `time` is in seconds and is sampled with the animation's own
/// clamping and cycling rules.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Layer {
    pub animation: usize,
    pub weight: f32,
    pub time: f32,
}

impl Layer {
    pub fn new(animation: usize, weight: f32, time: f32) -> Self {
        Self {
            animation,
            weight,
            time,
        }
    }
}

/// A uniform scale, rotation, and translation.
///
/// Points transform as `translation + scale * rotate(point, rotation)`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Similarity {
    pub scale: f32,
    /// Rotation in radians.
    pub rotation: f32,
    pub translation: [f32; 2],
}

impl Default for Similarity {
    fn default() -> Self {
        Self::IDENTITY
    }
}

impl Similarity {
    pub const IDENTITY: Self = Self {
        scale: 1.0,
        rotation: 0.0,
        translation: [0.0, 0.0],
    };

    pub fn apply(self, point: [f32; 2]) -> [f32; 2] {
        let rotated = rotate(point, self.rotation);
        [
            self.translation[0] + self.scale * rotated[0],
            self.translation[1] + self.scale * rotated[1],
        ]
    }

    pub fn apply_vector(self, vector: [f32; 2]) -> [f32; 2] {
        let rotated = rotate(vector, self.rotation);
        [self.scale * rotated[0], self.scale * rotated[1]]
    }

    /// The transform that applies `inner` first and then `outer`.
    pub fn compose(outer: Self, inner: Self) -> Self {
        Self {
            scale: outer.scale * inner.scale,
            rotation: outer.rotation + inner.rotation,
            translation: [
                outer.translation[0] + outer.apply_vector(inner.translation)[0],
                outer.translation[1] + outer.apply_vector(inner.translation)[1],
            ],
        }
    }
}

/// One bone's local pose, relative to its rest geometry.
///
/// Rotation and scale pivot about the bone's rest head; `translation` offsets
/// it in its parent's frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BonePose {
    /// Rotation about the rest head, in radians.
    pub rotation: f32,
    /// Offset in the parent's rest frame.
    pub translation: [f32; 2],
    pub scale: f32,
}

impl Default for BonePose {
    fn default() -> Self {
        Self::REST
    }
}

impl BonePose {
    pub const REST: Self = Self {
        rotation: 0.0,
        translation: [0.0, 0.0],
        scale: 1.0,
    };

    pub fn is_rest(self) -> bool {
        const EPSILON: f32 = 1e-6;
        self.rotation.abs() <= EPSILON
            && self.translation[0].abs() <= EPSILON
            && self.translation[1].abs() <= EPSILON
            && (self.scale - 1.0).abs() <= EPSILON
    }
}

/// The local pose of every bone in a document.
#[derive(Clone, Debug, PartialEq)]
pub struct Pose {
    locals: Vec<BonePose>,
}

impl Pose {
    /// A pose with every bone at rest.
    pub fn rest(bone_count: usize) -> Self {
        Self {
            locals: vec![BonePose::REST; bone_count],
        }
    }

    pub fn len(&self) -> usize {
        self.locals.len()
    }

    pub fn is_empty(&self) -> bool {
        self.locals.is_empty()
    }

    pub fn local(&self, bone: usize) -> Option<BonePose> {
        self.locals.get(bone).copied()
    }

    pub fn locals(&self) -> &[BonePose] {
        &self.locals
    }

    pub fn locals_mut(&mut self) -> &mut [BonePose] {
        &mut self.locals
    }
}

/// Builds a pose by summing every layer's curves.
///
/// Animations that do not exist and curves that no keyframes define are
/// skipped. Curve values are scaled by the layer weight and accumulated over
/// rest: `rest + Σ weight * (value - rest)`.
pub fn sample_pose(art: &Viveco<'_>, layers: &[Layer]) -> Pose {
    let mut pose = Pose::rest(art.bone_count());

    for layer in layers {
        if !layer.weight.is_finite() || layer.weight == 0.0 {
            continue;
        }
        let Some(animation) = art.animation(layer.animation) else {
            continue;
        };
        let duration = animation.duration();
        let cyclic = animation.cyclic();

        for curve in animation.curves() {
            let Some(property) = curve.property() else {
                continue;
            };
            let Some(value) = sample_curve(curve, layer.time, duration, cyclic) else {
                continue;
            };
            let bone = curve.bone();
            let Some(local) = pose.locals.get_mut(bone) else {
                continue;
            };
            let delta = (value - property.rest()) * layer.weight;
            match property {
                BoneProperty::X => local.translation[0] += delta,
                BoneProperty::Y => local.translation[1] += delta,
                BoneProperty::Angle => local.rotation += delta,
                BoneProperty::Scale => local.scale += delta,
            }
        }
    }

    pose
}

/// Computes the world transform of every bone.
///
/// Bones are stored parents-first, so one pass suffices. A bone rotates and
/// scales about its rest head: `pivot + translation + scale * rotate(p - pivot)`.
pub fn bone_worlds(art: &Viveco<'_>, pose: &Pose) -> Vec<Similarity> {
    let mut worlds = vec![Similarity::IDENTITY; art.bone_count()];

    for index in 0..art.bone_count() {
        let Some(bone) = art.bone(index) else {
            continue;
        };
        let local_pose = pose.local(index).unwrap_or(BonePose::REST);

        let pivot = bone.head();
        let linear = Similarity {
            scale: local_pose.scale,
            rotation: local_pose.rotation,
            translation: [0.0, 0.0],
        };
        let rotated_pivot = linear.apply_vector(pivot);
        let local = Similarity {
            scale: local_pose.scale,
            rotation: local_pose.rotation,
            translation: [
                pivot[0] + local_pose.translation[0] - rotated_pivot[0],
                pivot[1] + local_pose.translation[1] - rotated_pivot[1],
            ],
        };

        let parent = bone
            .parent()
            .and_then(|parent| worlds.get(parent))
            .copied()
            .unwrap_or(Similarity::IDENTITY);
        worlds[index] = Similarity::compose(parent, local);
    }

    worlds
}

fn rotate(vector: [f32; 2], rotation: f32) -> [f32; 2] {
    let (sin, cos) = rotation.sin_cos();
    [
        cos * vector[0] - sin * vector[1],
        sin * vector[0] + cos * vector[1],
    ]
}
