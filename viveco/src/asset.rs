//! The root of a vector-art asset and its JSON encoding.

use std::io;
use std::path::Path as FsPath;

use serde::de;
use serde::ser::SerializeMap;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::animation::{Animation, KeyframeTarget};
use crate::bone::{Bone, BoneIndex};
use crate::shape::{Shape, ShapeIndex, Weights};

/// Identifier written to and expected in an asset's `type` field.
pub const FORMAT_TYPE: &str = "viveco";

/// Oldest asset version this build can load.
pub const MIN_FORMAT_VERSION: u32 = 5;

/// Version written to and expected by this build of the format.
pub const FORMAT_VERSION: u32 = 5;

/// The root of a vector-art file.
#[derive(Clone, Debug, PartialEq)]
pub struct Asset {
    pub shapes: Vec<Shape>,

    /// Top-level bones of the rig, in tree order. Empty for an unrigged
    /// asset.
    pub bones: Vec<Bone>,

    /// Animation clips stored in the asset.
    pub animations: Vec<Animation>,
}

impl Serialize for Asset {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(None)?;
        map.serialize_entry("type", FORMAT_TYPE)?;
        map.serialize_entry("version", &FORMAT_VERSION)?;
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
        if !(MIN_FORMAT_VERSION..=FORMAT_VERSION).contains(&wire.version) {
            return Err(de::Error::custom(format!(
                "unsupported viveco version {} (supported {MIN_FORMAT_VERSION}..={FORMAT_VERSION})",
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
        assert!(Asset::from_json_str(json).is_err());
    }
}
