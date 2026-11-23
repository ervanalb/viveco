//! Bevy integration for Viveco assets.
//!
//! This module is enabled by the crate's `bevy` feature. It provides three
//! independent pieces:
//!
//! - [`VivecoLoader`]: an [`AssetLoader`](bevy::asset::AssetLoader) for
//!   `.viveco` files, so they load through the
//!   [`AssetServer`](bevy::asset::AssetServer).
//! - [`VivecoPlugin`]: registers the loader, rebuilds every [`VivecoScene`]
//!   from [`SceneFragment`]s each frame, and contributes the artwork of every
//!   [`VivecoInstance`].
//! - [`ParallaxLayer`] and [`apply_parallax`]: a system that scales and offsets
//!   marked entities by depth, emulating a perspective camera.
//!
//! Everything is explicit: the plugin never spawns entities. A [`VivecoScene`]
//! is the root of a draw tree and a per-frame buffer of fragments. Every
//! drawable entity is a [`VivecoChildOf`] a scene or another node: a
//! [`VivecoInstance`] contributes its document, [`VivecoFragments`] contributes
//! custom Vello scenes, and either can nest inside shapes to any depth. Marking
//! one scene [`VivecoDefaultScene`] gives unparented drawable nodes a root;
//! without such a marker, they are ignored.
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
//! use viveco::{Asset, VivecoInstance, VivecoPlugin, VivecoScene, VivecoChildOf};
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
//!     let scene = commands.spawn(VivecoScene::default()).id();
//!     let character = asset_server.load::<Asset>("character.viveco");
//!     commands.spawn((
//!         VivecoInstance::from_animation(character, 0),
//!         VivecoChildOf::root(scene),
//!     ));
//! }
//! ```
//!
//! Every instance linked to the same scene has its artwork Z-ordered against
//! every other fragment in that scene, and its entity's
//! [`GlobalTransform`](bevy::prelude::GlobalTransform) is baked into scene
//! space. Spawn several scenes for independent rendering (for example, one per
//! camera or render layer); a scene's own transform applies to all its
//! contents.
//!
//! Besides the Bevy transform, an instance carries
//! [`VivecoInstance::transform`], a Vello [`Affine`](vello::kurbo::Affine)
//! applied to the asset first, in the asset's local Y-down space. Use it to
//! mirror or skew artwork, or to offset it without adding an entity:
//!
//! ```no_run
//! # use bevy::prelude::*;
//! # use viveco::{Asset, VivecoInstance, VivecoChildOf};
//! # use vello::kurbo::Affine;
//! # fn spawn(commands: &mut Commands, asset: Handle<Asset>, scene: Entity) {
//! commands.spawn((
//!     VivecoInstance::from_animation(asset, 0)
//!         .with_transform(Affine::scale(-1.0)), // mirror horizontally
//!     VivecoChildOf::root(scene),
//! ));
//! # }
//! ```
//!
//! # Custom scene content
//!
//! Custom Vello content is drawn by an entity with [`VivecoFragments`].
//! Producers fill it in [`VivecoSceneSystems::Build`]; the plugin consumes it in
//! the same walk as instance artwork and clears it each frame:
//!
//! ```no_run
//! use bevy::prelude::*;
//! use vello::kurbo::Affine;
//! use viveco::{SceneFragment, VivecoFragments, VivecoSceneSystems};
//!
//! #[derive(Component)]
//! struct Ring {
//!     radius: f32,
//! }
//!
//! fn draw_rings(mut rings: Query<(&Ring, &mut VivecoFragments)>) {
//!     for (ring, mut fragments) in &mut rings {
//!         let mut scene = vello::Scene::new();
//!         scene.stroke(
//!             &vello::kurbo::Stroke::new(1.0),
//!             Affine::IDENTITY,
//!             vello::peniko::Color::from_rgba8(255, 255, 255, 255),
//!             None,
//!             &vello::kurbo::Circle::new((0.0, 0.0), ring.radius as f64),
//!         );
//!         fragments.push(SceneFragment::new(0.0, scene));
//!     }
//! }
//!
//! fn main() {
//!     App::new()
//!         .add_systems(Startup, |mut commands: Commands| {
//!             commands.spawn(VivecoFragments::default());
//!         })
//!         .add_systems(PostUpdate, draw_rings.in_set(VivecoSceneSystems::Build));
//! }
//! ```
//!
//! Content is authored in the entity's local space and placed by the entity's
//! [`GlobalTransform`](bevy::prelude::GlobalTransform) — including any Bevy
//! [`ChildOf`](bevy::prelude::ChildOf) ancestors — in every context. The Viveco
//! link only selects the clip and paint level. [`VivecoFragments`] composes with
//! [`VivecoInstance`] on the same entity, so a document and custom overlays can
//! share a node.
//!
//! A fragment can target a shape of the entity's *own* document with
//! [`VivecoFragments::push_at_shape`]. It is clipped by that shape and
//! Z-ordered with its children, so an overlay can be drawn inside a region of a
//! pre-made asset without a separate entity:
//!
//! ```no_run
//! # use bevy::prelude::*;
//! # use viveco::{SceneFragment, VivecoFragments};
//! fn draw_scanlines(mut sources: Query<&mut VivecoFragments>) {
//!     // Resolve the shape once, e.g. with a generated index constant.
//!     let screen = 5usize;
//!     for mut source in &mut sources {
//!         let mut scene = vello::Scene::new();
//!         // Draw the scan-line pattern in the entity's local space.
//!         source.push_at_shape(screen, SceneFragment::new(1.0, scene));
//!     }
//! }
//! ```
//!
//! A system that prefers to push finished scene-space fragments can still query
//! `&mut VivecoScene` in [`VivecoSceneSystems::Build`] and call
//! [`VivecoScene::push`]; those fragments are sorted with everything else.
//! [`SceneBuilder::into_fragments`](crate::render::SceneBuilder::into_fragments)
//! and [`Shape::to_fragment`](crate::Shape::to_fragment) turn model geometry
//! into fragments if you prefer the asset styling and Z semantics.
//!
//! # Default scene
//!
//! Marking a scene with [`VivecoDefaultScene`] gives unparented drawable nodes a
//! root, so they need no [`VivecoChildOf`]:
//!
//! ```no_run
//! # use bevy::prelude::*;
//! # use viveco::{Asset, VivecoDefaultScene, VivecoInstance, VivecoScene};
//! # fn spawn(commands: &mut Commands, asset: Handle<Asset>) {
//! commands.spawn((VivecoScene::default(), VivecoDefaultScene));
//! commands.spawn(VivecoInstance::from_animation(asset, 0));
//! # }
//! ```
//!
//! A node is linked once; an explicit [`VivecoChildOf`] always wins. If several
//! scenes carry the marker, the one with the lowest entity ID is used, and with
//! no marked scene unparented nodes are ignored. Removing the default scene
//! unlinks its members, which can then join a new default.
//!
//! # Children
//!
//! [`VivecoChildOf`] attaches a node to another node's *root* or to one *shape*
//! of its document. Children nest to any depth:
//!
//! - [`VivecoChildOf::shape`] paints the child inside that shape's clip layer,
//!   competing with the shape's children by Z.
//! - [`VivecoChildOf::root`] paints the child at the parent's paint level,
//!   unclipped relative to it.
//!
//! ```no_run
//! # use bevy::prelude::*;
//! # use viveco::{Asset, VivecoChildOf, VivecoDefaultScene, VivecoInstance, VivecoScene};
//! # fn spawn(commands: &mut Commands, tv: Handle<Asset>, animation: Handle<Asset>) {
//! commands.spawn((VivecoScene::default(), VivecoDefaultScene));
//! let tv_entity = commands.spawn(VivecoInstance::from_asset(tv)).id();
//!
//! // Draw the animation inside the TV's shape named "Screen". Resolve the name
//! // once with `asset.shape_index_by_name` or a generated index constant.
//! let screen = 5usize;
//! commands.spawn((
//!     VivecoInstance::from_animation(animation, 0),
//!     VivecoChildOf::shape(tv_entity, screen),
//! ));
//! # }
//! ```
//!
//! Every node is placed by its own
//! [`GlobalTransform`](bevy::prelude::GlobalTransform) (including any Bevy
//! [`ChildOf`](bevy::prelude::ChildOf) ancestors) times its
//! [`VivecoInstance::transform`]. `VivecoChildOf` only selects the clip and
//! paint level; to make a child follow its host, make it a Bevy `ChildOf` it as
//! well. A node with no instance and no custom fragments groups its children.
//!
//! # Stroke width
//!
//! [`VivecoInstance::stroke_width`] is explicit, in the asset's local units,
//! and painted like the rest of the geometry: scaling the instance's transform,
//! the instance entity, its scene, or the camera scales the stroke along with
//! the artwork. To keep a constant on-screen width instead, add
//! [`VivecoStrokeWidthInPixels`] to the instance or to its scene. The plugin
//! then rewrites each affected instance's `stroke_width` to compensate for the
//! instance's transform and every placement above it, its scene's scale, and the
//! observing camera's (a component on the instance overrides the scene's):
//!
//! ```no_run
//! # use bevy::prelude::*;
//! # use viveco::{Asset, VivecoInstance, VivecoStrokeWidthInPixels, VivecoScene, VivecoChildOf};
//! # fn spawn(commands: &mut Commands, asset: Handle<Asset>) {
//! let scene = commands
//!     .spawn((VivecoScene::default(), VivecoStrokeWidthInPixels::new(2.0))) // two logical pixels wide
//!     .id();
//! commands.spawn((
//!     VivecoInstance::from_animation(asset, 0),
//!     VivecoChildOf::root(scene),
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
//! several of them in one scene. To nest documents, build a
//! [`DrawNode`](crate::render::DrawNode) tree with shape- and root-context children and
//! add it with `SceneBuilder::add_node`. A `SceneInstance` borrows its asset and
//! resolves the sample with [`Pose::to_world`](crate::Pose::to_world), so
//! skinning happens while geometry is drawn rather than by cloning the asset.
//! For a single unposed asset, [`Asset::into_scene`](crate::Asset::into_scene)
//! does the same in one call. The owned
//! [`Asset::deformed`](crate::Asset::deformed) form remains available when a
//! caller needs a deformed document (the editor's rig view, for example).
//! Write the result to a
//! [`VelloScene2d`](bevy_vello::prelude::VelloScene2d) from your own system,
//! for example in `PostUpdate` after transform propagation, and run it when
//! the instance's mixer changes.
//!
//! [`VivecoScene`] and [`VivecoChildOf`] are the Bevy plugin's scene and tree
//! wiring; the manual path does not need them.

