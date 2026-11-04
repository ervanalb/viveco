//! Bevy integration for the viveco format.
//!
//! The plugin provides:
//!
//! * [`VivecoAsset`], a Bevy asset holding a compiled document. It borrows the
//!   embedded bytes when built from the [`viveco!`] proc macro, so drawing an
//!   asset costs no parsing allocation.
//! * [`VivecoLoader`], an [`AssetLoader`] for on-disk `.viveco` binaries.
//! * [`VivecoInstance`], the component that references an asset and lists the
//!   animation layers to blend.
//! * [`VivecoAnimator`], the playback component that advances each layer's
//!   time.
//!
//! An entity with a [`VivecoInstance`] gets a `VelloScene2d` component that the
//! plugin rebuilds every frame: first it samples and sums the animation layers
//! into a bone pose, then it skins every path point by the bones, then it hands
//! the resulting Vello scene to `bevy_vello`.
//!
//! ```no_run
//! use bevy::prelude::*;
//! use bevy_vello::{VelloPlugin, prelude::*};
//! use viveco_bevy::*;
//!
//! static STICK: &[u8] = viveco!("../stick.json");
//!
//! fn setup(mut commands: Commands, mut assets: ResMut<Assets<VivecoAsset>>) {
//!     let art = assets.add(VivecoAsset::from_static(STICK));
//!     commands.spawn(VivecoInstance::new(art).with_layer(1, 1.0, 0.0));
//! }
//!
//! App::new()
//!     .add_plugins(DefaultPlugins)
//!     .add_plugins(VelloPlugin::default())
//!     .add_plugins(VivecoPlugin)
//!     .add_systems(Startup, setup);
//! ```

use std::borrow::Cow;

use bevy::{
    asset::{AssetApp, AssetLoader, LoadContext, io::Reader},
    camera::visibility::NoFrustumCulling,
    prelude::*,
    tasks::ConditionalSendFuture,
};
use bevy_vello::prelude::*;

pub use viveco::{
    BonePose, Bounds, CurveRef, FormatError, KeyframeRef, Layer as AnimationLayer, PathRef, Pose,
    SegmentRef, ShapeRef, Similarity, Viveco, WeightRef, bone_worlds, build_scene, sample_curve,
    sample_pose,
};
pub use viveco_macros::viveco;

/// A compiled `.viveco` document.
///
/// Constructed either from the [`viveco!`] proc macro (which borrows the
/// embedded bytes, zero-copy) or from an on-disk binary loaded by
/// [`VivecoLoader`].
#[derive(Asset, TypePath, Clone, Debug)]
pub struct VivecoAsset {
    bytes: Cow<'static, [u8]>,
}

impl VivecoAsset {
    /// Wraps bytes embedded at compile time.
    ///
    /// Panics if the bytes are not a valid document. The proc macro guarantees
    /// validity, so this is only a guard against hand-written constants; use
    /// [`Self::try_from_static`] when the bytes are not known-good.
    pub fn from_static(bytes: &'static [u8]) -> Self {
        Self::try_from_static(bytes).expect("invalid viveco binary")
    }

    /// Fallible version of [`Self::from_static`].
    pub fn try_from_static(bytes: &'static [u8]) -> Result<Self, FormatError> {
        Viveco::parse(bytes)?;
        Ok(Self {
            bytes: Cow::Borrowed(bytes),
        })
    }

    /// Wraps an owned binary buffer, validating it once.
    pub fn try_from_vec(bytes: Vec<u8>) -> Result<Self, FormatError> {
        Viveco::parse(&bytes)?;
        Ok(Self {
            bytes: Cow::Owned(bytes),
        })
    }

    /// The raw compiled bytes.
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// A zero-copy view of the document.
    ///
    /// The asset was validated on construction, so this only re-reads the
    /// fixed header and section table, not the document body.
    pub fn data(&self) -> Result<Viveco<'_>, FormatError> {
        Viveco::parse_trusted(&self.bytes)
    }

    /// A zero-copy view, panicking if the bytes are invalid.
    pub fn expect_data(&self) -> Viveco<'_> {
        self.data().expect("invalid viveco binary")
    }
}

/// Errors produced by [`VivecoLoader`].
#[derive(Debug, thiserror::Error)]
pub enum VivecoError {
    #[error("failed to read viveco asset: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid viveco asset: {0}")]
    Format(#[from] FormatError),
}

/// Loader for compiled `.viveco` binaries on disk.
#[derive(Default, TypePath)]
pub struct VivecoLoader;

impl AssetLoader for VivecoLoader {
    type Asset = VivecoAsset;
    type Settings = ();
    type Error = VivecoError;

