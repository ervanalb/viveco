//! Serializable vector-art document model.
//!
//! The document is a stack of [`Layer`]s ordered by a Z coordinate. Each layer
//! holds zero or more [`Item`]s: plain [`PathItem`]s or [`ClipGroup`]s. Paths are
//! built from straight [`Segment::Line`]s and cubic Bézier
//! [`Segment::Cubic`]s, may be open or closed, and may be filled and/or stroked
//! with a solid color.
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
pub const FORMAT_VERSION: u32 = 1;

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
    pub layers: Vec<Layer>,
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
            layers: Vec::new(),
        }
    }

    /// Layers in painting order (ascending Z, stable for equal Z).
    pub fn sorted_layers(&self) -> Vec<&Layer> {
        let mut layers: Vec<&Layer> = self.layers.iter().collect();
        layers.sort_by(|a, b| a.z.partial_cmp(&b.z).unwrap_or(Ordering::Equal));
        layers
    }

    /// Axis-aligned bounds of all geometry in the document.
    ///
    /// Bézier control points are included, so the result is a conservative bound.
    pub fn bounds(&self) -> Option<Bounds> {
        let mut acc = BoundsAccumulator::default();
        for layer in &self.layers {
            for item in &layer.items {
                acc.add_item(item);
            }
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
// Layers and items
// ---------------------------------------------------------------------------

/// A group of items drawn together at a given depth.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Layer {
    /// Optional human-readable name, shown in the editor's layer tree.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,

    /// Z order. Lower values are painted first (further back).
    #[serde(default)]
    pub z: f32,

    #[serde(default)]
    pub items: Vec<Item>,
}

/// Something that can be drawn inside a layer (or inside a clip group).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Item {
    Path(PathItem),
    Clip(ClipGroup),
}

/// A path plus the paint applied to it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PathItem {
    pub path: Path,
    #[serde(default)]
    pub style: Style,
}

/// A set of target items clipped to the interior of `clip`.
///
/// The clip path is itself painted: its fill is drawn beneath the targets and
/// its stroke is drawn above them. Targets may themselves be clip groups,
/// allowing nested clipping.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ClipGroup {
    pub clip: Path,
    #[serde(default)]
    pub style: Style,
    #[serde(default)]
    pub targets: Vec<Item>,
}

// ---------------------------------------------------------------------------
// Geometry
// ---------------------------------------------------------------------------

/// A single path: a start point followed by line/cubic segments.
///
/// When `closed` is set, an implicit line joins the final point back to
/// `start`. An open path is still implicitly closed when filled, matching
/// common vector-graphics behavior; use stroke-only styles when you need an
/// open outline.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Path {
    /// Optional human-readable name, shown in the editor's layer tree.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,

    pub start: Point,
    #[serde(default)]
    pub segments: Vec<Segment>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub closed: bool,
}

fn is_false(value: &bool) -> bool {
    !*value
}

impl Path {
    pub fn new(start: Point) -> Self {
        Self {
            name: String::new(),
            start,
            segments: Vec::new(),
            closed: false,
        }
    }

    /// Sets the human-readable name used by the editor's layer tree.
    pub fn named(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
    }

    pub fn line_to(mut self, to: Point) -> Self {
        self.segments.push(Segment::Line { to });
        self
    }

    /// Appends a cubic Bézier segment ending at `to`.
    pub fn cubic_to(mut self, ctrl1: Point, ctrl2: Point, to: Point) -> Self {
        self.segments.push(Segment::Cubic { ctrl1, ctrl2, to });
        self
    }

    pub fn closed(mut self) -> Self {
        self.closed = true;
        self
    }
}

/// One segment of a [`Path`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Segment {
    Line {
        to: Point,
    },
    Cubic {
        ctrl1: Point,
        ctrl2: Point,
        to: Point,
    },
}

// ---------------------------------------------------------------------------
// Paint
// ---------------------------------------------------------------------------

/// Solid fills and/or strokes applied to a path.
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
        self.add_point(path.start);
        for segment in &path.segments {
            match segment {
                Segment::Line { to } => self.add_point(*to),
                Segment::Cubic { ctrl1, ctrl2, to } => {
                    self.add_point(*ctrl1);
                    self.add_point(*ctrl2);
                    self.add_point(*to);
                }
            }
        }
    }

    fn add_item(&mut self, item: &Item) {
        match item {
            Item::Path(path_item) => self.add_path(&path_item.path),
            Item::Clip(clip) => {
                self.add_path(&clip.clip);
                for target in &clip.targets {
                    self.add_item(target);
                }
            }
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

    fn sample_document() -> Document {
        Document {
            version: FORMAT_VERSION,
            layers: vec![Layer {
                name: String::new(),
                z: 5.0,
                items: vec![Item::Path(PathItem {
                    path: Path::new([10.0, 20.0])
                        .line_to([30.0, 20.0])
                        .cubic_to([40.0, 25.0], [50.0, 35.0], [60.0, 40.0])
                        .closed(),
                    style: Style {
                        fill: Some(Rgba8::rgb(0xb3, 0xb3, 0xb3)),
                        stroke: Some(StrokeStyle {
                            color: Rgba8::rgba(0, 0, 0, 128),
                            width: 2.0,
                        }),
                    },
                })],
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
    fn layers_are_sorted_by_z() {
        let mut doc = Document::new();
        doc.layers.push(Layer {
            name: String::new(),
            z: 10.0,
            items: vec![],
        });
        doc.layers.push(Layer {
            name: String::new(),
            z: -1.0,
            items: vec![],
        });
        doc.layers.push(Layer {
            name: String::new(),
            z: 2.0,
            items: vec![],
        });

        let zs: Vec<f32> = doc.sorted_layers().iter().map(|layer| layer.z).collect();
        assert_eq!(zs, vec![-1.0, 2.0, 10.0]);
    }

    #[test]
    fn bounds_include_clip_geometry_and_controls() {
        let doc = Document {
            version: FORMAT_VERSION,
            layers: vec![Layer {
                name: String::new(),
                z: 0.0,
                items: vec![
                    Item::Path(PathItem {
                        path: Path::new([0.0, 0.0]).line_to([10.0, 10.0]),
                        style: Style::default(),
                    }),
                    Item::Clip(ClipGroup {
                        clip: Path::new([-5.0, -5.0]).line_to([20.0, 20.0]),
                        style: Style::default(),
                        targets: vec![Item::Path(PathItem {
                            path: Path::new([100.0, 100.0]).cubic_to(
                                [110.0, 120.0],
                                [130.0, 140.0],
                                [150.0, 150.0],
                            ),
                            style: Style::default(),
                        })],
                    }),
                ],
            }],
        };

        let bounds = doc.bounds().unwrap();
        assert_eq!(bounds.min, [-5.0, -5.0]);
        assert_eq!(bounds.max, [150.0, 150.0]);
    }

    #[test]
    fn example_asset_loads_and_roundtrips() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/assets/example.vecart.json");
        let doc = Document::load_from_file(path).unwrap();
        let json = doc.to_json_string().unwrap();
        let decoded = Document::from_json_str(&json).unwrap();
        assert_eq!(doc, decoded);
    }
}
