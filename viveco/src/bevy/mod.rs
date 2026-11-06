//! Bevy integration for Viveco assets.
//!
//! This module is enabled by the crate's `bevy` feature. It provides two
//! independent pieces:
//!
//! - [`VivecoLoader`]: an [`AssetLoader`](bevy::asset::AssetLoader) for
//!   `.viveco` files, so they load through the
//!   [`AssetServer`](bevy::asset::AssetServer).
//! - [`VivecoPlugin`]: registers the loader and adds the systems that paint
//!   every [`VivecoInstance`] into the [`VivecoScene`] named by its
//!   [`VivecoSceneOf`] component.
//!
//! Everything is explicit: the plugin never spawns entities. Spawn the
//! [`VivecoScene`] entities your app needs and link instances with
//! [`VivecoSceneOf`]. Marking one scene [`VivecoDefaultScene`] opts into a
//! default target for instances that do not name a scene; without such a
//! marker, unlinked instances are ignored.
//!
//! # With the plugin
//!
//! Add bevy_vello's `VelloPlugin` and then [`VivecoPlugin`], spawn a scene, and
//! point one instance at it per rendered character:
//!
//! ```no_run
//! use bevy::prelude::*;
//! use bevy_vello::prelude::VelloView;
//! use bevy_vello::VelloPlugin;
//! use viveco::{Asset, VivecoInstance, VivecoPlugin, VivecoScene, VivecoSceneOf};
//!
//! fn main() {
//!     App::new()
//!         .add_plugins(DefaultPlugins)
//!         .add_plugins(VelloPlugin::default())
//!         .add_plugins(VivecoPlugin)
//!         .add_systems(Startup, setup)
//!         .run();
//! }
//!
//! fn setup(mut commands: Commands, asset_server: Res<AssetServer>) {
//!     commands.spawn((Camera2d, VelloView));
//!
//!     // Viveco does not create scenes: spawn one and link instances to it.
//!     let scene = commands.spawn(VivecoScene).id();
//!     let character = asset_server.load::<Asset>("character.viveco");
//!     commands.spawn((
//!         VivecoInstance::from_animation(character, 0),
//!         VivecoSceneOf(scene),
//!     ));
//! }
//! ```
//!
//! Every instance linked to the same scene has its artwork Z-ordered against
//! the other members, and its entity's
//! [`GlobalTransform`](bevy::prelude::GlobalTransform) is baked into scene
//! space. Spawn several scenes for independent rendering (for example, one per
//! camera or render layer); a scene's own transform applies to all its
//! contents.
//!
//! # Default scene
//!
//! Marking a scene with [`VivecoDefaultScene`] makes it the target for
//! instances that do not name one, so those instances need no [`VivecoSceneOf`]:
//!
//! ```no_run
//! # use bevy::prelude::*;
//! # use viveco::{Asset, VivecoDefaultScene, VivecoInstance, VivecoScene};
//! # fn spawn(commands: &mut Commands, asset: Handle<Asset>) {
//! commands.spawn((VivecoScene, VivecoDefaultScene));
//! commands.spawn(VivecoInstance::from_animation(asset, 0));
//! # }
//! ```
//!
//! Each instance is linked once; an explicit [`VivecoSceneOf`] always wins. If
//! several scenes carry the marker, the one with the lowest entity ID is used,
//! and with no marked scene unlinked instances are ignored. Removing the
//! default scene unlinks its members, which can then join a new default.
//!
//! # Stroke width
//!
//! [`VivecoInstance::stroke_width`] is explicit, in the asset's local units,
//! and painted like the rest of the geometry: scaling the instance entity, its
//! scene, or the camera scales the stroke along with the artwork. To keep a
//! constant on-screen width instead, add [`VivecoStrokeWidthInPixels`] to the
//! instance or to its scene. The plugin then rewrites each affected
//! instance's `stroke_width` to compensate for the instance's, the scene's,
//! and the observing camera's scales (a component on the instance overrides
//! the scene's):
//!
//! ```no_run
//! # use bevy::prelude::*;
//! # use viveco::{Asset, VivecoInstance, VivecoStrokeWidthInPixels, VivecoScene, VivecoSceneOf};
//! # fn spawn(commands: &mut Commands, asset: Handle<Asset>) {
//! let scene = commands
//!     .spawn((VivecoScene, VivecoStrokeWidthInPixels::new(2.0))) // two logical pixels wide
//!     .id();
//! commands.spawn((
//!     VivecoInstance::from_animation(asset, 0),
//!     VivecoSceneOf(scene),
//! ));
//! # }
//! ```
//!
//! Without the plugin, pass the width to
//! [`SceneInstance::with_stroke_width`](crate::render::SceneInstance::with_stroke_width)
//! yourself, scaling it however your app needs.
//!
//! # Without the plugin
//!
//! The plugin is only wiring, and it holds no state. Register the loader
//! directly if you do not want the rendering systems:
//!
//! ```no_run
//! # use bevy::prelude::*;
//! # use viveco::{Asset, VivecoLoader};
//! # let mut app = App::new();
//! app.init_asset::<Asset>()
//!     .init_asset_loader::<VivecoLoader>();
//! ```
//!
//! To drive rendering yourself, use the library calls the plugin's systems
//! are built from. A mixer becomes a [`VelloScene2d`](bevy_vello::prelude::VelloScene2d)
//! with [`SceneInstance`](crate::render::SceneInstance) and
//! [`SceneBuilder`](crate::render::SceneBuilder):
//!
//! ```
//! use bevy_vello::prelude::*;
//! use viveco::render::{SceneBuilder, SceneInstance};
//! use viveco::{AnimationInstance, Asset};
//!
//! fn paint(asset: &Asset, mixer: &[AnimationInstance]) -> VelloScene2d {
//!     let instance = SceneInstance::animated(asset, mixer);
//!     let mut builder = SceneBuilder::new();
//!     builder.add_instance(&instance);
//!     let mut scene = vello::Scene::new();
//!     builder.finish(&mut scene);
//!     VelloScene2d::from(scene)
//! }
//! ```
//!
//! Add one `SceneInstance` per instance (with its own
//! [`Affine`](vello::kurbo::Affine) placement and stroke width) to Z-order
//! several of them in one scene. For a single asset,
//! [`Asset::into_scene`](crate::Asset::into_scene) does the same in one call;
//! [`Asset::sample_pose`](crate::Asset::sample_pose) and
//! [`Asset::deformed`](crate::Asset::deformed) are the blending and skinning
//! steps underneath. Write the result to a
//! [`VelloScene2d`](bevy_vello::prelude::VelloScene2d) from your own system,
//! for example in `PostUpdate` after transform propagation, and run it when
//! the instance's mixer changes.
//!
//! [`VivecoScene`] and [`VivecoSceneOf`] are only bookkeeping for the plugin's
//! systems; the manual path does not need them.

mod loader;
mod plugin;

pub use loader::VivecoLoader;
pub use plugin::{
    VivecoDefaultScene, VivecoInstance, VivecoPlugin, VivecoScene, VivecoSceneMembers,
    VivecoSceneOf, VivecoStrokeWidthInPixels,
};

/// The Bevy integration's vocabulary.
///
/// Core asset types ([`Asset`](crate::Asset), [`AnimationInstance`](crate::AnimationInstance),
/// [`Pose`](crate::Pose), ...) stay at the crate root and are imported from
/// `viveco` directly.
pub mod prelude {
    pub use super::{
        VivecoDefaultScene, VivecoInstance, VivecoPlugin, VivecoScene, VivecoSceneOf,
        VivecoStrokeWidthInPixels,
    };
}
