//! Deserialization types for the authoring app's `.json` format.
//!
//! These mirror the app's `model` module. Only the fields the compiler needs
//! are modelled; names are read here and then dropped from the binary.

use serde::Deserialize;

/// Value of the document's `type` field.
pub const FORMAT_TYPE: &str = "viveco";
/// Oldest document version this compiler understands.
pub const MIN_VERSION: u32 = 1;
/// Newest document version this compiler understands.
pub const FORMAT_VERSION: u32 = 4;

#[derive(Deserialize)]
pub struct Document {
    /// Format identifier. Must be [`FORMAT_TYPE`].
    #[serde(rename = "type")]
    pub kind: String,
    pub version: u32,
    #[serde(default)]
    pub shapes: Vec<Shape>,
    #[serde(default)]
    pub bones: Vec<Bone>,
    #[serde(default)]
    pub animations: Vec<Animation>,
}

#[derive(Deserialize)]
pub struct Shape {
    #[serde(default)]
    #[allow(dead_code)]
    pub name: String,
    #[serde(default)]
    pub z: f32,
    #[serde(default)]
    pub style: Style,
    #[serde(default)]
    pub paths: Vec<Path>,
    #[serde(default)]
    pub children: Vec<Shape>,
}

#[derive(Deserialize, Default)]
pub struct Style {
    #[serde(default)]
    pub fill: Option<String>,
    #[serde(default)]
    pub stroke: Option<StrokeStyle>,
}

#[derive(Deserialize)]
pub struct StrokeStyle {
    pub color: String,
    pub width: f32,
}

#[derive(Deserialize)]
pub struct Path {
    #[serde(default)]
    #[allow(dead_code)]
    pub name: String,
    pub ends: PathEnds,
    #[serde(default)]
    pub segments: Vec<Segment>,
    #[serde(default)]
    pub start_weights: Vec<Weight>,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PathEnds {
    Open([f32; 2]),
    Closed,
}

#[derive(Deserialize)]
pub struct Segment {
    #[serde(default)]
    pub ctrl1: Option<[f32; 2]>,
    #[serde(default)]
    pub ctrl2: Option<[f32; 2]>,
    pub to: [f32; 2],
    #[serde(default)]
    pub ctrl1_weights: Vec<Weight>,
    #[serde(default)]
    pub ctrl2_weights: Vec<Weight>,
    #[serde(default)]
    pub to_weights: Vec<Weight>,
}

#[derive(Deserialize)]
pub struct Weight {
    pub bone: String,
    pub weight: f32,
}

#[derive(Deserialize)]
pub struct Bone {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub head: Option<[f32; 2]>,
    #[serde(default)]
    pub tail: [f32; 2],
    #[serde(default)]
    pub children: Vec<Bone>,
}

#[derive(Deserialize)]
pub struct Animation {
    #[serde(default)]
    #[allow(dead_code)]
    pub name: String,
    #[serde(default)]
    pub duration: f32,
    #[serde(default)]
    pub cyclic: bool,
    #[serde(default)]
    pub keyframes: Vec<Keyframe>,
}

#[derive(Deserialize)]
pub struct Keyframe {
    pub timestamp: f32,
    pub value: f32,
    #[serde(default)]
    pub handle_in: Option<f32>,
    #[serde(default)]
    pub handle_out: Option<f32>,
    pub target: Target,
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Target {
    Bone {
        bone: String,
        property: BoneProperty,
    },
    Shape {
        #[serde(default)]
        #[allow(dead_code)]
        shape: String,
    },
}

#[derive(Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum BoneProperty {
    X,
    Y,
    Angle,
    Scale,
}

impl BoneProperty {
    /// The code stored in the binary. Must match
    /// `viveco::format::BoneProperty::from_code`.
    pub fn code(self) -> u32 {
        match self {
            Self::X => 0,
            Self::Y => 1,
            Self::Angle => 2,
            Self::Scale => 3,
        }
    }
}
