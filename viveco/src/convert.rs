//! Converts Viveco assets and imports SVG documents.
//!
//! This module is enabled by the crate's `svg` feature. The input kind is
//! detected from the file's contents (with a little help from its extension):
//! an SVG document is imported as art, and a Viveco asset is decoded in
//! whichever encoding it uses. The output encoding is chosen from its
//! extension: `.json` writes JSON and anything else writes the binary format.
//!
//! A conversion is a pipeline: read the input (importing an SVG), keep only the
//! named top-level shapes, apply a new origin and then a uniform transform,
//! then write the result. [`ConvertOptions`] selects each optional step.
//!
//! When converting an SVG, [`ConvertOptions::group`] imports only the objects
//! inside the top-level `<g>` with that name. Groups are named by their
//! `inkscape:label` when they have one, and by their `id` otherwise.
//! [`ConvertOptions::keep`] then filters the top-level shapes of either input
//! kind by name. [`ConvertOptions::origin`] moves the filtered artwork so a
//! chosen anchor of its bounds becomes `(0, 0)`. [`ConvertOptions::transform`]
//! then scales, rotates, and translates the rest artwork and skeleton,
//! rewriting bone translation curves so animation stays consistent.
//!
//! The conversion is a plain library function, so it can also run from a build
//! script:
//!
//! ```no_run
//! // build.rs
//! use viveco::convert::{ConvertOptions, Origin, convert_file};
//!
//! fn main() {
//!     println!("cargo:rerun-if-changed=assets/art.svg");
//!     let out_dir = std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
//!     convert_file(
//!         "assets/art.svg",
//!         out_dir.join("art.viveco"),
//!         // Every option has a builder: with_group, with_keep, with_origin,
//!         // and with_transform.
//!         &ConvertOptions::new()
//!             .with_group("Front")
//!             .with_origin(Origin::TopLeft),
//!     )
//!     .expect("convert art.svg");
//! }
//! ```
//!
//! A build script that regenerates an asset should print
//! `cargo:rerun-if-changed` for every input it reads, as above; this module
//! does not print Cargo directives itself.
//!
//! To use this from a build script without compiling Bevy or Vello, disable
//! the default features and enable `svg`:
//!
//! ```toml
//! [build-dependencies]
//! viveco = { version = "...", default-features = false, features = ["svg"] }
//! ```

use std::ffi::OsStr;
use std::fmt;
use std::path::{Path, PathBuf};

use crate::Asset;
use crate::asset::Bounds;
use crate::pose::Similarity;
use crate::shape::Point;
use crate::svg::{SvgImportError, SvgImportOptions};

/// Names what a conversion should import, keep, and transform.
///
/// The default converts the whole input unchanged.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ConvertOptions {
    /// Imports only the objects in the top-level group with this name.
    ///
    /// A top-level group is a `<g>` directly inside the root `<svg>`. Its name
    /// is its `inkscape:label` when present (as on Inkscape layers) and its
    /// `id` otherwise; both are accepted when both exist. This only applies to
    /// SVG input; setting it for a Viveco asset fails the conversion.
    pub group: Option<String>,

    /// Keeps only the top-level shapes with these names.
    ///
    /// Filtering happens after the input is read (and after an SVG is
    /// imported), so it applies equally to both input kinds. The whole subtree
    /// of a kept shape is preserved; animation keyframes for dropped shapes are
    /// removed. Empty means keep every shape. A name that matches no top-level
    /// shape fails the conversion.
    pub keep: Vec<String>,

    /// A uniform scale, rotation, and translation applied after filtering and
    /// after [`origin`](Self::origin).
    ///
    /// This transforms the rest artwork and skeleton, and rewrites bone
    /// translation curves so animation stays consistent. See
    /// [`TransformOptions`]. The default applies no transform.
    pub transform: TransformOptions,

    /// Moves the filtered artwork so that this anchor of its rest bounds
    /// becomes the asset-space origin `(0, 0)`.
    ///
    /// The anchor is measured after [`keep`](Self::keep) has run, so it always
    /// refers to the artwork that actually gets written. This is a translation
    /// of the whole asset, skeleton included; bone translation curves keep
    /// their relative motion. `None` leaves the origin alone, and artwork with
    /// no geometry has no anchor to move to.
    ///
    /// When a [`transform`](Self::transform) is also set, the origin is applied
    /// first, so the transform's scale and rotation act on the re-origined
    /// artwork.
    pub origin: Option<Origin>,
}

