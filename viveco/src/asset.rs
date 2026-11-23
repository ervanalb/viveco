//! The root of a vector-art asset and its file encodings.

use std::io;
use std::path::Path as FsPath;

use serde::de;
use serde::ser::SerializeStruct;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::animation::{Animation, KeyframeTarget};
use crate::bone::{Bone, BoneIndex};
use crate::pose::Similarity;
use crate::shape::{Path, PathEnds, Point, Shape, ShapeIndex, Weights};

/// Identifier written to and expected in an asset's `type` field.
pub const FORMAT_TYPE: &str = "viveco";

/// Version written to and expected by this build of the format.
///
/// Only this exact version is accepted; older files must be converted before
/// they can be loaded.
pub const FORMAT_VERSION: u32 = 0;

/// Bytes every binary asset begins with.
///
/// The leading `V` is neither whitespace nor `{`, so JSON contents can never
/// be mistaken for a binary asset.
const BINARY_MAGIC: &[u8] = b"VIVECOB1";

/// An error decoding an asset from the binary format.
#[derive(Debug)]
pub enum BinaryDecodeError {
    /// The bytes do not begin with the binary magic.
    MissingMagic,
    /// The payload is not a valid postcard encoding of an asset.
    Postcard(postcard::Error),
}

impl std::fmt::Display for BinaryDecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingMagic => f.write_str("missing viveco binary magic"),
            Self::Postcard(err) => write!(f, "invalid viveco binary: {err}"),
        }
    }
}

impl std::error::Error for BinaryDecodeError {}

impl From<postcard::Error> for BinaryDecodeError {
    fn from(err: postcard::Error) -> Self {
        Self::Postcard(err)
    }
}

/// An error loading an asset from its bytes or from a file.
#[derive(Debug)]
pub enum AssetLoadError {
    /// Reading the file failed.
    ///
    /// Only [`Asset::load_from_file`] produces this variant;
    /// [`Asset::from_bytes`] cannot.
    Io(io::Error),
    /// The contents were not valid UTF-8 while being parsed as JSON.
    Utf8(std::str::Utf8Error),
    /// The JSON document was invalid.
    Json(serde_json::Error),
    /// The binary document was invalid.
    Binary(BinaryDecodeError),
}

impl std::fmt::Display for AssetLoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(err) => write!(f, "could not read viveco asset: {err}"),
            Self::Utf8(err) => write!(f, "viveco JSON is not UTF-8: {err}"),
            Self::Json(err) => write!(f, "invalid viveco JSON: {err}"),
            // `BinaryDecodeError` already describes the failure.
            Self::Binary(err) => write!(f, "{err}"),
        }
    }
}

impl std::error::Error for AssetLoadError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(err) => Some(err),
            Self::Utf8(err) => Some(err),
            Self::Json(err) => Some(err),
            Self::Binary(err) => Some(err),
        }
    }
}

impl From<io::Error> for AssetLoadError {
    fn from(err: io::Error) -> Self {
        Self::Io(err)
    }
}

impl From<std::str::Utf8Error> for AssetLoadError {
    fn from(err: std::str::Utf8Error) -> Self {
        Self::Utf8(err)
    }
}

impl From<serde_json::Error> for AssetLoadError {
    fn from(err: serde_json::Error) -> Self {
        Self::Json(err)
    }
}

impl From<BinaryDecodeError> for AssetLoadError {
    fn from(err: BinaryDecodeError) -> Self {
        Self::Binary(err)
    }
}

/// The root of a vector-art file.
#[derive(Clone, Debug, PartialEq)]
pub struct Asset {
    pub shapes: Vec<Shape>,

    /// Top-level bones of the rig, in tree order. Empty for an unrigged
    /// asset.
    ///
    /// Tree order is topological: the flat [`BoneIndex`] is depth-first
    /// pre-order, so every parent precedes its descendants and each subtree
    /// occupies a contiguous index range. Skinning relies on this to resolve
    /// world-space bone transforms in one linear pass.
    pub bones: Vec<Bone>,

    /// Animations stored in the asset.
    pub animations: Vec<Animation>,
}

