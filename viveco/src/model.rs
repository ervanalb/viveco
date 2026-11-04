//! Serializable vector-art document model.
//!
//! A document is a stack of [`Shape`]s ordered by a Z coordinate. A shape owns
//! zero or more [`Path`]s, which are filled together as a single compound path
//! (so holes can be made by giving inner contours the opposite winding), and
//! zero or more child shapes. Children are clipped to the interior of their
//! parent and are painted above the parent's fill but beneath its stroke.
//!
//! Z only orders shapes among their siblings. Moving a shape therefore carries
//! its entire subtree with it and never disturbs the parent/child clipping
//! relationship.
//!
//! Documents may also carry a skeleton of [`Bone`]s. Bones are an editor-only
//! overlay: they do not paint, and they do not affect shape bounds. Each bone
//! stores its two endpoints in document space and nests its children, forming
//! a hierarchical rig that later stages can bind vertices to. Because points
//! are absolute, editing a bone rewrites its descendants too, so the stored
//! geometry always describes where every joint actually is.
//!
//! Coordinates are plain document units with the origin at the top-left and Y
//! pointing down (the usual vector-graphics convention).

use std::cmp::Ordering;
use std::fmt;
use std::io;
use std::path::Path as FsPath;

use serde::de;
use serde::ser::SerializeMap;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Identifier written to and expected in a document's `type` field.
pub const FORMAT_TYPE: &str = "viveco";

/// Oldest document version this build can load.
pub const MIN_FORMAT_VERSION: u32 = 5;

/// Version written to and expected by this build of the format.
pub const FORMAT_VERSION: u32 = 5;

/// A 2D point/vector in document space.
pub type Point = [f32; 2];

/// Flat index of a bone, in depth-first pre-order.
///
/// `0` is the first top-level bone, followed by its whole subtree, then the
/// next top-level bone, and so on. A bone's subtree therefore occupies a
/// contiguous range of indices. Every reference to a bone (weights and
/// animation targets) stores this index; inserting, removing, or reparenting a
/// bone rewrites the references of the whole document.
pub type BoneIndex = usize;

/// Flat index of a shape, in depth-first pre-order.
///
/// Works like [`BoneIndex`] over [`Document::shapes`].
pub type ShapeIndex = usize;

// ---------------------------------------------------------------------------
// Weights
// ---------------------------------------------------------------------------

/// One bone's influence on a single path point.
///
/// `bone` is the flat [`BoneIndex`] of the bone it binds to.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Weight {
    /// Flat index of the bone this weight binds to.
    pub bone: BoneIndex,
    /// How strongly the bone moves the point.
    pub weight: f32,
}

/// The bones influencing one path point: zero or more `(bone, weight)` pairs.
///
/// Anchors and control points carry their own list. Every operation that
/// inserts, removes, or moves a bone keeps these lists consistent, and
/// splitting a segment blends the two neighbouring anchors' lists.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Weights(Vec<Weight>);

impl Weights {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = &Weight> {
        self.0.iter()
    }

    /// The weight bound to `bone`, or `None` when the point is not influenced
    /// by it.
    pub fn get(&self, bone: BoneIndex) -> Option<f32> {
        self.0
            .iter()
            .find(|weight| weight.bone == bone)
            .map(|weight| weight.weight)
    }

    /// The total influence of all bones on the point.
    pub fn sum(&self) -> f32 {
        self.0.iter().map(|weight| weight.weight).sum()
    }

    /// Adds or replaces the weight for `bone`.
    pub fn set(&mut self, bone: BoneIndex, weight: f32) {
        if let Some(existing) = self.0.iter_mut().find(|entry| entry.bone == bone) {
            existing.weight = weight;
        } else {
            self.0.push(Weight { bone, weight });
        }
    }

    /// Blends two points' weights by `t`, where `t == 0` gives `from` and
    /// `t == 1` gives `to`.
    ///
    /// Bones missing from one side count as zero there, so a bone that only
    /// influences one endpoint fades in or out across the segment. Entries
    /// that cancel to exactly zero are dropped.
    pub fn blend(from: &Self, to: &Self, t: f32) -> Self {
        let mut blended = Self::new();
        for weight in &from.0 {
            let other = to.get(weight.bone).unwrap_or(0.0);
            blended.set(weight.bone, weight.weight * (1.0 - t) + other * t);
        }
        for weight in &to.0 {
            if from.get(weight.bone).is_none() {
                blended.set(weight.bone, weight.weight * t);
            }
        }
        blended.0.retain(|weight| weight.weight != 0.0);
        blended
    }

    /// Rewrites every weight through `map`, dropping bones the map deletes and
    /// rescaling the survivors so the list still totals what it did before.
    ///
    /// `map[old]` is the bone's new index, or `None` when the bone is gone.
    /// If no bone remains there is no total to preserve, so the list simply
    /// becomes empty.
    pub fn apply_bone_map(&mut self, map: &[Option<BoneIndex>]) {
        let before = self.sum();
        let mut dropped = false;
        self.0
            .retain_mut(|weight| match map.get(weight.bone).copied().flatten() {
                Some(index) => {
                    weight.bone = index;
                    true
                }
                None => {
                    dropped = true;
                    false
                }
            });
        if dropped {
            let after = self.sum();
            if after > f32::EPSILON {
                let scale = before / after;
                for weight in &mut self.0 {
                    weight.weight *= scale;
                }
            }
        }
    }

    /// Composites a brush's `value` onto `bone`'s weight with opacity `alpha`,
    /// then scales the other bones back so the list never totals more than
    /// one.
    ///
    /// This is the "over" operation of a weight-paint brush: the painted bone
    /// moves toward `value` by `alpha`, and whatever influence it gains is
    /// taken from the others. Weights that fade to zero are dropped.
    pub fn paint_bone(&mut self, bone: BoneIndex, value: f32, alpha: f32) {
        let alpha = alpha.clamp(0.0, 1.0);
        if alpha <= f32::EPSILON {
            return;
        }
        let old = self.get(bone).unwrap_or(0.0);
        let new = (value.clamp(0.0, 1.0) * alpha + old * (1.0 - alpha)).clamp(0.0, 1.0);
        self.set(bone, new);

        // Only the part of the unit budget the painted bone leaves unused may
        // be shared by the others.
        let others: f32 = self
            .0
            .iter()
            .filter(|weight| weight.bone != bone)
            .map(|weight| weight.weight)
            .sum();
        let budget = (1.0 - new).max(0.0);
        if others > budget && others > f32::EPSILON {
            let scale = budget / others;
            for weight in &mut self.0 {
                if weight.bone != bone {
                    weight.weight *= scale;
                }
            }
        }
        self.remove_zero();
    }

