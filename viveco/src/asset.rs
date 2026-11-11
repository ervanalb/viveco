//! The root of a vector-art asset and its file encodings.

use std::io;
use std::path::Path as FsPath;

use serde::de;
use serde::ser::SerializeStruct;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::animation::{Animation, KeyframeTarget};
use crate::bone::{Bone, BoneIndex};
use crate::shape::{Shape, ShapeIndex, Weights};

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
    fn old_version_assets_are_rejected() {
        let json = r#"{"type": "viveco", "version": 5}"#;
        assert!(Asset::from_json_str(json).is_err());
    }
}