impl ConvertOptions {
    /// Options that convert the whole input.
    pub fn new() -> Self {
        Self::default()
    }

    /// Restricts an SVG import to the top-level group with the given name.
    pub fn with_group(mut self, group: impl Into<String>) -> Self {
        self.group = Some(group.into());
        self
    }

    /// Keeps only the top-level shape with the given name.
    ///
    /// Call this once per name. An empty set keeps every shape.
    pub fn with_keep(mut self, name: impl Into<String>) -> Self {
        self.keep.push(name.into());
        self
    }

    /// Keeps only the top-level shapes in `names`.
    pub fn with_keep_all(mut self, names: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.keep.extend(names.into_iter().map(Into::into));
        self
    }

    /// Applies a scale, rotation, and translation after filtering.
    ///
    /// This composes with [`Self::with_origin`]: the origin is applied first.
    pub fn with_transform(mut self, transform: TransformOptions) -> Self {
        self.transform = transform;
        self
    }

    /// Moves the filtered artwork so the chosen bounds anchor is `(0, 0)`.
    ///
    /// This composes with [`Self::with_transform`]: the origin is applied
    /// first.
    pub fn with_origin(mut self, origin: Origin) -> Self {
        self.origin = Some(origin);
        self
    }

    /// The corresponding SVG import options.
    fn svg_options(&self) -> SvgImportOptions {
        SvgImportOptions {
            group: self.group.clone(),
        }
    }
}

/// A uniform scale, rotation, and translation applied to a converted asset.
///
/// Scale and rotation act about the asset-space origin, and the translation is
/// applied afterwards. Points transform as
/// `translate + scale * rotate(point)`. To scale or rotate about another point,
/// set [`ConvertOptions::origin`] first; that moves the artwork so the chosen
/// anchor becomes the origin.
///
/// Only lengths are transformed. Shape and animation Z (paint order), bone
/// rotation and scale curves, animation timings, and bone weights are
/// untouched. Bone translation curves are transformed as vectors so animated
/// motion rotates and scales with the rest of the artwork.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TransformOptions {
    /// Offset applied after scaling and rotating.
    pub translate: Point,
    /// Rotation in radians. Asset space has Y pointing down, so positive
    /// angles turn clockwise on screen.
    pub rotate: f32,
    /// Uniform scale factor. Must be positive and finite to be meaningful.
    pub scale: f32,
}

impl Default for TransformOptions {
    fn default() -> Self {
        Self {
            translate: [0.0, 0.0],
            rotate: 0.0,
            scale: 1.0,
        }
    }
}

impl TransformOptions {
    /// A transform that leaves the asset unchanged.
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether this transform would leave the asset unchanged.
    pub fn is_identity(&self) -> bool {
        self.translate == [0.0, 0.0] && self.rotate == 0.0 && self.scale == 1.0
    }

    /// Sets the translation applied after scaling and rotating.
    pub fn with_translate(mut self, translate: Point) -> Self {
        self.translate = translate;
        self
    }

    /// Sets the rotation in radians.
    pub fn with_rotate(mut self, radians: f32) -> Self {
        self.rotate = radians;
        self
    }

    /// Sets the uniform scale factor.
    pub fn with_scale(mut self, scale: f32) -> Self {
        self.scale = scale;
        self
    }

    /// The similarity this transform describes.
    fn similarity(&self) -> Similarity {
        Similarity {
            scale: self.scale,
            rotation: self.rotate,
            translation: self.translate,
        }
    }
}

/// A named anchor on an artwork's bounds, used as a new origin.
///
/// Asset space has Y pointing down, so [`Top`](Self::Top) is the smallest Y and
/// [`Bottom`](Self::Bottom) the largest. See [`ConvertOptions::origin`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Origin {
    /// The top-left corner of the bounds.
    TopLeft,
    /// The midpoint of the top edge.
    Top,
    /// The top-right corner of the bounds.
    TopRight,
    /// The midpoint of the left edge.
    Left,
    /// The center of the bounds.
    Center,
    /// The midpoint of the right edge.
    Right,
    /// The bottom-left corner of the bounds.
    BottomLeft,
    /// The midpoint of the bottom edge.
    Bottom,
    /// The bottom-right corner of the bounds.
    BottomRight,
}

