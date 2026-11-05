//! Bevy asset loader for `.viveco` files.

use ::bevy::asset::{AssetLoader, LoadContext, io::Reader};
use ::bevy::reflect::TypePath;
use ::bevy::tasks::ConditionalSendFuture;

use crate::AssetLoadError;

/// Loads Viveco documents from the binary or JSON encoding.
///
/// Decoding is delegated to [`Asset::from_bytes`](crate::Asset::from_bytes),
/// which detects the encoding from the contents just as
/// [`Asset::load_from_file`](crate::Asset::load_from_file) does.
#[derive(Default, TypePath)]
pub struct VivecoLoader;

impl AssetLoader for VivecoLoader {
    type Asset = crate::Asset;
    type Settings = ();
    type Error = AssetLoadError;

    fn load(
        &self,
        reader: &mut dyn Reader,
        _settings: &Self::Settings,
        _load_context: &mut LoadContext,
    ) -> impl ConditionalSendFuture<Output = Result<Self::Asset, Self::Error>> {
        Box::pin(async move {
            let mut bytes = Vec::new();
            reader.read_to_end(&mut bytes).await?;
            crate::Asset::from_bytes(&bytes)
        })
    }

    fn extensions(&self) -> &[&str] {
        &["viveco"]
    }
}