impl Serialize for Asset {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        // Every field is always written, in a fixed order. JSON sees named
        // keys; postcard sees the same values positionally. Skipping empty
        // fields would desynchronize postcard's positional decoding.
        let mut state = serializer.serialize_struct("Asset", 5)?;
        state.serialize_field("type", FORMAT_TYPE)?;
        state.serialize_field("version", &FORMAT_VERSION)?;
        state.serialize_field("shapes", &self.shapes)?;
        state.serialize_field("bones", &self.bones)?;
        state.serialize_field("animations", &self.animations)?;
        state.end()
    }
}

impl<'de> Deserialize<'de> for Asset {
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
        if wire.version != FORMAT_VERSION {
            return Err(de::Error::custom(format!(
                "unsupported viveco version {} (expected {FORMAT_VERSION})",
                wire.version
            )));
        }

        Ok(Self {
            shapes: wire.shapes,
            bones: wire.bones,
            animations: wire.animations,
        })
    }
}

impl Default for Asset {
    fn default() -> Self {
        Self::new()
    }
}

impl Asset {
    pub fn new() -> Self {
        Self {
            shapes: Vec::new(),
            bones: Vec::new(),
            animations: Vec::new(),
        }
    }

    pub fn from_json_str(text: &str) -> serde_json::Result<Self> {
        serde_json::from_str(text)
    }

    pub fn to_json_string(&self) -> serde_json::Result<String> {
        serde_json::to_string_pretty(self)
    }

    /// Encodes the asset in the binary format, including the leading magic.
    pub fn to_postcard_vec(&self) -> Result<Vec<u8>, postcard::Error> {
        let mut bytes = Vec::from(BINARY_MAGIC);
        bytes.extend_from_slice(&postcard::to_allocvec(self)?);
        Ok(bytes)
    }

    /// Decodes an asset from the binary format.
    pub fn from_postcard_slice(bytes: &[u8]) -> Result<Self, BinaryDecodeError> {
        let Some(payload) = bytes.strip_prefix(BINARY_MAGIC) else {
            return Err(BinaryDecodeError::MissingMagic);
        };
        Ok(postcard::from_bytes(payload)?)
    }

    /// Decodes an asset from file contents, detecting the encoding.
    ///
    /// Contents beginning with the binary magic are decoded with postcard;
    /// anything else is parsed as UTF-8 JSON. This is the encoding detection
    /// used by [`Asset::load_from_file`] and the Bevy asset loader.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, AssetLoadError> {
        match Self::from_postcard_slice(bytes) {
            Ok(asset) => Ok(asset),
            // Anything without the magic is treated as JSON, so its contents
            // are parsed as text.
            Err(BinaryDecodeError::MissingMagic) => {
                Ok(Self::from_json_str(std::str::from_utf8(bytes)?)?)
            }
            Err(err) => Err(err.into()),
        }
    }

    /// Reads an asset, choosing the encoding from the file's contents.
    ///
    /// The contents are decoded by [`Asset::from_bytes`].
    pub fn load_from_file(path: impl AsRef<FsPath>) -> Result<Self, AssetLoadError> {
        Self::from_bytes(&std::fs::read(path)?)
    }

    /// Writes the asset, choosing the encoding from the path's extension.
    ///
    /// A `.json` extension (any case) writes JSON; every other extension
    /// writes the postcard binary format.
    pub fn save_to_file(&self, path: impl AsRef<FsPath>) -> io::Result<()> {
        let path = path.as_ref();
        let json = path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| extension.eq_ignore_ascii_case("json"));
        let bytes = if json {
            self.to_json_string()
                .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?
                .into_bytes()
        } else {
            self.to_postcard_vec()
                .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?
        };
        std::fs::write(path, bytes)
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

    /// Uniformly scales every length in the asset by `factor`, about the
    /// origin.
    ///
    /// This is [`Self::apply_transform`] with a rotation of zero, kept as a
    /// convenience. See that method for exactly what is and is not a length.
    /// A factor of one leaves the asset unchanged.
    ///
    /// The caller is responsible for passing a positive, finite factor.
    pub fn scale_uniform(&mut self, factor: f32) {
        self.apply_transform(Similarity {
            scale: factor,
            rotation: 0.0,
            translation: [0.0, 0.0],
        });
    }

    /// Applies a uniform scale, rotation, and translation to the whole asset.
    ///
    /// Every shape point—an open path's start, each segment's two controls, and
    /// its end point—is transformed, as is every bone's stored head and tail;
    /// omitted heads stay omitted. Because rotations and scales are uniform,
    /// the asset keeps its proportions.
    ///
    /// Bone translation curves (X and Y) are transformed too, since they are
    /// offsets in asset space. A pure scale multiplies them channel by channel.
    /// A rotation mixes the two channels: both curves are split at the union of
    /// their keyframe times and rewritten as rotated vectors, so sampling the
    /// transformed animation reproduces the rotated motion exactly.
    ///
    /// Values that are not lengths are left untouched: shape and animation Z
    /// (paint order), bone rotation and scale curves, animation timings, and
    /// bone weights. The translation part of `transform` moves the artwork and
    /// the skeleton but not the animation offsets, which are relative.
    ///
    /// The caller is responsible for passing a positive, finite scale.
    pub fn apply_transform(&mut self, transform: Similarity) {
        if transform == Similarity::IDENTITY {
            return;
        }
        for shape in &mut self.shapes {
            transform_shape(shape, transform);
        }
        for bone in &mut self.bones {
            transform_bone(bone, transform);
        }
        for animation in &mut self.animations {
            animation.transform_translations(transform.scale, transform.rotation);
        }
    }

    /// The axis-aligned bounds of all artwork in the asset, including nested
    /// children.
    ///
    /// Bézier control points are included, so the result is a conservative
    /// bound rather than the exact extent of the curves. Bones are not artwork
    /// and are not included. Returns `None` when the asset has no geometry.
    pub fn bounds(&self) -> Option<Bounds> {
        let mut accumulator = BoundsAccumulator::default();
        for shape in &self.shapes {
            accumulator.add_shape(shape);
        }
        accumulator.finish()
    }
}