    /// Composites a brush's `value` onto the list's unused share `1 - sum`,
    /// scaling every weight to match.
    ///
    /// This is the inverse of [`Self::paint_bone`], used when no bone is
    /// selected: raising the brush value reduces each bone's influence, while
    /// lowering it restores influence. A list with no weights has nothing to
    /// scale and is left untouched.
    pub fn paint_total(&mut self, value: f32, alpha: f32) {
        let alpha = alpha.clamp(0.0, 1.0);
        if alpha <= f32::EPSILON || self.0.is_empty() {
            return;
        }
        let sum = self.sum();
        let display = 1.0 - sum;
        let new_display = (value.clamp(0.0, 1.0) * alpha + display * (1.0 - alpha)).clamp(0.0, 1.0);
        let new_sum = 1.0 - new_display;
        if sum > f32::EPSILON {
            let scale = new_sum / sum;
            for weight in &mut self.0 {
                weight.weight *= scale;
            }
        }
        self.remove_zero();
    }

    /// Drops weights that have faded to zero.
    fn remove_zero(&mut self) {
        const EPSILON: f32 = 1e-6;
        self.0.retain(|weight| weight.weight > EPSILON);
    }
}

// ---------------------------------------------------------------------------
// Color
// ---------------------------------------------------------------------------

/// An 8-bit sRGB color with straight (non-premultiplied) alpha.
///
/// Serializes as `"#rrggbb"` when opaque, or `"#rrggbbaa"` otherwise.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rgba8 {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Rgba8 {
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b, a: 255 }
    }

    pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self { r, g, b, a }
    }

    pub fn to_hex(self) -> String {
        if self.a == 255 {
            format!("#{:02x}{:02x}{:02x}", self.r, self.g, self.b)
        } else {
            format!("#{:02x}{:02x}{:02x}{:02x}", self.r, self.g, self.b, self.a)
        }
    }

    /// Parses `#rgb`, `#rrggbb` or `#rrggbbaa` (the leading `#` is optional).
    pub fn from_hex(text: &str) -> Result<Self, String> {
        let hex = text.trim().trim_start_matches('#');
        if !hex.is_ascii() {
            return Err(format!("invalid hex color {text:?}"));
        }

        let component = |range: std::ops::Range<usize>| {
            u8::from_str_radix(&hex[range], 16).map_err(|_| format!("invalid hex color {text:?}"))
        };

        match hex.len() {
            3 => {
                // #rgb -> #rrggbb
                let digit = |i: usize| {
                    u8::from_str_radix(&hex[i..i + 1], 16)
                        .map(|v| v * 17)
                        .map_err(|_| format!("invalid hex color {text:?}"))
                };
                Ok(Self::rgb(digit(0)?, digit(1)?, digit(2)?))
            }
            6 => Ok(Self::rgb(
                component(0..2)?,
                component(2..4)?,
                component(4..6)?,
            )),
            8 => Ok(Self::rgba(
                component(0..2)?,
                component(2..4)?,
                component(4..6)?,
                component(6..8)?,
            )),
            _ => Err(format!("invalid hex color {text:?}")),
        }
    }
}

impl fmt::Display for Rgba8 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_hex())
    }
}

impl Serialize for Rgba8 {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_hex())
    }
}

impl<'de> Deserialize<'de> for Rgba8 {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        Rgba8::from_hex(&text).map_err(de::Error::custom)
    }
}

// ---------------------------------------------------------------------------
// Document
// ---------------------------------------------------------------------------

/// The root of a vector-art file.
#[derive(Clone, Debug, PartialEq)]
pub struct Document {
    /// Format version; currently [`FORMAT_VERSION`].
    pub version: u32,

    pub shapes: Vec<Shape>,

    /// Top-level bones of the rig, in tree order. Empty for an unrigged
    /// document.
    pub bones: Vec<Bone>,

    /// Animation clips stored in the document.
    pub animations: Vec<Animation>,
}

impl Serialize for Document {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(None)?;
        map.serialize_entry("type", FORMAT_TYPE)?;
        map.serialize_entry("version", &self.version)?;
        map.serialize_entry("shapes", &self.shapes)?;
        if !self.bones.is_empty() {
            map.serialize_entry("bones", &self.bones)?;
        }
        if !self.animations.is_empty() {
            map.serialize_entry("animations", &self.animations)?;
        }
        map.end()
    }
}

impl<'de> Deserialize<'de> for Document {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Wire {
            #[serde(rename = "type")]
            kind: String,
            version: u32,
            #[serde(default)]
            shapes: Vec<Shape>,
            #[serde(default)]
            bones: Vec<Bone>,
            #[serde(default)]
            animations: Vec<Animation>,
        }

        let wire = Wire::deserialize(deserializer)?;
        if wire.kind != FORMAT_TYPE {
            return Err(de::Error::custom(format!(
                "expected type {FORMAT_TYPE:?}, found {:?}",
                wire.kind
            )));
        }
        if !(MIN_FORMAT_VERSION..=FORMAT_VERSION).contains(&wire.version) {
            return Err(de::Error::custom(format!(
                "unsupported viveco version {} (supported {MIN_FORMAT_VERSION}..={FORMAT_VERSION})",
                wire.version
            )));
        }

        Ok(Self {
            version: wire.version,
            shapes: wire.shapes,
            bones: wire.bones,
            animations: wire.animations,
        })
    }
}

impl Default for Document {
    fn default() -> Self {
        Self::new()
    }
}

impl Document {
    pub fn new() -> Self {
        Self {
            version: FORMAT_VERSION,
            shapes: Vec::new(),
            bones: Vec::new(),
            animations: Vec::new(),
        }
    }

    /// Indices of the top-level [`Self::shapes`] in painting order (ascending
    /// Z, stable for equal Z). The first index is painted furthest back.
    pub fn shape_indices_in_paint_order(&self) -> Vec<usize> {
        indices_in_paint_order(&self.shapes)
    }