impl Origin {
    /// Every origin, in reading order.
    pub const ALL: [Self; 9] = [
        Self::TopLeft,
        Self::Top,
        Self::TopRight,
        Self::Left,
        Self::Center,
        Self::Right,
        Self::BottomLeft,
        Self::Bottom,
        Self::BottomRight,
    ];

    /// The canonical hyphenated name, as accepted from the command line.
    pub fn name(self) -> &'static str {
        match self {
            Self::TopLeft => "top-left",
            Self::Top => "top",
            Self::TopRight => "top-right",
            Self::Left => "left",
            Self::Center => "center",
            Self::Right => "right",
            Self::BottomLeft => "bottom-left",
            Self::Bottom => "bottom",
            Self::BottomRight => "bottom-right",
        }
    }

    /// The anchor point this origin names on `bounds`.
    pub fn anchor(self, bounds: Bounds) -> Point {
        let center = bounds.center();
        match self {
            Self::TopLeft => bounds.min,
            Self::Top => [center[0], bounds.min[1]],
            Self::TopRight => [bounds.max[0], bounds.min[1]],
            Self::Left => [bounds.min[0], center[1]],
            Self::Center => center,
            Self::Right => [bounds.max[0], center[1]],
            Self::BottomLeft => [bounds.min[0], bounds.max[1]],
            Self::Bottom => [center[0], bounds.max[1]],
            Self::BottomRight => bounds.max,
        }
    }
}

impl fmt::Display for Origin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

impl std::str::FromStr for Origin {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let text = text.trim();
        match text.to_ascii_lowercase().as_str() {
            "top-left" => Ok(Self::TopLeft),
            "top" => Ok(Self::Top),
            "top-right" => Ok(Self::TopRight),
            "left" => Ok(Self::Left),
            "center" => Ok(Self::Center),
            "right" => Ok(Self::Right),
            "bottom-left" => Ok(Self::BottomLeft),
            "bottom" => Ok(Self::Bottom),
            "bottom-right" => Ok(Self::BottomRight),
            _ => {
                let options = Self::ALL.map(Self::name).join(", ");
                Err(format!("expected origin {options}, found `{text}`"))
            }
        }
    }
}

/// Why a conversion failed.
#[derive(Debug)]
pub enum ConvertError {
    /// Reading the input failed.
    Read {
        /// The input path that could not be read.
        path: PathBuf,
        /// The underlying I/O error.
        source: std::io::Error,
    },
    /// The input was an SVG document, and importing it failed.
    Svg {
        /// The input path that could not be imported.
        path: PathBuf,
        /// The underlying import error.
        source: SvgImportError,
    },
    /// The input was a Viveco asset, and loading it failed.
    Asset {
        /// The input path that could not be loaded.
        path: PathBuf,
        /// The underlying decoding error.
        source: crate::AssetLoadError,
    },
    /// [`ConvertOptions::group`] was set for input that is not an SVG document.
    GroupRequiresSvg,
    /// A name in [`ConvertOptions::keep`] matched no top-level shape.
    UnknownShapes {
        /// The requested names that matched nothing.
        missing: Vec<String>,
        /// Every top-level shape name in the input, in document order.
        available: Vec<String>,
    },
    /// Writing the output failed.
    Write {
        /// The output path that could not be written.
        path: PathBuf,
        /// The underlying I/O error.
        source: std::io::Error,
    },
}

impl fmt::Display for ConvertError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Read { path, source } => write!(f, "failed to read {}: {source}", path.display()),
            Self::Svg { path, source } => {
                write!(f, "failed to import {}: {source}", path.display())
            }
            Self::Asset { path, source } => {
                write!(f, "failed to load {}: {source}", path.display())
            }
            Self::GroupRequiresSvg => f.write_str("--group only applies to SVG input"),
            Self::UnknownShapes { missing, available } => {
                if available.is_empty() {
                    write!(
                        f,
                        "no top-level shape named {}; the input has no top-level shapes",
                        quoted(missing)
                    )
                } else {
                    write!(
                        f,
                        "no top-level shape named {}; available top-level shapes: {}",
                        quoted(missing),
                        quoted(available)
                    )
                }
            }
            Self::Write { path, source } => {
                write!(f, "failed to write {}: {source}", path.display())
            }
        }
    }
}

impl std::error::Error for ConvertError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Read { source, .. } | Self::Write { source, .. } => Some(source),
            Self::Svg { source, .. } => Some(source),
            Self::Asset { source, .. } => Some(source),
            Self::GroupRequiresSvg | Self::UnknownShapes { .. } => None,
        }
    }
}

