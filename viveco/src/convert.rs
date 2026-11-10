//! Converts Viveco assets and imports SVG documents.
//!
//! This module is enabled by the crate's `svg` feature. The input kind is
//! detected from the file's contents (with a little help from its extension):
//! an SVG document is imported as art, and a Viveco asset is decoded in
//! whichever encoding it uses. The output encoding is chosen from its
//! extension: `.json` writes JSON and anything else writes the binary format.
//!
//! When converting an SVG, [`ConvertOptions::group`] imports only the objects
//! inside the top-level `<g>` with that name. Groups are named by their
//! `inkscape:label` when they have one, and by their `id` otherwise.
//!
//! The conversion is a plain library function, so it can also run from a build
//! script:
//!
//! ```no_run
//! // build.rs
//! use viveco::convert::{ConvertOptions, convert_file};
//!
//! fn main() {
//!     println!("cargo:rerun-if-changed=assets/art.svg");
//!     let out_dir = std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
//!     convert_file(
//!         "assets/art.svg",
//!         out_dir.join("art.viveco"),
//!         &ConvertOptions::new().with_group("Front"),
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
use crate::svg::{SvgImportError, SvgImportOptions};

/// Names what a conversion should import.
///
/// The default converts the whole input.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ConvertOptions {
    /// Imports only the objects in the top-level group with this name.
    ///
    /// A top-level group is a `<g>` directly inside the root `<svg>`. Its name
    /// is its `inkscape:label` when present (as on Inkscape layers) and its
    /// `id` otherwise; both are accepted when both exist. This only applies to
    /// SVG input; setting it for a Viveco asset fails the conversion.
    pub group: Option<String>,
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

    /// The corresponding SVG import options.
    fn svg_options(&self) -> SvgImportOptions {
        SvgImportOptions {
            group: self.group.clone(),
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
            Self::GroupRequiresSvg => None,
        }
    }
}

/// Converts the document at `input` and writes the asset to `output`.
///
/// The input kind is detected from its contents. An SVG document is imported,
/// using `options` to select a group if requested; a Viveco asset is decoded in
/// whichever encoding it uses. [`Asset::save_to_file`] chooses the output
/// encoding from `output`'s extension (`.json` writes JSON, anything else
/// writes the binary format).
///
/// Use [`default_output`] to derive the same output path the `viveco-convert`
/// command line uses when none is given.
pub fn convert_file(
    input: impl AsRef<Path>,
    output: impl AsRef<Path>,
    options: &ConvertOptions,
) -> Result<(), ConvertError> {
    let input = input.as_ref();
    let output = output.as_ref();

    let bytes = std::fs::read(input).map_err(|source| ConvertError::Read {
        path: input.to_owned(),
        source,
    })?;

    let asset = if is_svg(input, &bytes) {
        Asset::from_svg_bytes_with_options(&bytes, &options.svg_options()).map_err(|source| {
            ConvertError::Svg {
                path: input.to_owned(),
                source,
            }
        })?
    } else {
        if options.group.is_some() {
            return Err(ConvertError::GroupRequiresSvg);
        }
        Asset::from_bytes(&bytes).map_err(|source| ConvertError::Asset {
            path: input.to_owned(),
            source,
        })?
    };

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
