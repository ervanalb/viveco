//! Bevy asset loader for `.viveco` files.

use std::fmt;
use std::io;

use ::bevy::asset::{AssetLoader, LoadContext, io::Reader};
use ::bevy::reflect::TypePath;
use ::bevy::tasks::ConditionalSendFuture;

use crate::BinaryDecodeError;

/// Loads Viveco documents from the binary or JSON encoding.
///
/// The encoding is detected from the file contents, matching
/// [`Asset::load_from_file`](crate::Asset::load_from_file).
#[derive(Default, TypePath)]
pub struct VivecoLoader;

/// Errors produced while loading a Viveco asset.
#[derive(Debug)]
pub enum VivecoLoaderError {
    /// Reading the file failed.
    Io(io::Error),
    /// The file was not valid UTF-8 while being parsed as JSON.
    Utf8(std::str::Utf8Error),
    /// The JSON document was invalid.
    Json(serde_json::Error),
    /// The binary document was invalid.
    Binary(BinaryDecodeError),
}

impl fmt::Display for VivecoLoaderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(err) => write!(f, "could not read viveco asset: {err}"),
            Self::Utf8(err) => write!(f, "viveco JSON is not UTF-8: {err}"),
            Self::Json(err) => write!(f, "invalid viveco JSON: {err}"),
            Self::Binary(err) => write!(f, "invalid viveco binary: {err}"),
        }
    }
}

impl std::error::Error for VivecoLoaderError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(err) => Some(err),
            Self::Utf8(err) => Some(err),
            Self::Json(err) => Some(err),
            Self::Binary(err) => Some(err),
        }
    }
}

impl From<io::Error> for VivecoLoaderError {
    fn from(err: io::Error) -> Self {
        Self::Io(err)
    }
}

impl From<std::str::Utf8Error> for VivecoLoaderError {
    fn from(err: std::str::Utf8Error) -> Self {
        Self::Utf8(err)
    }
}

impl From<serde_json::Error> for VivecoLoaderError {
    fn from(err: serde_json::Error) -> Self {
        Self::Json(err)
    }
}

impl From<BinaryDecodeError> for VivecoLoaderError {
    fn from(err: BinaryDecodeError) -> Self {
        Self::Binary(err)
    }
}

impl AssetLoader for VivecoLoader {
    type Asset = crate::Asset;
    type Settings = ();
    type Error = VivecoLoaderError;

    fn load(
        &self,
        reader: &mut dyn Reader,
        _settings: &Self::Settings,
        _load_context: &mut LoadContext,
    ) -> impl ConditionalSendFuture<Output = Result<Self::Asset, Self::Error>> {
        Box::pin(async move {
            let mut bytes = Vec::new();
            reader.read_to_end(&mut bytes).await?;

            let asset = match crate::Asset::from_postcard_slice(&bytes) {
                Ok(asset) => asset,
                // Anything that is not binary is treated as JSON, mirroring
                // the library's content-based detection.
                Err(BinaryDecodeError::MissingMagic) => {
                    crate::Asset::from_json_str(std::str::from_utf8(&bytes)?)?
                }
                Err(err) => return Err(err.into()),
            };
            Ok(asset)
        })
    }

    fn extensions(&self) -> &[&str] {
        &["viveco"]
    }
}
