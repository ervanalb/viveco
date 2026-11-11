//! Shapes, their geometry, styling, and vertex weights.
//!
//! A shape is a stack of [`Shape`]s ordered by a Z coordinate. A shape owns
//! zero or more [`Path`]s, which are filled together as a single compound path
//! (so holes can be made by giving inner contours the opposite winding), and
//! zero or more child shapes. Children are clipped to the interior of their
//! parent and are painted above the parent's fill but beneath its stroke.
//!
//! Z only orders shapes among their siblings. Moving a shape therefore carries
//! its entire subtree with it and never disturbs the parent/child clipping
//! relationship.
//!
//! Coordinates are plain asset units with the origin at the top-left and Y
//! pointing down (the usual vector-graphics convention).

use std::cmp::Ordering;
use std::fmt;

use serde::de;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::asset::Asset;
use crate::bone::BoneIndex;

/// A 2D point/vector in asset space.
pub type Point = [f32; 2];

/// Flat index of a shape, in depth-first pre-order.
///
/// Works like [`BoneIndex`] over [`Asset::shapes`].
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
    #[serde(default)]
    pub name: String,

    /// Z order among siblings. Lower values are painted first (further back).
    #[serde(default)]
    pub z: f32,

    #[serde(default)]
    pub style: Style,

    /// Contours combined into this shape's compound path.
    #[serde(default)]
    pub paths: Vec<Path>,

    /// Child shapes, clipped to this shape's interior.
    #[serde(default)]
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
    pub(crate) fn for_each_weights_mut(&mut self, f: &mut impl FnMut(&mut Weights)) {
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

impl Asset {
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

    /// The number of shapes in the asset, counting descendants.
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

    /// The flat pre-order index of the shape at `chain`.
    ///
    /// `chain` is a path of sibling indices from the root, like the one the
    /// editor's tree addresses use: `[i]` names `shapes[i]`, `[i, j]` names
    /// its `children[j]`, and so on.
    pub fn shape_index_of_chain(&self, chain: &[usize]) -> Option<ShapeIndex> {
        fn find(shapes: &[Shape], chain: &[usize], base: ShapeIndex) -> Option<ShapeIndex> {
            let (&index, rest) = chain.split_first()?;
            let mut flat = base;
            for (position, shape) in shapes.iter().enumerate() {
                if position == index {
                    return if rest.is_empty() {
                        Some(flat)
                    } else {
                        find(&shape.children, rest, flat + 1)
                    };
                }
                flat += shape.subtree_len();
            }
            None
        }

        find(&self.shapes, chain, 0)
    }

    /// The pre-order index of the first shape named `name`.
    ///
    /// Names are descriptive and need not be unique, so this returns the first
    /// match in the same depth-first order as [`Self::shape_by_index`].
    pub fn shape_index_by_name(&self, name: &str) -> Option<ShapeIndex> {
        fn find(shapes: &[Shape], name: &str, cursor: &mut usize) -> Option<ShapeIndex> {
            for shape in shapes {
                let current = *cursor;
                *cursor += 1;
                if shape.name == name {
                    return Some(current);
                }
                if let Some(found) = find(&shape.children, name, cursor) {
                    return Some(found);
                }
            }
            None
        }

        let mut cursor = 0;
        find(&self.shapes, name, &mut cursor)
    }
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
    #[serde(default)]
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
    #[serde(default)]
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
    pub(crate) fn for_each_weights_mut(&mut self, f: &mut impl FnMut(&mut Weights)) {
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
    #[serde(default)]
    pub ctrl1: Option<Point>,
    #[serde(default)]
    pub ctrl2: Option<Point>,
    pub to: Point,
    /// Bone weights of the control near the segment's start.
    #[serde(default)]
    pub ctrl1_weights: Weights,
    /// Bone weights of the control near the segment's end.
    #[serde(default)]
    pub ctrl2_weights: Weights,
    /// Bone weights of the anchor at the segment's end.
    #[serde(default)]
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

/// Solid fills and/or strokes applied to a shape.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Style {
    #[serde(default)]
    pub fill: Option<Rgba8>,
    #[serde(default)]
    pub stroke: Option<StrokeStyle>,
}

/// A solid-color stroke.
///
/// The width is not part of the model: the editor paints every stroke as a
/// one-pixel outline regardless of zoom.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct StrokeStyle {
    pub color: Rgba8,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bone::{insertion_map, removal_map};

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
        let asset = Asset {
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

        let zs: Vec<f32> = asset.sorted_shapes().iter().map(|shape| shape.z).collect();
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
        let asset = Asset {
            bones: Vec::new(),
            animations: Vec::new(),
            shapes: vec![Shape {
                paths: vec![path],
                ..Default::default()
            }],
        };

        let json = asset.to_json_string().unwrap();
        let decoded = Asset::from_json_str(&json).unwrap();
        assert_eq!(asset, decoded);
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

    #[test]
    fn shapes_are_found_by_name_in_pre_order() {
        let asset = Asset {
            shapes: vec![
                Shape {
                    name: "Outer".into(),
                    children: vec![Shape {
                        name: "Inner".into(),
                        ..Default::default()
                    }],
                    ..Default::default()
                },
                Shape {
                    name: "Sibling".into(),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };

        assert_eq!(asset.shape_index_by_name("Outer"), Some(0));
        assert_eq!(asset.shape_index_by_name("Inner"), Some(1));
        assert_eq!(asset.shape_index_by_name("Sibling"), Some(2));
        assert_eq!(asset.shape_index_by_name("Missing"), None);

        // Chains resolve to the same flat pre-order indices.
        assert_eq!(asset.shape_index_of_chain(&[0]), Some(0));
        assert_eq!(asset.shape_index_of_chain(&[0, 0]), Some(1));
        assert_eq!(asset.shape_index_of_chain(&[1]), Some(2));
        assert_eq!(asset.shape_index_of_chain(&[2]), None);
        assert_eq!(asset.shape_index_of_chain(&[0, 1]), None);
        assert_eq!(asset.shape_index_of_chain(&[]), None);
    }
}