mod loader;
mod parallax;
mod plugin;

// The asset model itself is Bevy-free (so it can be used from build scripts),
// so its Bevy trait implementations live here instead of on the type.
impl ::bevy::asset::Asset for crate::Asset {}

impl ::bevy::asset::VisitAssetDependencies for crate::Asset {
    fn visit_dependencies(&self, _visit: &mut impl FnMut(::bevy::asset::UntypedAssetId)) {}
}

impl ::bevy::reflect::TypePath for crate::Asset {
    fn type_path() -> &'static str {
        "viveco::asset::Asset"
    }

    fn short_type_path() -> &'static str {
        "Asset"
    }

    fn type_ident() -> Option<&'static str> {
        Some("Asset")
    }

    fn crate_name() -> Option<&'static str> {
        Some("viveco")
    }

    fn module_path() -> Option<&'static str> {
        Some("viveco::asset")
    }
}

pub use crate::render::SceneFragment;
pub use loader::VivecoLoader;
pub use parallax::{ParallaxLayer, apply_parallax};
pub use plugin::{
    VivecoChildOf, VivecoChildren, VivecoContext, VivecoDefaultScene, VivecoFragments,
    VivecoInstance, VivecoPlugin, VivecoScene, VivecoSceneSystems, VivecoStrokeWidthInPixels,
    scene_affine,
};

/// The Bevy integration's vocabulary.
///
/// Core asset types ([`Asset`](crate::Asset), [`AnimationInstance`](crate::AnimationInstance),
/// [`Pose`](crate::Pose), ...) stay at the crate root and are imported from
/// `viveco` directly.
pub mod prelude {
    pub use super::{
        ParallaxLayer, SceneFragment, VivecoChildOf, VivecoChildren, VivecoContext,
        VivecoDefaultScene, VivecoFragments, VivecoInstance, VivecoPlugin, VivecoScene,
        VivecoSceneSystems, VivecoStrokeWidthInPixels, apply_parallax, scene_affine,
    };
}