/// Formats `names` as a comma-separated list of backtick-quoted names.
fn quoted(names: &[String]) -> String {
    names
        .iter()
        .map(|name| format!("`{name}`"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Reads the document at `input` into an [`Asset`].
///
/// The input kind is detected from its contents. An SVG document is imported,
/// using [`ConvertOptions::group`] to select a group if requested; a Viveco
/// asset is decoded in whichever encoding it uses. The other options do not
/// affect loading, so a caller that only wants to inspect a file can pass
/// default options.
pub fn load_asset(
    input: impl AsRef<Path>,
    options: &ConvertOptions,
) -> Result<Asset, ConvertError> {
    let input = input.as_ref();
    let bytes = std::fs::read(input).map_err(|source| ConvertError::Read {
        path: input.to_owned(),
        source,
    })?;

    if is_svg(input, &bytes) {
        Asset::from_svg_bytes_with_options(&bytes, &options.svg_options()).map_err(|source| {
            ConvertError::Svg {
                path: input.to_owned(),
                source,
            }
        })
    } else {
        if options.group.is_some() {
            return Err(ConvertError::GroupRequiresSvg);
        }
        Asset::from_bytes(&bytes).map_err(|source| ConvertError::Asset {
            path: input.to_owned(),
            source,
        })
    }
}

/// Converts the document at `input` and writes the asset to `output`.
///
/// The conversion runs in four steps:
///
/// 1. Read the input, importing it as SVG art if it is an SVG document.
/// 2. Keep only the top-level shapes named in [`ConvertOptions::keep`], if any.
/// 3. Apply [`ConvertOptions::origin`], then [`ConvertOptions::transform`].
/// 4. Write the asset.
///
/// [`Asset::save_to_file`] chooses the output encoding from `output`'s
/// extension (`.json` writes JSON, anything else writes the binary format).
///
/// Use [`default_output`] to derive the same output path the `viveco`
/// command line uses when none is given.
pub fn convert_file(
    input: impl AsRef<Path>,
    output: impl AsRef<Path>,
    options: &ConvertOptions,
) -> Result<(), ConvertError> {
    let input = input.as_ref();
    let output = output.as_ref();

    let mut asset = load_asset(input, options)?;

    if !options.keep.is_empty()
        && let Err(missing) = asset.retain_top_level_shapes(&options.keep)
    {
        let available = asset
            .shapes
            .iter()
            .map(|shape| {
                if shape.name.is_empty() {
                    "(unnamed)".to_owned()
                } else {
                    shape.name.clone()
                }
            })
            .collect();
        return Err(ConvertError::UnknownShapes { missing, available });
    }

    if let Some(origin) = options.origin {
        // The anchor is measured after filtering, so it describes the artwork
        // that is actually written.
        if let Some(bounds) = asset.bounds() {
            let anchor = origin.anchor(bounds);
            if anchor != [0.0, 0.0] {
                asset.apply_transform(Similarity {
                    scale: 1.0,
                    rotation: 0.0,
                    translation: [-anchor[0], -anchor[1]],
                });
            }
        }
    }

    if !options.transform.is_identity() {
        asset.apply_transform(options.transform.similarity());
    }

    asset
        .save_to_file(output)
        .map_err(|source| ConvertError::Write {
            path: output.to_owned(),
            source,
        })
}

/// Whether `input` is an SVG document.
///
/// The extension is trusted for `.svg` and `.svgz`; otherwise the contents are
/// inspected, so SVG input works under another name. A Viveco asset can never
/// be mistaken for one because its encodings start with `{` or a binary magic.
pub fn is_svg(input: &Path, bytes: &[u8]) -> bool {
    let extension_is_svg = input
        .extension()
        .and_then(OsStr::to_str)
        .is_some_and(|extension| {
            extension.eq_ignore_ascii_case("svg") || extension.eq_ignore_ascii_case("svgz")
        });
    extension_is_svg || looks_like_svg(bytes)
}

/// Whether the beginning of `bytes` is an XML document containing `<svg>`.
fn looks_like_svg(bytes: &[u8]) -> bool {
    let head = &bytes[..bytes.len().min(4096)];
    let text = String::from_utf8_lossy(head);
    let text = text.trim_start_matches('\u{feff}').trim_start();
    text.starts_with('<') && text.contains("<svg")
}

/// The output path used when the caller does not name one.
///
/// JSON and SVG inputs are converted to the binary Viveco format; anything
/// else is written as JSON.
pub fn default_output(input: &Path) -> PathBuf {
    let source_encoding = input
        .extension()
        .and_then(OsStr::to_str)
        .is_some_and(|extension| {
            extension.eq_ignore_ascii_case("json")
                || extension.eq_ignore_ascii_case("svg")
                || extension.eq_ignore_ascii_case("svgz")
        });
    input.with_extension(if source_encoding { "viveco" } else { "json" })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_output_flips_the_encoding() {
        assert_eq!(
            default_output(Path::new("art.json")),
            PathBuf::from("art.viveco")
        );
        assert_eq!(
            default_output(Path::new("art.JSON")),
            PathBuf::from("art.viveco")
        );
        assert_eq!(
            default_output(Path::new("art.viveco")),
            PathBuf::from("art.json")
        );
        assert_eq!(default_output(Path::new("art")), PathBuf::from("art.json"));
    }

    #[test]
    fn svg_input_defaults_to_the_binary_format() {
        assert_eq!(
            default_output(Path::new("art.svg")),
            PathBuf::from("art.viveco")
        );
        assert_eq!(
            default_output(Path::new("art.svgz")),
            PathBuf::from("art.viveco")
        );
    }

    #[test]
    fn detects_svg_by_extension_or_contents() {
        let svg = br#"<?xml version="1.0"?><svg xmlns="http://www.w3.org/2000/svg"/>"#;
        assert!(is_svg(Path::new("art.svg"), b""));
        assert!(is_svg(Path::new("art"), svg));
        assert!(!is_svg(Path::new("art.viveco"), b"\xff\xfe"));
        assert!(!is_svg(Path::new("art.json"), br#"{"shapes":[]}"#));
    }

    #[test]
    fn builds_svg_import_options_from_the_group() {
        assert_eq!(ConvertOptions::new().svg_options(), SvgImportOptions::new());
        assert_eq!(
            ConvertOptions::new().with_group("Front").svg_options(),
            SvgImportOptions::new().with_group("Front")
        );
    }

    #[test]
    fn converts_a_selected_svg_group_to_the_binary_format() {
        let svg = r##"<svg xmlns="http://www.w3.org/2000/svg"
                xmlns:inkscape="http://www.inkscape.org/namespaces/inkscape"
                viewBox="0 0 10 10">
              <g inkscape:label="Back" id="back">
                <rect id="back-rect" width="1" height="1" fill="#000"/>
              </g>
              <g inkscape:label="Front" id="front">
                <rect id="front-rect" width="1" height="1" fill="#000"/>
              </g>
            </svg>"##;
        let (input, output) = temp_paths("svg_to_binary");
        std::fs::write(&input, svg).unwrap();

        convert_file(&input, &output, &ConvertOptions::new().with_group("Front")).unwrap();

        let bytes = std::fs::read(&output).unwrap();
        assert_eq!(
            Asset::from_bytes(&bytes).unwrap().shapes[0].name,
            "front-rect"
        );

        std::fs::remove_file(&input).unwrap();
        std::fs::remove_file(&output).unwrap();
    }

    #[test]
    fn rejects_a_group_for_non_svg_input() {
        let (input, output) = temp_paths("group_requires_svg");
        // The `.svg` extension would make the input be treated as SVG, so use
        // a JSON path for a Viveco asset.
        let input = input.with_extension("json");
        let json = Asset::new().to_json_string().unwrap();
        std::fs::write(&input, json).unwrap();

        let err =
            convert_file(&input, &output, &ConvertOptions::new().with_group("Front")).unwrap_err();
        assert!(matches!(err, ConvertError::GroupRequiresSvg));

        std::fs::remove_file(&input).unwrap();
    }

    #[test]
    fn keeping_top_level_shapes_filters_and_remaps_animation_targets() {
        use crate::animation::{Animation, Keyframe, KeyframeTarget, ShapeProperty};
        use crate::shape::{Path, Shape};

        let mut animation = Animation::new(1.0);
        for shape in 0..2 {
            animation.insert_keyframe(Keyframe::new(
                shape as f32 * 0.1,
                0.0,
                KeyframeTarget::Shape {
                    shape,
                    property: ShapeProperty::Z,
                },
            ));
        }
        let asset = Asset {
            shapes: vec![
                Shape {
                    name: "A".into(),
                    paths: vec![Path::new([0.0, 0.0])],
                    ..Default::default()
                },
                Shape {
                    name: "B".into(),
                    paths: vec![Path::new([10.0, 0.0])],
                    ..Default::default()
                },
            ],
            animations: vec![animation],
            ..Default::default()
        };

        let (input, output) = temp_paths("keep");
        let input = input.with_extension("json");
        std::fs::write(&input, asset.to_json_string().unwrap()).unwrap();

        convert_file(&input, &output, &ConvertOptions::new().with_keep("B")).unwrap();

        let loaded = Asset::from_bytes(&std::fs::read(&output).unwrap()).unwrap();
        assert_eq!(loaded.shapes.len(), 1);
        assert_eq!(loaded.shapes[0].name, "B");
        // B was flat shape 1 and is now shape 0; A's keyframe is gone.
        assert_eq!(loaded.animations[0].keyframes.len(), 1);
        assert!(matches!(
            loaded.animations[0].keyframes[0].target,
            KeyframeTarget::Shape { shape: 0, .. }
        ));

        std::fs::remove_file(&input).unwrap();
        std::fs::remove_file(&output).unwrap();
    }

    #[test]
    fn unknown_keep_names_fail_and_list_the_available_shapes() {
        use crate::shape::Shape;

        let asset = Asset {
            shapes: vec![
                Shape {
                    name: "A".into(),
                    ..Default::default()
                },
                Shape {
                    name: "B".into(),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let (input, output) = temp_paths("keep_unknown");
        let input = input.with_extension("json");
        std::fs::write(&input, asset.to_json_string().unwrap()).unwrap();

        let err = convert_file(
            &input,
            &output,
            &ConvertOptions::new().with_keep("Nope").with_keep("B"),
        )
        .unwrap_err();

        match err {
            ConvertError::UnknownShapes { missing, available } => {
                assert_eq!(missing, ["Nope"]);
                assert_eq!(available, ["A", "B"]);
            }
            other => panic!("expected UnknownShapes, got {other}"),
        }

        std::fs::remove_file(&input).unwrap();
    }

    #[test]
    fn the_transform_runs_after_filtering() {
        use crate::shape::{Path, Shape};

        let asset = Asset {
            shapes: vec![
                Shape {
                    name: "Keep".into(),
                    paths: vec![Path::new([0.0, 0.0]).line_to([10.0, 10.0])],
                    ..Default::default()
                },
                Shape {
                    name: "Drop".into(),
                    paths: vec![Path::new([100.0, 100.0]).line_to([200.0, 200.0])],
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let (input, output) = temp_paths("transform");
        let input = input.with_extension("json");
        std::fs::write(&input, asset.to_json_string().unwrap()).unwrap();

        let transform = TransformOptions::new()
            .with_scale(2.0)
            .with_translate([5.0, 0.0]);
        let options = ConvertOptions::new()
            .with_keep("Keep")
            .with_transform(transform);
        convert_file(&input, &output, &options).unwrap();

        let loaded = Asset::from_bytes(&std::fs::read(&output).unwrap()).unwrap();
        // The kept shape is (0, 0)..(10, 10). Doubling it about the origin
        // gives (0, 0)..(20, 20), and the translation moves that to
        // (5, 0)..(25, 20). The dropped shape is gone before the transform.
        let bounds = loaded.bounds().unwrap();
        assert_eq!(bounds.min, [5.0, 0.0]);
        assert_eq!(bounds.max, [25.0, 20.0]);

        std::fs::remove_file(&input).unwrap();
        std::fs::remove_file(&output).unwrap();
    }

    #[test]
    fn an_identity_transform_is_not_applied() {
        let options = ConvertOptions::new();
        assert!(options.transform.is_identity());

        let transform = TransformOptions::new()
            .with_scale(2.0)
            .with_rotate(0.5)
            .with_translate([1.0, 2.0]);
        assert!(!transform.is_identity());
    }

    #[test]
    fn origins_name_the_expected_bounds_anchors() {
        let bounds = Bounds {
            min: [10.0, 20.0],
            max: [30.0, 60.0],
        };
        let center = [20.0, 40.0];

        for (origin, point) in [
            (Origin::TopLeft, [10.0, 20.0]),
            (Origin::Top, [20.0, 20.0]),
            (Origin::TopRight, [30.0, 20.0]),
            (Origin::Left, [10.0, 40.0]),
            (Origin::Center, center),
            (Origin::Right, [30.0, 40.0]),
            (Origin::BottomLeft, [10.0, 60.0]),
            (Origin::Bottom, [20.0, 60.0]),
            (Origin::BottomRight, [30.0, 60.0]),
        ] {
            assert_eq!(origin.anchor(bounds), point, "{origin}");
        }
    }

    #[test]
    fn origin_names_round_trip_and_are_case_insensitive() {
        for origin in Origin::ALL {
            assert_eq!(origin.name().parse::<Origin>(), Ok(origin));
            assert_eq!(origin.to_string(), origin.name());
        }
        assert_eq!("TOP-LEFT".parse::<Origin>(), Ok(Origin::TopLeft));
        assert!("middle".parse::<Origin>().is_err());
    }

    #[test]
    fn origin_moves_the_filtered_artwork_to_zero() {
        use crate::shape::{Path, Shape};

        let asset = Asset {
            shapes: vec![
                Shape {
                    name: "Keep".into(),
                    paths: vec![Path::new([10.0, 20.0]).line_to([20.0, 30.0])],
                    ..Default::default()
                },
                Shape {
                    name: "Drop".into(),
                    paths: vec![Path::new([100.0, 100.0]).line_to([200.0, 200.0])],
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let (input, output) = temp_paths("origin");
        let input = input.with_extension("json");
        std::fs::write(&input, asset.to_json_string().unwrap()).unwrap();

        let options = ConvertOptions::new()
            .with_keep("Keep")
            .with_origin(Origin::BottomRight);
        convert_file(&input, &output, &options).unwrap();

        // The kept shape's bottom-right is (20, 30), so it becomes the origin.
        let loaded = Asset::from_bytes(&std::fs::read(&output).unwrap()).unwrap();
        let bounds = loaded.bounds().unwrap();
        assert_eq!(bounds.max, [0.0, 0.0]);
        assert_eq!(bounds.min, [-10.0, -10.0]);

        std::fs::remove_file(&input).unwrap();
        std::fs::remove_file(&output).unwrap();
    }

    #[test]
    fn origin_is_applied_before_the_transform() {
        use crate::shape::{Path, Shape};

        let asset = Asset {
            shapes: vec![Shape {
                name: "Keep".into(),
                paths: vec![Path::new([10.0, 20.0]).line_to([20.0, 30.0])],
                ..Default::default()
            }],
            ..Default::default()
        };
        let (input, output) = temp_paths("origin_then_transform");
        let input = input.with_extension("json");
        std::fs::write(&input, asset.to_json_string().unwrap()).unwrap();

        let options = ConvertOptions::new()
            .with_origin(Origin::TopLeft)
            .with_transform(
                TransformOptions::new()
                    .with_scale(2.0)
                    .with_translate([1.0, 0.0]),
            );
        convert_file(&input, &output, &options).unwrap();

        // Re-origining first moves the bounds to (0, 0)..(10, 10). Doubling
        // about the origin gives (0, 0)..(20, 20), then the translation moves
        // it to (1, 0)..(21, 20). Applying the transform first would have
        // produced different bounds.
        let loaded = Asset::from_bytes(&std::fs::read(&output).unwrap()).unwrap();
        let bounds = loaded.bounds().unwrap();
        assert_eq!(bounds.min, [1.0, 0.0]);
        assert_eq!(bounds.max, [21.0, 20.0]);

        std::fs::remove_file(&input).unwrap();
        std::fs::remove_file(&output).unwrap();
    }

    #[test]
    fn builders_keep_origin_and_transform_together() {
        let options = ConvertOptions::new()
            .with_transform(TransformOptions::new().with_scale(2.0))
            .with_origin(Origin::Top);
        assert_eq!(options.origin, Some(Origin::Top));
        assert!(!options.transform.is_identity());

        let options = options.with_transform(TransformOptions::new().with_rotate(1.0));
        assert_eq!(options.origin, Some(Origin::Top));
        assert!(!options.transform.is_identity());
    }

    /// Two distinct temporary paths tagged with the test's process.
    fn temp_paths(tag: &str) -> (PathBuf, PathBuf) {
        let dir = std::env::temp_dir();
        let tag = format!("viveco_convert_{tag}_{}", std::process::id());
        (
            dir.join(format!("{tag}.svg")),
            dir.join(format!("{tag}.viveco")),
        )
    }
}