    fn load(
        &self,
        reader: &mut dyn Reader,
        _settings: &Self::Settings,
        _load_context: &mut LoadContext,
    ) -> impl ConditionalSendFuture<Output = Result<Self::Asset, Self::Error>> {
        Box::pin(async move {
            let mut bytes = Vec::new();
            reader.read_to_end(&mut bytes).await?;
            Ok(VivecoAsset::try_from_vec(bytes)?)
        })
    }

    fn extensions(&self) -> &[&str] {
        &["viveco"]
    }
}

/// What to draw and which animations to blend.
///
/// `layers` is deliberately small and plain: each entry names an animation by
/// index, plus its weight and local time. An entity with this component is
/// rendered by [`VivecoPlugin`]; the plugin maintains a `VelloScene2d` on the
/// same entity.
#[derive(Component, Clone, Debug)]
#[require(VivecoAnimator)]
pub struct VivecoInstance {
    /// The compiled document.
    pub art: Handle<VivecoAsset>,
    /// Animations to blend, in any order.
    pub layers: Vec<AnimationLayer>,
}

impl VivecoInstance {
    pub fn new(art: Handle<VivecoAsset>) -> Self {
        Self {
            art,
            layers: Vec::new(),
        }
    }

    /// Adds an animation layer. `animation` is an index into the document's
    /// animation array; names are not stored in the binary.
    pub fn with_layer(mut self, animation: usize, weight: f32, time: f32) -> Self {
        self.layers
            .push(AnimationLayer::new(animation, weight, time));
        self
    }
}

/// Advances each layer's local time every frame.
///
/// Cyclic animations wrap at their duration; non-cyclic ones clamp to it. Set
/// `paused` to take manual control of [`VivecoInstance::layers`] times.
#[derive(Component, Clone, Debug)]
pub struct VivecoAnimator {
    pub speed: f32,
    pub paused: bool,
}

impl Default for VivecoAnimator {
    fn default() -> Self {
        Self {
            speed: 1.0,
            paused: false,
        }
    }
}

/// Adds the viveco asset, loader, and rendering systems.
///
/// Add `VelloPlugin` (from `bevy_vello`) as well; this plugin only produces
/// Vello scenes and relies on Bevy Vello to rasterize them.
pub struct VivecoPlugin;

impl Plugin for VivecoPlugin {
    fn build(&self, app: &mut App) {
        app.init_asset::<VivecoAsset>()
            .register_asset_loader(VivecoLoader)
            .add_systems(Update, (advance_animations, render_vec_art).chain());
    }
}

/// Advances layer times for entities with a [`VivecoAnimator`].
fn advance_animations(
    time: Res<Time>,
    assets: Res<Assets<VivecoAsset>>,
    mut query: Query<(&VivecoAnimator, &mut VivecoInstance)>,
) {
    for (animator, mut instance) in &mut query {
        if animator.paused {
            continue;
        }
        let delta = time.delta_secs() * animator.speed;
        if delta == 0.0 {
            continue;
        }
        let Some(asset) = assets.get(&instance.art) else {
            continue;
        };
        let Ok(data) = asset.data() else {
            continue;
        };
        for layer in &mut instance.layers {
            let Some(animation) = data.animation(layer.animation) else {
                continue;
            };
            let duration = animation.duration();
            layer.time = if animation.cyclic() && duration > 0.0 {
                (layer.time + delta).rem_euclid(duration)
            } else {
                (layer.time + delta).clamp(0.0, duration.max(0.0))
            };
        }
    }
}

/// Rebuilds the Vello scene for every [`VivecoInstance`].
fn render_vec_art(
    mut commands: Commands,
    assets: Res<Assets<VivecoAsset>>,
    mut query: Query<(Entity, &VivecoInstance, Option<&mut VelloScene2d>)>,
) {
    for (entity, instance, scene) in &mut query {
        let Some(asset) = assets.get(&instance.art) else {
            continue;
        };
        let Ok(data) = asset.data() else {
            continue;
        };

        let pose = sample_pose(&data, &instance.layers);
        let mut vello_scene = vello::Scene::new();
        build_scene(&data, &pose, &mut vello_scene);

        match scene {
            Some(mut existing) => {
                *existing = VelloScene2d::from(vello_scene);
            }
            None => {
                commands
                    .entity(entity)
                    .insert((VelloScene2d::from(vello_scene), NoFrustumCulling));
            }
        }
    }
}