    /// Top-level shapes in painting order (ascending Z, stable for equal Z).
    pub fn sorted_shapes(&self) -> Vec<&Shape> {
        self.shape_indices_in_paint_order()
            .into_iter()
            .map(|index| &self.shapes[index])
            .collect()
    }

    /// Axis-aligned bounds of all geometry in the document, including the
    /// geometry of nested children.
    ///
    /// Bézier control points are included, so the result is a conservative bound.
    pub fn bounds(&self) -> Option<Bounds> {
        let mut acc = BoundsAccumulator::default();
        for shape in &self.shapes {
            acc.add_shape(shape);
        }
        acc.finish()
    }

    pub fn from_json_str(text: &str) -> serde_json::Result<Self> {
        serde_json::from_str(text)
    }

    pub fn to_json_string(&self) -> serde_json::Result<String> {
        serde_json::to_string_pretty(self)
    }

    pub fn load_from_file(path: impl AsRef<FsPath>) -> io::Result<Self> {
        let text = std::fs::read_to_string(path)?;
        Self::from_json_str(&text).map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))
    }

    pub fn save_to_file(&self, path: impl AsRef<FsPath>) -> io::Result<()> {
        let text = self
            .to_json_string()
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
        std::fs::write(path, text)
    }

    /// Applies `f` to the weight list of every path point in every shape,
    /// including nested children.
    fn for_each_weights_mut(&mut self, f: &mut impl FnMut(&mut Weights)) {
        for shape in &mut self.shapes {
            shape.for_each_weights_mut(&mut *f);
        }
    }

    /// Rewrites every reference to a bone through `map`.
    ///
    /// Applies the map to every point's weight list (dropping bones the map
    /// deletes, rescaling each list so it still totals what it did before) and
    /// to every animation keyframe that targets a bone. Keyframes targeting a
    /// deleted bone are dropped.
    pub fn apply_bone_map(&mut self, map: &[Option<BoneIndex>]) {
        self.for_each_weights_mut(&mut |weights| weights.apply_bone_map(map));
        for animation in &mut self.animations {
            animation.keyframes.retain_mut(|keyframe| {
                let KeyframeTarget::Bone { bone, .. } = &mut keyframe.target else {
                    return true;
                };
                match map.get(*bone).copied().flatten() {
                    Some(index) => {
                        *bone = index;
                        true
                    }
                    None => false,
                }
            });
        }
    }

    /// Rewrites every reference to a shape through `map`, dropping keyframes
    /// that target a deleted shape.
    pub fn apply_shape_map(&mut self, map: &[Option<ShapeIndex>]) {
        for animation in &mut self.animations {
            animation.keyframes.retain_mut(|keyframe| {
                let KeyframeTarget::Shape { shape, .. } = &mut keyframe.target else {
                    return true;
                };
                match map.get(*shape).copied().flatten() {
                    Some(index) => {
                        *shape = index;
                        true
                    }
                    None => false,
                }
            });
        }
    }

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

    /// The number of shapes in the document, counting descendants.
    pub fn shape_count(&self) -> usize {
        self.shapes.iter().map(Shape::subtree_len).sum()
    }

    /// The shape with the given pre-order index, if any.
    pub fn shape_by_index(&self, index: ShapeIndex) -> Option<&Shape> {
        fn find<'a>(shapes: &'a [Shape], cursor: &mut usize, target: usize) -> Option<&'a Shape> {
            for shape in shapes {
                let current = *cursor;
                *cursor += 1;
                if current == target {
                    return Some(shape);
                }
                if let Some(found) = find(&shape.children, cursor, target) {
                    return Some(found);
                }
            }
            None
        }
        let mut cursor = 0;
        find(&self.shapes, &mut cursor, index)
    }

    /// The shape with the given pre-order index, if any.
    pub fn shape_by_index_mut(&mut self, index: ShapeIndex) -> Option<&mut Shape> {
        fn find<'a>(
            shapes: &'a mut [Shape],
            cursor: &mut usize,
            target: usize,
        ) -> Option<&'a mut Shape> {
            shapes.iter_mut().find_map(|shape| {
                let current = *cursor;
                *cursor += 1;
                if current == target {
                    Some(shape)
                } else {
                    find(&mut shape.children, cursor, target)
                }
            })
        }
        let mut cursor = 0;
        find(&mut self.shapes, &mut cursor, index)
    }

    /// The number of shapes in the subtree rooted at `index`, including the
    /// shape itself.
    pub fn shape_subtree_len(&self, index: ShapeIndex) -> Option<usize> {
        self.shape_by_index(index).map(Shape::subtree_len)
    }
}

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
// Skeleton
// ---------------------------------------------------------------------------

/// A single bone in the rig's hierarchy.
///
/// A bone is the segment from `head` to `tail`, stored directly in document
/// space. Children are stored the same way, so the hierarchy is purely
/// structural: moving a bone rewrites the endpoints of everything below it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Bone {
    /// Optional human-readable name, shown in the editor's skeleton tree.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,

    /// The origin end of the bone, in document space.
    ///
    /// This may be omitted when the bone has a parent, in which case the head
    /// follows the parent's tail. A root bone has no parent, so its head is
    /// always present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head: Option<Point>,

    /// The far end of the bone, in document space.
    #[serde(default)]
    pub tail: Point,

    /// Nested bones, painted relative to this one.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
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
    /// Builds a bone from document-space endpoints.
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

// ---------------------------------------------------------------------------
// Animations
// ---------------------------------------------------------------------------

/// A property of a bone that an animation curve can drive.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BoneProperty {
    /// Horizontal translation of the bone's local transform.
    X,
    /// Vertical translation of the bone's local transform.
    Y,
    /// Local rotation of the bone, in radians.
    Angle,
    /// Uniform scale of the bone's local transform.
    Scale,
}

/// A property of a shape that an animation curve can drive.
///
/// Shape properties are not implemented yet, so this enum intentionally has no
/// variants: no shape keyframe can be constructed, and any attempt to
/// deserialize one fails.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShapeProperty {}