/// An axis-aligned rectangle in asset space.
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

/// Accumulates the bounds of every point that affects painted geometry.
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

/// Transforms every point of `shape` and its descendants.
fn transform_shape(shape: &mut Shape, transform: Similarity) {
    for path in &mut shape.paths {
        transform_path(path, transform);
    }
    for child in &mut shape.children {
        transform_shape(child, transform);
    }
}

/// Transforms a path's start, controls, and segment ends.
fn transform_path(path: &mut Path, transform: Similarity) {
    if let PathEnds::Open(start) = &mut path.ends {
        *start = transform.apply(*start);
    }
    for segment in &mut path.segments {
        if let Some(ctrl1) = &mut segment.ctrl1 {
            *ctrl1 = transform.apply(*ctrl1);
        }
        if let Some(ctrl2) = &mut segment.ctrl2 {
            *ctrl2 = transform.apply(*ctrl2);
        }
        segment.to = transform.apply(segment.to);
    }
}

/// Transforms a bone's endpoints and every bone below it.
fn transform_bone(bone: &mut Bone, transform: Similarity) {
    if let Some(head) = &mut bone.head {
        *head = transform.apply(*head);
    }
    bone.tail = transform.apply(bone.tail);
    for child in &mut bone.children {
        transform_bone(child, transform);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bone::insertion_map;
    use crate::shape::{Path, Rgba8, StrokeStyle, Style};

    fn sample_path() -> Path {
        Path::new([10.0, 20.0])
            .line_to([30.0, 20.0])
            .cubic_to([40.0, 25.0], [50.0, 35.0], [60.0, 40.0])
            .closed()
    }

    fn sample_asset() -> Asset {
        Asset {
            bones: Vec::new(),
            animations: Vec::new(),
            shapes: vec![Shape {
                name: "Outer".to_string(),
                z: 5.0,
                style: Style {
                    fill: Some(Rgba8::rgb(0xb3, 0xb3, 0xb3)),
                    stroke: Some(StrokeStyle {
                        color: Rgba8::rgba(0, 0, 0, 128),
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
        let asset = sample_asset();
        let json = asset.to_json_string().unwrap();
        let decoded = Asset::from_json_str(&json).unwrap();
        assert_eq!(asset, decoded);
    }

    #[test]
    fn serialized_assets_carry_the_current_version() {
        let json = Asset::new().to_json_string().unwrap();
        assert!(json.contains(&format!("\"version\": {FORMAT_VERSION}")));
        assert!(json.contains(&format!("\"type\": {FORMAT_TYPE:?}")));
    }

    #[test]
    fn binary_roundtrip_preserves_the_asset() {
        use crate::animation::{Animation, BoneProperty, Keyframe, KeyframeTarget};

        let mut asset = sample_asset();
        let mut keyframe = Keyframe::new(
            0.5,
            2.0,
            KeyframeTarget::Bone {
                bone: 0,
                property: BoneProperty::Angle,
            },
        );
        keyframe.handle_in = Some(0.25);
        keyframe.handle_out = Some(0.75);
        let mut animation = Animation::new(1.0);
        animation.insert_keyframe(keyframe);
        asset.animations.push(animation);

        let bytes = asset.to_postcard_vec().unwrap();
        assert!(bytes.starts_with(BINARY_MAGIC));
        assert_eq!(Asset::from_postcard_slice(&bytes).unwrap(), asset);
    }

    #[test]
    fn json_is_not_decoded_as_binary() {
        let json = sample_asset().to_json_string().unwrap();
        assert!(!json.as_bytes().starts_with(BINARY_MAGIC));
        assert!(matches!(
            Asset::from_postcard_slice(json.as_bytes()),
            Err(BinaryDecodeError::MissingMagic)
        ));
    }

    #[test]
    fn from_bytes_detects_the_encoding() {
        let asset = sample_asset();

        let json = asset.to_json_string().unwrap();
        assert_eq!(Asset::from_bytes(json.as_bytes()).unwrap(), asset);

        let binary = asset.to_postcard_vec().unwrap();
        assert_eq!(Asset::from_bytes(&binary).unwrap(), asset);
    }

    #[test]
    fn from_bytes_reports_the_decoding_failure() {
        assert!(matches!(
            Asset::from_bytes(&[0xff, 0xfe]),
            Err(AssetLoadError::Utf8(_))
        ));
        assert!(matches!(
            Asset::from_bytes(b"not an asset"),
            Err(AssetLoadError::Json(_))
        ));

        let mut binary = sample_asset().to_postcard_vec().unwrap();
        binary.truncate(binary.len() - 1);
        assert!(matches!(
            Asset::from_bytes(&binary),
            Err(AssetLoadError::Binary(BinaryDecodeError::Postcard(_)))
        ));
    }

    #[test]
    fn truncated_binary_is_rejected() {
        let mut bytes = sample_asset().to_postcard_vec().unwrap();
        bytes.truncate(bytes.len() - 1);
        assert!(matches!(
            Asset::from_postcard_slice(&bytes),
            Err(BinaryDecodeError::Postcard(_))
        ));
    }

    #[test]
    fn file_extension_selects_the_encoding() {
        let asset = sample_asset();
        let dir = std::env::temp_dir();
        let tag = std::process::id();

        let json_path = dir.join(format!("viveco_asset_test_{tag}.json"));
        asset.save_to_file(&json_path).unwrap();
        let written = std::fs::read(&json_path).unwrap();
        assert!(!written.starts_with(BINARY_MAGIC));
        assert_eq!(Asset::load_from_file(&json_path).unwrap(), asset);
        std::fs::remove_file(&json_path).unwrap();

        let binary_path = dir.join(format!("viveco_asset_test_{tag}.viveco"));
        asset.save_to_file(&binary_path).unwrap();
        let written = std::fs::read(&binary_path).unwrap();
        assert!(written.starts_with(BINARY_MAGIC));
        assert_eq!(Asset::load_from_file(&binary_path).unwrap(), asset);
        std::fs::remove_file(&binary_path).unwrap();
    }

    #[test]
    fn applying_a_bone_map_updates_animation_targets() {
        use crate::animation::{Animation, BoneProperty, Keyframe, KeyframeTarget};

        let mut animation = Animation::new(1.0);
        animation.insert_keyframe(Keyframe::new(
            0.0,
            0.0,
            KeyframeTarget::Bone {
                bone: 0,
                property: BoneProperty::X,
            },
        ));
        animation.insert_keyframe(Keyframe::new(
            0.5,
            0.0,
            KeyframeTarget::Bone {
                bone: 1,
                property: BoneProperty::X,
            },
        ));
        let mut asset = Asset {
            animations: vec![animation],
            ..Default::default()
        };

        // Insert a bone at index 0, pushing both targets along.
        asset.apply_bone_map(&insertion_map(2, 0, 1));

        let bones: Vec<BoneIndex> = asset.animations[0]
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
        use crate::animation::{Animation, BoneProperty, Keyframe, KeyframeTarget};
        use crate::bone::removal_map;

        let mut animation = Animation::new(1.0);
        animation.insert_keyframe(Keyframe::new(
            0.0,
            0.0,
            KeyframeTarget::Bone {
                bone: 0,
                property: BoneProperty::X,
            },
        ));
        animation.insert_keyframe(Keyframe::new(
            0.5,
            0.0,
            KeyframeTarget::Bone {
                bone: 1,
                property: BoneProperty::X,
            },
        ));
        let mut asset = Asset {
            animations: vec![animation],
            ..Default::default()
        };

        asset.apply_bone_map(&removal_map(2, 0, 1));

        assert_eq!(asset.animations[0].keyframes.len(), 1);
        assert!(matches!(
            asset.animations[0].keyframes[0].target,
            KeyframeTarget::Bone { bone: 0, .. }
        ));
    }

    #[test]
    fn shape_z_keyframes_are_supported() {
        use crate::animation::{Animation, Keyframe, KeyframeTarget, ShapeProperty};

        let json = r#"{
            "type": "viveco",
            "version": 0,
            "shapes": [{}],
            "animations": [{
                "duration": 1.0,
                "keyframes": [
                    {"timestamp": 0.0, "value": 3.0,
                     "target": {"shape": {"shape": 0, "property": "z"}}}
                ]
            }]
        }"#;
        let asset = Asset::from_json_str(json).unwrap();
        assert_eq!(asset.animations[0].keyframes.len(), 1);
        assert_eq!(asset.sample_shape_pose(&[(0, 0.0, 1.0)]).get(0), 3.0);

        // Unknown shape properties are still rejected.
        let json = json.replace("\"z\"", "\"scale\"");
        assert!(Asset::from_json_str(&json).is_err());

        // Shape targets survive a binary round trip.
        let mut animation = Animation::new(1.0);
        animation.insert_keyframe(Keyframe::new(
            0.0,
            2.0,
            KeyframeTarget::Shape {
                shape: 0,
                property: ShapeProperty::Z,
            },
        ));
        let asset = Asset {
            shapes: vec![Shape::default()],
            animations: vec![animation],
            ..Default::default()
        };
        let bytes = asset.to_postcard_vec().unwrap();
        assert_eq!(Asset::from_postcard_slice(&bytes).unwrap(), asset);
    }

    #[test]
    fn applying_a_shape_map_updates_animation_targets() {
        use crate::animation::{Animation, Keyframe, KeyframeTarget, ShapeProperty};
        use crate::bone::move_map;

        let mut animation = Animation::new(1.0);
        for shape in [0, 2] {
            animation.insert_keyframe(Keyframe::new(
                shape as f32 * 0.1,
                0.0,
                KeyframeTarget::Shape {
                    shape,
                    property: ShapeProperty::Z,
                },
            ));
        }
        let mut asset = Asset {
            animations: vec![animation],
            ..Default::default()
        };

        // Move flat shape 0 to index 2: everything shifts left, then the block
        // lands at the end.
        asset.apply_shape_map(&move_map(4, 0, 1, 2));

        let shapes: Vec<_> = asset.animations[0]
            .keyframes
            .iter()
            .map(|keyframe| match &keyframe.target {
                KeyframeTarget::Shape { shape, .. } => *shape,
                KeyframeTarget::Bone { .. } => unreachable!(),
            })
            .collect();
        assert_eq!(shapes, vec![2, 1]);
    }

    #[test]
    fn uniform_scaling_scales_geometry_bones_and_translations() {
        use crate::animation::{Animation, BoneProperty, Keyframe, KeyframeTarget, ShapeProperty};
        use crate::shape::PathEnds;

        let mut path = Path::new([1.0, 2.0]).cubic_to([3.0, 4.0], [5.0, 6.0], [7.0, 8.0]);
        path.segments[0].to_weights.set(0, 1.0);
        let mut animation = Animation::new(1.0);
        for (timestamp, value, target) in [
            (
                0.0,
                5.0,
                KeyframeTarget::Bone {
                    bone: 0,
                    property: BoneProperty::X,
                },
            ),
            (
                0.1,
                -2.0,
                KeyframeTarget::Bone {
                    bone: 0,
                    property: BoneProperty::Y,
                },
            ),
            (
                0.2,
                1.5,
                KeyframeTarget::Bone {
                    bone: 0,
                    property: BoneProperty::Angle,
                },
            ),
            (
                0.3,
                2.0,
                KeyframeTarget::Bone {
                    bone: 0,
                    property: BoneProperty::Scale,
                },
            ),
            (
                0.4,
                4.0,
                KeyframeTarget::Shape {
                    shape: 0,
                    property: ShapeProperty::Z,
                },
            ),
        ] {
            let mut keyframe = Keyframe::new(timestamp, value, target);
            keyframe.handle_in = Some(value / 2.0);
            keyframe.handle_out = Some(value * 2.0);
            animation.insert_keyframe(keyframe);
        }

        let mut asset = Asset {
            shapes: vec![Shape {
                z: 5.0,
                paths: vec![path],
                children: vec![Shape {
                    z: -3.0,
                    paths: vec![Path::new([9.0, 10.0]).line_to([11.0, 12.0])],
                    ..Default::default()
                }],
                ..Default::default()
            }],
            bones: vec![Bone {
                head: Some([1.0, 1.0]),
                tail: [2.0, 3.0],
                children: vec![Bone {
                    // A connected child keeps its omitted head.
                    head: None,
                    tail: [4.0, 6.0],
                    ..Default::default()
                }],
                ..Default::default()
            }],
            animations: vec![animation],
        };

        asset.scale_uniform(2.0);

        let shape = &asset.shapes[0];
        assert_eq!(shape.z, 5.0, "paint order is not a length");
        assert_eq!(
            shape.paths[0].ends,
            PathEnds::Open([2.0, 4.0]),
            "an open path's start scales"
        );
        assert_eq!(shape.paths[0].segments[0].ctrl1, Some([6.0, 8.0]));
        assert_eq!(shape.paths[0].segments[0].ctrl2, Some([10.0, 12.0]));
        assert_eq!(shape.paths[0].segments[0].to, [14.0, 16.0]);

        let child = &shape.children[0];
        assert_eq!(child.z, -3.0);
        assert_eq!(child.paths[0].start(), Some([18.0, 20.0]));
        assert_eq!(child.paths[0].segments[0].to, [22.0, 24.0]);

        let parent = &asset.bones[0];
        assert_eq!(parent.head, Some([2.0, 2.0]));
        assert_eq!(parent.tail, [4.0, 6.0]);
        assert_eq!(parent.children[0].head, None);
        assert_eq!(parent.children[0].tail, [8.0, 12.0]);

        let keyframes = &asset.animations[0].keyframes;
        // X and Y are lengths, so their values and handles scale.
        assert_eq!(keyframes[0].value, 10.0);
        assert_eq!(keyframes[0].handle_in, Some(5.0));
        assert_eq!(keyframes[0].handle_out, Some(20.0));
        assert_eq!(keyframes[1].value, -4.0);
        // Rotation, scale, and Z are dimensionless or paint order.
        for index in 2..5 {
            assert_eq!(keyframes[index].value, [1.5, 2.0, 4.0][index - 2]);
            assert_eq!(
                keyframes[index].handle_in,
                Some([1.5, 2.0, 4.0][index - 2] / 2.0)
            );
            assert_eq!(
                keyframes[index].handle_out,
                Some([1.5, 2.0, 4.0][index - 2] * 2.0)
            );
        }
    }

    #[test]
    fn bounds_include_nested_shapes_and_bezier_controls() {
        let asset = Asset {
            shapes: vec![Shape {
                paths: vec![Path::new([10.0, 20.0]).cubic_to(
                    [0.0, 0.0],
                    [40.0, 50.0],
                    [30.0, 30.0],
                )],
                children: vec![Shape {
                    paths: vec![Path::new([-5.0, 25.0]).line_to([15.0, 60.0])],
                    ..Default::default()
                }],
                ..Default::default()
            }],
            ..Default::default()
        };

        let bounds = asset.bounds().unwrap();
        assert_eq!(bounds.min, [-5.0, 0.0]);
        assert_eq!(bounds.max, [40.0, 60.0]);
        assert_eq!(bounds.width(), 45.0);
        assert_eq!(bounds.height(), 60.0);
        assert_eq!(bounds.center(), [17.5, 30.0]);

        assert_eq!(Asset::new().bounds(), None);
    }

    #[test]
    fn transform_applies_a_similarity_to_artwork_and_bones() {
        use crate::bone::Bone;
        use std::f32::consts::FRAC_PI_2;

        let mut asset = Asset {
            shapes: vec![Shape {
                paths: vec![Path::new([1.0, 0.0]).line_to([0.0, 1.0])],
                children: vec![Shape {
                    paths: vec![Path::new([2.0, 0.0])],
                    ..Default::default()
                }],
                ..Default::default()
            }],
            bones: vec![Bone {
                head: Some([2.0, 0.0]),
                tail: [0.0, 2.0],
                children: vec![Bone {
                    head: None,
                    tail: [0.0, 3.0],
                    ..Default::default()
                }],
                ..Default::default()
            }],
            ..Default::default()
        };

        // A quarter turn, doubled, then shifted by [5, 6].
        asset.apply_transform(Similarity {
            scale: 2.0,
            rotation: FRAC_PI_2,
            translation: [5.0, 6.0],
        });

        let near = |actual: Point, expected: Point| {
            assert!(
                (actual[0] - expected[0]).abs() < 1e-5 && (actual[1] - expected[1]).abs() < 1e-5,
                "expected {expected:?}, got {actual:?}"
            );
        };

        let shape = &asset.shapes[0];
        near(shape.paths[0].start().unwrap(), [5.0, 8.0]);
        near(shape.paths[0].segments[0].to, [3.0, 6.0]);
        near(shape.children[0].paths[0].start().unwrap(), [5.0, 10.0]);

        let parent = &asset.bones[0];
        near(parent.head.unwrap(), [5.0, 10.0]);
        near(parent.tail, [1.0, 6.0]);
        assert_eq!(parent.children[0].head, None);
        near(parent.children[0].tail, [-1.0, 6.0]);
    }

    /// An asset whose bone has X and Y curves with different keyframe times,
    /// so rotating it forces the curves to be resplit.
    fn animated_translation_asset(cyclic: bool) -> Asset {
        use crate::animation::{Animation, BoneProperty, Keyframe, KeyframeTarget};

        let mut animation = Animation::new(4.0);
        animation.cyclic = cyclic;
        for (timestamp, value, property) in [
            (0.0, 1.0, BoneProperty::X),
            (2.0, 3.0, BoneProperty::X),
            (1.0, -2.0, BoneProperty::Y),
            (3.0, 4.0, BoneProperty::Y),
        ] {
            let mut keyframe =
                Keyframe::new(timestamp, value, KeyframeTarget::Bone { bone: 0, property });
            keyframe.handle_in = Some(value * 0.5);
            keyframe.handle_out = Some(value * 1.5);
            animation.insert_keyframe(keyframe);
        }

        Asset {
            bones: vec![Bone {
                head: Some([0.0, 0.0]),
                tail: [10.0, 0.0],
                ..Default::default()
            }],
            animations: vec![animation],
            ..Default::default()
        }
    }

    #[test]
    fn rotating_translation_curves_matches_rotated_samples() {
        use std::f32::consts::FRAC_PI_2;

        for cyclic in [false, true] {
            let mut asset = animated_translation_asset(cyclic);
            let times = [0.0, 0.5, 1.0, 1.5, 2.0, 2.5, 3.0, 3.5, 4.0];
            let before: Vec<[f32; 2]> = times
                .iter()
                .map(|&time| {
                    asset.animations[0]
                        .sample_pose(&asset, time)
                        .get(0)
                        .translation
                })
                .collect();

            let scale = 2.0;
            let rotation = FRAC_PI_2 / 2.0;
            asset.apply_transform(Similarity {
                scale,
                rotation,
                translation: [100.0, -50.0],
            });

            let (sin, cos) = rotation.sin_cos();
            for (&time, before) in times.iter().zip(before) {
                let after = asset.animations[0]
                    .sample_pose(&asset, time)
                    .get(0)
                    .translation;
                let expected = [
                    scale * (cos * before[0] - sin * before[1]),
                    scale * (sin * before[0] + cos * before[1]),
                ];
                assert!(
                    (after[0] - expected[0]).abs() < 1e-4 && (after[1] - expected[1]).abs() < 1e-4,
                    "cyclic={cyclic} t={time}: expected {expected:?}, got {after:?}"
                );
            }
        }
    }

    #[test]
    fn rotating_an_unpaired_translation_channel_creates_the_other_curve() {
        use crate::animation::{Animation, BoneProperty, Keyframe, KeyframeTarget};
        use crate::bone::Bone;
        use std::f32::consts::FRAC_PI_2;

        let mut animation = Animation::new(3.0);
        for (timestamp, value) in [(0.0, 1.0), (2.0, 3.0)] {
            animation.insert_keyframe(Keyframe::new(
                timestamp,
                value,
                KeyframeTarget::Bone {
                    bone: 0,
                    property: BoneProperty::X,
                },
            ));
        }
        let mut asset = Asset {
            bones: vec![Bone {
                head: Some([0.0, 0.0]),
                tail: [10.0, 0.0],
                ..Default::default()
            }],
            animations: vec![animation],
            ..Default::default()
        };

        asset.apply_transform(Similarity {
            scale: 1.0,
            rotation: FRAC_PI_2,
            translation: [0.0, 0.0],
        });

        // The X offset now points down, so it has become a Y curve.
        for time in [0.0, 0.5, 1.0, 1.5, 2.0] {
            let pose = asset.animations[0].sample_pose(&asset, time);
            let x = pose.get(0).translation[0];
            let y = pose.get(0).translation[1];
            assert!(x.abs() < 1e-5, "t={time}: x should be zero, got {x}");
            let before = 1.0 + 2.0 * (time / 2.0);
            assert!(
                (y - before).abs() < 1e-4,
                "t={time}: expected {before}, got {y}"
            );
        }
    }

    #[test]
    fn transformed_animation_deforms_the_same_way_as_the_original() {
        use crate::animation::{Animation, BoneProperty, Keyframe, KeyframeTarget};
        use crate::bone::Bone;
        use crate::shape::{Path, Weights};

        let mut path = Path::new([0.0, 0.0]).line_to([10.0, 0.0]);
        let weights = {
            let mut weights = Weights::new();
            weights.set(0, 1.0);
            weights
        };
        path.start_weights = weights.clone();
        path.segments[0].to_weights = weights;

        let mut animation = Animation::new(1.0);
        for (timestamp, x, y) in [(0.0, 2.0, 1.0), (1.0, 4.0, -1.0)] {
            animation.insert_keyframe(Keyframe::new(
                timestamp,
                x,
                KeyframeTarget::Bone {
                    bone: 0,
                    property: BoneProperty::X,
                },
            ));
            animation.insert_keyframe(Keyframe::new(
                timestamp,
                y,
                KeyframeTarget::Bone {
                    bone: 0,
                    property: BoneProperty::Y,
                },
            ));
        }
        let mut asset = Asset {
            shapes: vec![Shape {
                paths: vec![path],
                ..Default::default()
            }],
            bones: vec![Bone {
                head: Some([0.0, 0.0]),
                tail: [10.0, 0.0],
                ..Default::default()
            }],
            animations: vec![animation],
        };

        let time = 0.25;
        let before = asset.deformed(&asset.animations[0].sample_pose(&asset, time));
        let original = before.shapes[0].paths[0].clone();

        let transform = Similarity {
            scale: 2.0,
            rotation: 0.7,
            translation: [5.0, -3.0],
        };
        asset.apply_transform(transform);

        let after = asset.deformed(&asset.animations[0].sample_pose(&asset, time));
        let transformed = &after.shapes[0].paths[0];

        let near = |actual: Point, expected: Point| {
            assert!(
                (actual[0] - expected[0]).abs() < 1e-4 && (actual[1] - expected[1]).abs() < 1e-4,
                "expected {expected:?}, got {actual:?}"
            );
        };
        near(
            transformed.start().unwrap(),
            transform.apply(original.start().unwrap()),
        );
        near(
            transformed.segments[0].to,
            transform.apply(original.segments[0].to),
        );
    }

    #[test]
    fn pure_scale_transform_matches_scale_uniform() {
        let mut scaled = animated_translation_asset(true);
        let mut transformed = scaled.clone();

        scaled.scale_uniform(2.5);
        transformed.apply_transform(Similarity {
            scale: 2.5,
            rotation: 0.0,
            translation: [0.0, 0.0],
        });

        assert_eq!(scaled, transformed);
    }

    #[test]
    fn scaling_by_one_leaves_the_asset_unchanged() {
        let mut asset = sample_asset();
        let before = asset.clone();

        asset.scale_uniform(1.0);

        assert_eq!(asset, before);
    }

    #[test]
    fn old_version_assets_are_rejected() {
        let json = r#"{"type": "viveco", "version": 5}"#;
        assert!(Asset::from_json_str(json).is_err());
    }
}
