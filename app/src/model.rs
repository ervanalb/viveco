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
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Version written to and expected by this build of the format.
pub const FORMAT_VERSION: u32 = 2;

/// A 2D point/vector in document space.
pub type Point = [f32; 2];

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
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Document {
    /// Format version; currently [`FORMAT_VERSION`].
    #[serde(default = "default_version")]
    pub version: u32,

    #[serde(default)]
    pub shapes: Vec<Shape>,

    /// Top-level bones of the rig, in tree order. Empty for an unrigged
    /// document.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub bones: Vec<Bone>,
}

fn default_version() -> u32 {
    FORMAT_VERSION
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
        if let Some(start) = self.start()
            && !self
                .segments
                .last()
                .is_some_and(|segment| segment.to == start)
        {
            self.segments.push(Segment::line(start));
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
                true
            }
        }
    }
}

/// One segment of a [`Path`], ending at `to`.
///
/// `ctrl1` is the Bézier control point near the segment's start and `ctrl2`
/// the one near `to`. An omitted control is treated as lying exactly on its
/// endpoint, so it has no visible effect and the editor draws no handle for
/// it. A segment with no controls is a straight line.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Segment {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ctrl1: Option<Point>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ctrl2: Option<Point>,
    pub to: Point,
}

impl Segment {
    /// A straight segment: both control points are omitted.
    pub fn line(to: Point) -> Self {
        Self {
            ctrl1: None,
            ctrl2: None,
            to,
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
}