/// What a [`Keyframe`] drives.
///
/// Bone and shape targets store the target's flat index, which is how the rest
/// of the document identifies both. Inserting, removing, or moving a bone or
/// shape therefore rewrites the target of every keyframe that refers to it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum KeyframeTarget {
    /// Drives one property of a bone.
    Bone {
        /// Flat index of the bone.
        bone: BoneIndex,
        property: BoneProperty,
    },
    /// Drives one property of a shape.
    Shape {
        /// Flat index of the shape.
        shape: ShapeIndex,
        property: ShapeProperty,
    },
}

/// One value of an animation curve at a point in time.
///
/// The two optional handles are the control points of the one-dimensional
/// Bézier curve between this keyframe and its neighbours.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Keyframe {
    /// Position along the animation, in the range `0..=duration`.
    pub timestamp: f32,
    /// The animated value at [`Self::timestamp`].
    pub value: f32,
    /// Bézier handle toward the previous keyframe.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handle_in: Option<f32>,
    /// Bézier handle toward the next keyframe.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handle_out: Option<f32>,
    /// What this keyframe animates.
    pub target: KeyframeTarget,
}

impl Keyframe {
    /// Builds a keyframe with no curve handles.
    pub fn new(timestamp: f32, value: f32, target: KeyframeTarget) -> Self {
        Self {
            timestamp,
            value,
            handle_in: None,
            handle_out: None,
            target,
        }
    }
}

/// A single animation curve driving one target over time.
///
/// Keyframes are kept sorted by timestamp and constrained to `0..=duration`.
/// Both invariants are enforced whenever the list is built through
/// [`Self::insert_keyframe`], [`Self::set_duration`], or by deserialization.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(from = "AnimationData")]
pub struct Animation {
    /// User-visible name. Empty names are shown by position.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    /// Length of the animation. Keyframes may not fall outside it.
    pub duration: f32,
    /// Whether the animation loops back to the start.
    pub cyclic: bool,
    /// The curve's keyframes, ordered by timestamp.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub keyframes: Vec<Keyframe>,
}

impl Default for Animation {
    fn default() -> Self {
        Self {
            name: String::new(),
            duration: 0.0,
            cyclic: false,
            keyframes: Vec::new(),
        }
    }
}

impl Animation {
    /// Builds an empty animation of the given duration.
    pub fn new(duration: f32) -> Self {
        Self {
            duration,
            ..Default::default()
        }
    }

    /// Inserts a keyframe, keeping the list ordered by timestamp.
    ///
    /// Keyframes outside `0..=duration` are ignored, and equal timestamps
    /// place the new keyframe after the existing ones.
    pub fn insert_keyframe(&mut self, keyframe: Keyframe) {
        if !self.contains_time(keyframe.timestamp) {
            return;
        }
        let index = self
            .keyframes
            .partition_point(|existing| existing.timestamp <= keyframe.timestamp);
        self.keyframes.insert(index, keyframe);
    }

    /// Changes the duration, dropping keyframes that now fall outside
    /// `0..=duration`.
    pub fn set_duration(&mut self, duration: f32) {
        self.keyframes
            .retain(|keyframe| timestamp_within(keyframe.timestamp, duration));
        self.duration = duration;
    }

    /// Restores the invariants: sorts keyframes by timestamp and drops those
    /// outside `0..=duration`.
    pub fn normalize(&mut self) {
        let duration = self.duration;
        self.keyframes
            .retain(|keyframe| timestamp_within(keyframe.timestamp, duration));
        self.keyframes.sort_by(|a, b| {
            a.timestamp
                .partial_cmp(&b.timestamp)
                .unwrap_or(Ordering::Equal)
        });
    }

    fn contains_time(&self, timestamp: f32) -> bool {
        timestamp_within(timestamp, self.duration)
    }
}

/// Whether `timestamp` falls in the valid animation range `0..=duration`.
fn timestamp_within(timestamp: f32, duration: f32) -> bool {
    timestamp >= 0.0 && timestamp <= duration
}

/// Deserialization form of [`Animation`], normalized on conversion so a loaded
/// document always satisfies the keyframe invariants.
#[derive(Deserialize)]
struct AnimationData {
    #[serde(default)]
    name: String,
    #[serde(default)]
    duration: f32,
    #[serde(default)]
    cyclic: bool,
    #[serde(default)]
    keyframes: Vec<Keyframe>,
}

impl From<AnimationData> for Animation {
    fn from(data: AnimationData) -> Self {
        let mut animation = Self {
            name: data.name,
            duration: data.duration,
            cyclic: data.cyclic,
            keyframes: data.keyframes,
        };
        animation.normalize();
        animation
    }
}

// ---------------------------------------------------------------------------
// Shapes
// ---------------------------------------------------------------------------

/// A painted shape: a compound path plus optionally clipped child shapes.
///
/// The shape's [`paths`](Self::paths) are combined into a single compound path
/// and filled with a non-zero winding rule, so contours wound opposite to the
/// outer one become holes. The same combined path both paints the fill and
/// clips the shape's [`children`](Self::children). The stroke is drawn on the
/// combined path after the children, so it sits above them.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Shape {
    /// Optional human-readable name, shown in the editor's shape tree.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,

    /// Z order among siblings. Lower values are painted first (further back).
    #[serde(default)]
    pub z: f32,

    #[serde(default)]
    pub style: Style,

    /// Contours combined into this shape's compound path.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub paths: Vec<Path>,

    /// Child shapes, clipped to this shape's interior.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<Shape>,
}

impl Shape {
    /// Indices of [`Self::children`] in painting order (ascending Z, stable for
    /// equal Z).
    pub fn child_indices_in_paint_order(&self) -> Vec<usize> {
        indices_in_paint_order(&self.children)
    }

    /// Child shapes in painting order (ascending Z, stable for equal Z).
    pub fn sorted_children(&self) -> Vec<&Shape> {
        self.child_indices_in_paint_order()
            .into_iter()
            .map(|index| &self.children[index])
            .collect()
    }

    /// True when the shape has neither paths nor children.
    pub fn is_empty(&self) -> bool {
        self.paths.is_empty() && self.children.is_empty()
    }

    /// The number of shapes in this subtree, including the shape itself.
    pub fn subtree_len(&self) -> usize {
        1 + self.children.iter().map(Shape::subtree_len).sum::<usize>()
    }

    /// Applies `f` to every point weight in this shape and its descendants.
    fn for_each_weights_mut(&mut self, f: &mut impl FnMut(&mut Weights)) {
        for path in &mut self.paths {
            path.for_each_weights_mut(&mut *f);
        }
        for child in &mut self.children {
            child.for_each_weights_mut(&mut *f);
        }
    }
}

/// Indices of `shapes` in painting order (ascending Z, stable for equal Z).
pub fn indices_in_paint_order(shapes: &[Shape]) -> Vec<usize> {
    let mut indices: Vec<usize> = (0..shapes.len()).collect();
    indices.sort_by(|&a, &b| {
        shapes[a]
            .z
            .partial_cmp(&shapes[b].z)
            .unwrap_or(Ordering::Equal)
    });
    indices
}

// ---------------------------------------------------------------------------
// Geometry
// ---------------------------------------------------------------------------

/// A single path: how it ends, followed by its segments.
///
/// A closed path has no separate start point: its first segment begins at the
/// end point of the last segment, so the closing edge is an ordinary segment
/// that can be edited like any other. An open path is still implicitly closed
/// when filled, matching common vector-graphics behavior; use stroke-only
/// styles when you need an open outline.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Path {
    /// Optional human-readable name, shown in the editor's shape tree.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,

    pub ends: PathEnds,
    #[serde(default)]
    pub segments: Vec<Segment>,
    /// Bone weights of the open start anchor.
    ///
    /// A closed loop has no separate start: its first anchor is the last
    /// segment's end point, whose weights live in that segment's
    /// [`to_weights`](Segment::to_weights). This list is therefore only used
    /// while the path is open.
    #[serde(default, skip_serializing_if = "Weights::is_empty")]
    pub start_weights: Weights,
}

/// How a path begins, and whether it loops back to that point.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PathEnds {
    /// An open path starting at the given point.
    Open(Point),
    /// A closed loop. The loop starts at the end point of the last segment.
    Closed,
}

impl Path {
    pub fn new(start: Point) -> Self {
        Self {
            name: String::new(),
            ends: PathEnds::Open(start),
            segments: Vec::new(),
            start_weights: Weights::new(),
        }
    }

    /// Sets the human-readable name used by the editor's shape tree.
    pub fn named(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
    }

    /// Appends a straight segment ending at `to`.
    pub fn line_to(mut self, to: Point) -> Self {
        self.segments.push(Segment::line(to));
        self
    }

    /// Appends a cubic Bézier segment ending at `to`.
    pub fn cubic_to(mut self, ctrl1: Point, ctrl2: Point, to: Point) -> Self {
        self.segments.push(Segment {
            ctrl1: Some(ctrl1),
            ctrl2: Some(ctrl2),
            to,
            ..Default::default()
        });
        self
    }

    /// Closes the path, adding a straight segment back to the start when the
    /// last point is not already there.
    pub fn closed(mut self) -> Self {
        self.close();
        self
    }

    /// The first anchor of the path, or `None` for an empty closed loop.
    pub fn start(&self) -> Option<Point> {
        match self.ends {
            PathEnds::Open(start) => Some(start),
            PathEnds::Closed => self.segments.last().map(|segment| segment.to),
        }
    }

    pub fn is_closed(&self) -> bool {
        matches!(self.ends, PathEnds::Closed)
    }

    /// Closes the loop, adding a straight edge from the last point back to the
    /// start when needed. Returns `false` when the path has no geometry.
    pub fn close(&mut self) -> bool {
        if self.segments.is_empty() {
            return false;
        }
        // The closed loop's start is the last segment's end point, so the
        // closing segment carries the start anchor's weights.
        let start_weights = std::mem::take(&mut self.start_weights);
        if let Some(start) = self.start() {
            if !self
                .segments
                .last()
                .is_some_and(|segment| segment.to == start)
            {
                let mut closing = Segment::line(start);
                closing.to_weights = start_weights;
                self.segments.push(closing);
            } else if self
                .segments
                .last()
                .is_some_and(|segment| segment.to_weights.is_empty())
                && let Some(last) = self.segments.last_mut()
            {
                last.to_weights = start_weights;
            }
        }
        self.ends = PathEnds::Closed;
        true
    }

    /// Opens the loop, dropping the closing segment and using its end point as
    /// the new start. Returns `false` when there is no closing segment.
    pub fn open(&mut self) -> bool {
        match self.ends {
            PathEnds::Open(_) => true,
            PathEnds::Closed => {
                let Some(last) = self.segments.pop() else {
                    return false;
                };
                self.ends = PathEnds::Open(last.to);
                self.start_weights = last.to_weights;
                true
            }
        }
    }

    /// Applies `f` to every point's weight list: the open start and each
    /// segment's two controls and end point.
    fn for_each_weights_mut(&mut self, f: &mut impl FnMut(&mut Weights)) {
        f(&mut self.start_weights);
        for segment in &mut self.segments {
            f(&mut segment.ctrl1_weights);
            f(&mut segment.ctrl2_weights);
            f(&mut segment.to_weights);
        }
    }
}

/// One segment of a [`Path`], ending at `to`.
///
/// `ctrl1` is the Bézier control point near the segment's start and `ctrl2`
/// the one near `to`. An omitted control is treated as lying exactly on its
/// endpoint, so it has no visible effect and the editor draws no handle for
/// it. A segment with no controls is a straight line.
///
/// Each of the three points carries its own bone weights. A control's weights
/// mirror the anchor it belongs to when it is created, and the end point's
/// weights are those of the anchor at `to`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Segment {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ctrl1: Option<Point>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ctrl2: Option<Point>,
    pub to: Point,
    /// Bone weights of the control near the segment's start.
    #[serde(default, skip_serializing_if = "Weights::is_empty")]
    pub ctrl1_weights: Weights,
    /// Bone weights of the control near the segment's end.
    #[serde(default, skip_serializing_if = "Weights::is_empty")]
    pub ctrl2_weights: Weights,
    /// Bone weights of the anchor at the segment's end.
    #[serde(default, skip_serializing_if = "Weights::is_empty")]
    pub to_weights: Weights,
}

impl Default for Segment {
    fn default() -> Self {
        Self {
            ctrl1: None,
            ctrl2: None,
            to: [0.0, 0.0],
            ctrl1_weights: Weights::new(),
            ctrl2_weights: Weights::new(),
            to_weights: Weights::new(),
        }
    }
}

impl Segment {
    /// A straight segment: both control points are omitted.
    pub fn line(to: Point) -> Self {
        Self {
            to,
            ..Default::default()
        }
    }

    /// True when neither control point is present.
    pub fn is_line(&self) -> bool {
        self.ctrl1.is_none() && self.ctrl2.is_none()
    }
}

// ---------------------------------------------------------------------------
// Paint
// ---------------------------------------------------------------------------

/// Stroke width used for new and default strokes, in document units.
pub const DEFAULT_STROKE_WIDTH: f32 = 2.0;

/// Solid fills and/or strokes applied to a shape.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Style {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fill: Option<Rgba8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stroke: Option<StrokeStyle>,
}

/// A solid-color stroke of constant width.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct StrokeStyle {
    pub color: Rgba8,
    pub width: f32,
}

// ---------------------------------------------------------------------------
// Bounds
// ---------------------------------------------------------------------------

/// Axis-aligned rectangle in document space.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Bounds {
    pub min: Point,
    pub max: Point,
}

impl Bounds {
    pub fn width(&self) -> f32 {
        self.max[0] - self.min[0]
    }

    pub fn height(&self) -> f32 {
        self.max[1] - self.min[1]
    }

    pub fn center(&self) -> Point {
        [
            (self.min[0] + self.max[0]) * 0.5,
            (self.min[1] + self.max[1]) * 0.5,
        ]
    }
}

#[derive(Default)]
struct BoundsAccumulator {
    min: Point,
    max: Point,
    any: bool,
}

impl BoundsAccumulator {
    fn add_point(&mut self, point: Point) {
        if self.any {
            self.min[0] = self.min[0].min(point[0]);
            self.min[1] = self.min[1].min(point[1]);
            self.max[0] = self.max[0].max(point[0]);
            self.max[1] = self.max[1].max(point[1]);
        } else {
            self.min = point;
            self.max = point;
            self.any = true;
        }
    }

    fn add_path(&mut self, path: &Path) {
        if let Some(start) = path.start() {
            self.add_point(start);
        }
        for segment in &path.segments {
            if let Some(ctrl1) = segment.ctrl1 {
                self.add_point(ctrl1);
            }
            if let Some(ctrl2) = segment.ctrl2 {
                self.add_point(ctrl2);
            }
            self.add_point(segment.to);
        }
    }

    fn add_shape(&mut self, shape: &Shape) {
        for path in &shape.paths {
            self.add_path(path);
        }
        for child in &shape.children {
            self.add_shape(child);
        }
    }

    fn finish(self) -> Option<Bounds> {
        self.any.then_some(Bounds {
            min: self.min,
            max: self.max,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_path() -> Path {
        Path::new([10.0, 20.0])
            .line_to([30.0, 20.0])
            .cubic_to([40.0, 25.0], [50.0, 35.0], [60.0, 40.0])
            .closed()
    }

    fn sample_document() -> Document {
        Document {
            version: FORMAT_VERSION,
            bones: Vec::new(),
            animations: Vec::new(),
            shapes: vec![Shape {
                name: "Outer".to_string(),
                z: 5.0,
                style: Style {
                    fill: Some(Rgba8::rgb(0xb3, 0xb3, 0xb3)),
                    stroke: Some(StrokeStyle {
                        color: Rgba8::rgba(0, 0, 0, 128),
                        width: 2.0,
                    }),
                },
                paths: vec![sample_path()],
                children: vec![Shape {
                    name: "Child".to_string(),
                    z: 1.0,
                    style: Style::default(),
                    paths: vec![Path::new([0.0, 0.0]).line_to([1.0, 1.0])],
                    children: Vec::new(),
                }],
            }],
        }
    }

    #[test]
    fn json_roundtrip() {
        let doc = sample_document();
        let json = doc.to_json_string().unwrap();
        let decoded = Document::from_json_str(&json).unwrap();
        assert_eq!(doc, decoded);
    }

    #[test]
    fn hex_color_roundtrip() {
        assert_eq!(Rgba8::rgb(0x00, 0x00, 0xff).to_hex(), "#0000ff");
        assert_eq!(Rgba8::rgba(0x12, 0x34, 0x56, 0x78).to_hex(), "#12345678");
        assert_eq!(
            Rgba8::from_hex("#abc").unwrap(),
            Rgba8::rgb(0xaa, 0xbb, 0xcc)
        );
        assert_eq!(
            Rgba8::from_hex("12345678").unwrap(),
            Rgba8::rgba(0x12, 0x34, 0x56, 0x78)
        );
        assert!(Rgba8::from_hex("#12345").is_err());
    }

    #[test]
    fn closing_and_opening_around_a_loop_round_trips() {
        let mut path = Path::new([0.0, 0.0])
            .line_to([10.0, 0.0])
            .line_to([10.0, 10.0]);

        assert!(path.close());
        assert!(path.is_closed());
        assert_eq!(path.start(), Some([0.0, 0.0]));
        assert_eq!(path.segments.len(), 3);
        assert_eq!(path.segments[2], Segment::line([0.0, 0.0]));

        assert!(path.open());
        assert!(!path.is_closed());
        assert_eq!(path.start(), Some([0.0, 0.0]));
        assert_eq!(path.segments.len(), 2);
    }

    #[test]
    fn siblings_are_sorted_by_z() {
        let doc = Document {
            version: FORMAT_VERSION,
            bones: Vec::new(),
            animations: Vec::new(),
            shapes: vec![
                Shape {
                    z: 10.0,
                    ..Default::default()
                },
                Shape {
                    z: -1.0,
                    ..Default::default()
                },
                Shape {
                    z: 2.0,
                    ..Default::default()
                },
            ],
        };

        let zs: Vec<f32> = doc.sorted_shapes().iter().map(|shape| shape.z).collect();
        assert_eq!(zs, vec![-1.0, 2.0, 10.0]);

        let parent = Shape {
            z: 0.0,
            children: vec![
                Shape {
                    z: 3.0,
                    ..Default::default()
                },
                Shape {
                    z: 0.0,
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let child_zs: Vec<f32> = parent
            .sorted_children()
            .iter()
            .map(|child| child.z)
            .collect();
        assert_eq!(child_zs, vec![0.0, 3.0]);
    }

    #[test]
    fn bounds_include_nested_shapes_and_controls() {
        let doc = Document {
            version: FORMAT_VERSION,
            bones: Vec::new(),
            animations: Vec::new(),
            shapes: vec![
                Shape {
                    paths: vec![Path::new([0.0, 0.0]).line_to([10.0, 10.0])],
                    ..Default::default()
                },
                Shape {
                    paths: vec![Path::new([-5.0, -5.0]).line_to([20.0, 20.0])],
                    children: vec![Shape {
                        paths: vec![Path::new([100.0, 100.0]).cubic_to(
                            [110.0, 120.0],
                            [130.0, 140.0],
                            [150.0, 150.0],
                        )],
                        ..Default::default()
                    }],
                    ..Default::default()
                },
            ],
        };

        let bounds = doc.bounds().unwrap();
        assert_eq!(bounds.min, [-5.0, -5.0]);
        assert_eq!(bounds.max, [150.0, 150.0]);
    }

    #[test]
    fn weights_blend_by_parameter() {
        let mut from = Weights::new();
        from.set(0, 1.0);
        from.set(1, 0.5);
        let mut to = Weights::new();
        to.set(1, 1.0);
        to.set(2, 1.0);

        let blended = Weights::blend(&from, &to, 0.25);
        assert_eq!(blended.get(0), Some(0.75));
        assert!((blended.get(1).unwrap() - 0.625).abs() < 1e-6);
        assert_eq!(blended.get(2), Some(0.25));

        // The endpoints reproduce the inputs exactly.
        assert_eq!(Weights::blend(&from, &to, 0.0).iter().count(), 2);
        assert_eq!(Weights::blend(&from, &to, 1.0).iter().count(), 2);
    }

    #[test]
    fn removing_a_bone_keeps_the_weight_total() {
        let mut weights = Weights::new();
        weights.set(0, 0.5);
        weights.set(1, 0.5);

        // Bone 1 is gone; bone 0 stays where it was.
        weights.apply_bone_map(&removal_map(2, 1, 1));

        assert_eq!(weights.get(1), None);
        assert!((weights.sum() - 1.0).abs() < 1e-6);
        assert!((weights.get(0).unwrap() - 1.0).abs() < 1e-6);
    }

    #[test]
    fn applying_a_bone_map_shifts_indices() {
        let mut weights = Weights::new();
        weights.set(0, 0.25);
        weights.set(1, 0.75);

        // A new bone appears at index 0, pushing both along.
        weights.apply_bone_map(&insertion_map(2, 0, 1));

        assert_eq!(weights.get(0), None);
        assert_eq!(weights.get(1), Some(0.25));
        assert_eq!(weights.get(2), Some(0.75));
    }

    #[test]
    fn weights_round_trip_through_json() {
        let mut path = Path::new([0.0, 0.0]).cubic_to([1.0, 2.0], [3.0, 4.0], [5.0, 6.0]);
        path.start_weights.set(0, 0.25);
        path.segments[0].ctrl1_weights.set(0, 0.5);
        path.segments[0].ctrl2_weights.set(0, 0.75);
        path.segments[0].to_weights.set(0, 1.0);
        let doc = Document {
            version: FORMAT_VERSION,
            bones: Vec::new(),
            animations: Vec::new(),
            shapes: vec![Shape {
                paths: vec![path],
                ..Default::default()
            }],
        };

        let json = doc.to_json_string().unwrap();
        let decoded = Document::from_json_str(&json).unwrap();
        assert_eq!(doc, decoded);
    }

    #[test]
    fn painting_a_bone_scales_the_others_back() {
        let mut weights = Weights::new();
        weights.set(0, 0.2);
        weights.set(1, 0.8);

        // A full-strength dab sets bone 0 to 0.7 and shrinks the rest to fit.
        weights.paint_bone(0, 0.7, 1.0);

        assert!((weights.get(0).unwrap() - 0.7).abs() < 1e-6);
        assert!((weights.get(1).unwrap() - 0.3).abs() < 1e-6);
        assert!((weights.sum() - 1.0).abs() < 1e-6);
    }

    #[test]
    fn painting_a_bone_partially_blends_towards_the_value() {
        let mut weights = Weights::new();
        weights.set(0, 0.0);

        weights.paint_bone(0, 1.0, 0.25);

        assert!((weights.get(0).unwrap() - 0.25).abs() < 1e-6);
    }

    #[test]
    fn painting_a_bone_to_zero_drops_it() {
        let mut weights = Weights::new();
        weights.set(0, 0.5);
        weights.set(1, 0.5);

        weights.paint_bone(0, 0.0, 1.0);

        assert_eq!(weights.get(0), None);
        // Erasing one bone does not inflate the others.
        assert!((weights.get(1).unwrap() - 0.5).abs() < 1e-6);
    }

    #[test]
    fn painting_without_a_bone_reduces_the_total() {
        let mut weights = Weights::new();
        weights.set(0, 0.5);
        weights.set(1, 0.5);

        // The displayed value is `1 - sum`, so a value of one removes all
        // influence.
        weights.paint_total(1.0, 1.0);

        assert!(weights.is_empty());
    }

    #[test]
    fn painting_a_total_onto_an_unweighted_point_does_nothing() {
        let mut weights = Weights::new();

        weights.paint_total(0.0, 1.0);

        // There is no bone to scale, so the point stays unweighted.
        assert!(weights.is_empty());
    }

    fn bone_keyframe(
        timestamp: f32,
        value: f32,
        bone: BoneIndex,
        property: BoneProperty,
    ) -> Keyframe {
        Keyframe::new(timestamp, value, KeyframeTarget::Bone { bone, property })
    }

    fn keyframe_times(animation: &Animation) -> Vec<f32> {
        animation
            .keyframes
            .iter()
            .map(|keyframe| keyframe.timestamp)
            .collect()
    }

    #[test]
    fn animations_round_trip_through_json() {
        let mut animation = Animation::new(5.0);
        animation.cyclic = true;
        let mut keyframe = bone_keyframe(1.5, 10.0, 0, BoneProperty::Angle);
        keyframe.handle_in = Some(0.25);
        keyframe.handle_out = Some(0.75);
        animation.insert_keyframe(keyframe);

        let document = Document {
            animations: vec![animation],
            ..Default::default()
        };

        let json = document.to_json_string().unwrap();
        assert_eq!(Document::from_json_str(&json).unwrap(), document);

        // Omitted handles and empty animation lists stay out of the file.
        let empty = Document {
            animations: vec![Animation::new(1.0)],
            ..Default::default()
        };
        let json = empty.to_json_string().unwrap();
        assert!(!json.contains("keyframes"));
        assert!(!json.contains("handle"));
    }

    #[test]
    fn animation_names_round_trip_and_are_omitted_when_empty() {
        let named = Document {
            animations: vec![Animation {
                name: "Walk".into(),
                ..Animation::new(2.0)
            }],
            ..Default::default()
        };
        let json = named.to_json_string().unwrap();
        assert!(json.contains("\"Walk\""));
        assert_eq!(Document::from_json_str(&json).unwrap(), named);

        let unnamed = Document {
            animations: vec![Animation::new(2.0)],
            ..Default::default()
        };
        let json = unnamed.to_json_string().unwrap();
        assert!(!json.contains("\"name\""));
    }

    #[test]
    fn inserting_keyframes_keeps_them_ordered_and_in_range() {
        let mut animation = Animation::new(10.0);
        animation.insert_keyframe(bone_keyframe(5.0, 0.0, 1, BoneProperty::X));
        animation.insert_keyframe(bone_keyframe(1.0, 0.0, 0, BoneProperty::X));
        animation.insert_keyframe(bone_keyframe(5.0, 0.0, 2, BoneProperty::Y));
        animation.insert_keyframe(bone_keyframe(-1.0, 0.0, 3, BoneProperty::X));
        animation.insert_keyframe(bone_keyframe(11.0, 0.0, 4, BoneProperty::X));

        assert_eq!(keyframe_times(&animation), vec![1.0, 5.0, 5.0]);

        // Equal timestamps keep insertion order.
        let bones: Vec<BoneIndex> = animation
            .keyframes
            .iter()
            .map(|keyframe| match &keyframe.target {
                KeyframeTarget::Bone { bone, .. } => *bone,
                KeyframeTarget::Shape { .. } => unreachable!(),
            })
            .collect();
        assert_eq!(bones, vec![0, 1, 2]);
    }

    #[test]
    fn changing_duration_drops_outlying_keyframes() {
        let mut animation = Animation::new(10.0);
        for timestamp in [0.0, 2.0, 8.0, 10.0] {
            animation.insert_keyframe(bone_keyframe(timestamp, timestamp, 0, BoneProperty::X));
        }

        animation.set_duration(5.0);

        assert_eq!(animation.duration, 5.0);
        assert_eq!(keyframe_times(&animation), vec![0.0, 2.0]);
    }

    #[test]
    fn loading_animations_normalizes_their_keyframes() {
        let json = r#"{
            "type": "viveco",
            "version": 5,
            "animations": [{
                "duration": 4.0,
                "cyclic": true,
                "keyframes": [
                    {"timestamp": 3.0, "value": 3.0,
                     "target": {"kind": "bone", "bone": 0, "property": "x"}},
                    {"timestamp": -1.0, "value": 0.0,
                     "target": {"kind": "bone", "bone": 1, "property": "y"}},
                    {"timestamp": 1.0, "value": 1.0,
                     "target": {"kind": "bone", "bone": 2, "property": "angle"}},
                    {"timestamp": 9.0, "value": 9.0,
                     "target": {"kind": "bone", "bone": 3, "property": "scale"}}
                ]
            }]
        }"#;

        let document = Document::from_json_str(json).unwrap();
        let animation = &document.animations[0];
        assert!(animation.cyclic);
        assert_eq!(keyframe_times(animation), vec![1.0, 3.0]);
    }

    #[test]
    fn applying_a_bone_map_updates_animation_targets() {
        let mut animation = Animation::new(1.0);
        animation.insert_keyframe(bone_keyframe(0.0, 0.0, 0, BoneProperty::X));
        animation.insert_keyframe(bone_keyframe(1.0, 0.0, 1, BoneProperty::X));
        let mut document = Document {
            animations: vec![animation],
            ..Default::default()
        };

        // Insert a bone at index 0, pushing both targets along.
        document.apply_bone_map(&insertion_map(2, 0, 1));

        let bones: Vec<BoneIndex> = document.animations[0]
            .keyframes
            .iter()
            .map(|keyframe| match &keyframe.target {
                KeyframeTarget::Bone { bone, .. } => *bone,
                KeyframeTarget::Shape { .. } => unreachable!(),
            })
            .collect();
        assert_eq!(bones, vec![1, 2]);
    }

    #[test]
    fn applying_a_bone_map_drops_targets_of_removed_bones() {
        let mut animation = Animation::new(1.0);
        animation.insert_keyframe(bone_keyframe(0.0, 0.0, 0, BoneProperty::X));
        animation.insert_keyframe(bone_keyframe(1.0, 0.0, 1, BoneProperty::X));
        let mut document = Document {
            animations: vec![animation],
            ..Default::default()
        };

        document.apply_bone_map(&removal_map(2, 0, 1));

        assert_eq!(document.animations[0].keyframes.len(), 1);
        assert!(matches!(
            document.animations[0].keyframes[0].target,
            KeyframeTarget::Bone { bone: 0, .. }
        ));
    }

    #[test]
    fn shape_keyframes_have_no_properties_yet() {
        // `ShapeProperty` is uninhabited, so a shape-targeted keyframe cannot
        // be constructed or deserialized.
        let json = r#"{
            "type": "viveco",
            "version": 5,
            "animations": [{
                "duration": 1.0,
                "keyframes": [
                    {"timestamp": 0.0, "value": 0.0,
                     "target": {"kind": "shape", "shape": 0, "property": "x"}}
                ]
            }]
        }"#;
        assert!(Document::from_json_str(json).is_err());
    }

    #[test]
    fn pre_order_bone_indices_match_a_depth_first_walk() {
        let document = Document {
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

        let names: Vec<&str> = (0..document.bone_count())
            .map(|index| document.bone_by_index(index).unwrap().name.as_str())
            .collect();
        assert_eq!(names, vec!["A", "A1", "A2", "A2a", "B"]);
        assert_eq!(document.bone_subtree_len(0), Some(4));
        assert_eq!(document.bone_subtree_len(2), Some(2));
        assert_eq!(document.bone_subtree_len(4), Some(1));
        assert_eq!(document.bone_by_index(5), None);
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
}
