//! The instance component, scene content, and the systems that render them.

use std::cmp::Ordering;

use ::bevy::camera::CameraUpdateSystems;
use ::bevy::camera::visibility::{NoFrustumCulling, RenderLayers, VisibilitySystems};
use ::bevy::prelude::*;
use ::bevy::transform::TransformSystems;
use ::bevy_vello::prelude::{VelloScene2d, VelloView};
use ::vello::Scene;
use ::vello::kurbo::Affine;

use crate::render::{SceneBuilder, SceneFragment, SceneInstance, ShapeAttachment};
use crate::{AnimationInstance, ShapeIndex};

use super::VivecoLoader;

/// Renders a loaded [`Asset`](crate::Asset) by blending animations at the
/// given times.
///
/// Each animation instance is sampled at its time, the poses are blended by
/// weight, and the resulting geometry is placed into the [`VivecoScene`] named
/// by this entity's [`VivecoSceneOf`] component. An instance without a
/// [`VivecoSceneOf`] joins the scene marked [`VivecoDefaultScene`], if any;
/// otherwise it is not rendered. [`VivecoPlugin`] never spawns a scene, so
/// spawn a [`VivecoScene`] yourself and link the instance to it (or mark it as
/// the default).
///
/// Add [`VivecoChildOf`] to draw the instance inside a shape of another
/// instance instead of into a scene of its own. A child is drawn through its
/// host and does not need [`VivecoSceneOf`].
///
/// ```
/// # use bevy::prelude::*;
/// # use viveco::{Asset, VivecoInstance, VivecoScene, VivecoSceneOf};
/// # fn spawn(commands: &mut Commands, asset: Handle<Asset>) {
/// let scene = commands.spawn(VivecoScene::default()).id();
/// commands.spawn((
///     VivecoInstance::from_animation(asset, 0),
///     VivecoSceneOf(scene),
/// ));
/// # }
/// ```
///
/// The entity's [`GlobalTransform`] is baked into scene space, and each
/// top-level shape is Z-ordered against every other fragment in the scene. Both
/// non-cyclic and cyclic animations use the sampling behavior of
/// [`Animation::sample_pose`](crate::Animation::sample_pose).
///
/// The [`transform`](Self::transform) field adds a Vello [`Affine`] of its own,
/// applied to the artwork before the entity's transform. It is expressed in the
/// asset's local Y-down space, so it can mirror, skew, or offset the asset
/// independently of the Bevy hierarchy above it.
///
/// Stroke widths are explicit and in the asset's local units: they are painted
/// like the rest of the geometry, so scaling the instance's transform, the
/// instance entity, its scene, or the camera scales the stroke along with the
/// artwork. Add [`VivecoStrokeWidthInPixels`] to keep an on-screen width
/// instead.
///
/// A user who wants a different pose or rendering path can write their own
/// producer system in [`VivecoSceneSystems::Build`] and push
/// [`SceneFragment`]s; the underlying steps are ordinary library calls.
#[derive(Component, Clone, Debug)]
#[require(Transform, Visibility)]
pub struct VivecoInstance {
    /// The loaded document to render.
    pub asset: Handle<crate::Asset>,
    /// The animation mixer: the instances to blend, as
    /// `(animation index, time in seconds, weight)`.
    pub mixer: Vec<AnimationInstance>,
    /// The Vello transform applied to the asset before the entity's
    /// [`GlobalTransform`].
    ///
    /// The artwork is drawn through `scene_affine(GlobalTransform) * transform`,
    /// so this is a placement in the asset's own Y-down space. The Bevy
    /// hierarchy still applies on top of it. Use it for transforms a Bevy
    /// [`Transform`] cannot express, such as mirroring or skewing, or to offset
    /// artwork without adding an entity.
    ///
    /// Defaults to [`Affine::IDENTITY`].
    pub transform: Affine,
    /// Stroke width in the asset's local units, used as-is.
    ///
    /// The painted width also scales with the instance entity, its scene, and
    /// any camera zoom. Set [`VivecoStrokeWidthInPixels`] on the instance or
    /// its scene to have the plugin maintain a constant on-screen width
    /// instead.
    pub stroke_width: f32,
}

impl VivecoInstance {
    /// Creates an instance for `asset` with the given mixer and a stroke width
    /// of `1.0`.
    pub fn new(asset: Handle<crate::Asset>, mixer: Vec<AnimationInstance>) -> Self {
        Self {
            asset,
            mixer,
            transform: Affine::IDENTITY,
            stroke_width: 1.0,
        }
    }

    /// Creates an instance that renders `asset` at rest, with no animations.
    ///
    /// This is the static path: the mixer is empty, so the plugin samples no
    /// animation and allocates no world pose. It is purely for ergonomics;
    /// [`VivecoInstance::new`] with an empty mixer behaves identically.
    pub fn from_asset(asset: Handle<crate::Asset>) -> Self {
        Self::new(asset, Vec::new())
    }

    /// Sets the Vello transform applied to the asset before the entity's
    /// [`GlobalTransform`].
    ///
    /// See [`transform`](Self::transform) for the coordinate space. The default
    /// is [`Affine::IDENTITY`].
    pub fn with_transform(mut self, transform: Affine) -> Self {
        self.transform = transform;
        self
    }

    /// Sets the stroke width, in the asset's local units.
    pub fn with_stroke_width(mut self, stroke_width: f32) -> Self {
        self.stroke_width = stroke_width;
        self
    }

    /// Creates an instance that plays only the animation at `index`.
    pub fn from_animation(asset: Handle<crate::Asset>, index: usize) -> Self {
        Self::new(asset, vec![(index, 0.0, 1.0)])
    }
}

/// Links a [`VivecoInstance`] to the [`VivecoScene`] it draws into.
///
/// A scene is created and populated explicitly:
///
/// ```
/// # use bevy::prelude::*;
/// # use viveco::{Asset, VivecoInstance, VivecoScene, VivecoSceneOf};
/// # fn spawn(commands: &mut Commands, asset: Handle<Asset>) {
/// let scene = commands.spawn(VivecoScene::default()).id();
/// commands.spawn((
///     VivecoInstance::from_animation(asset, 0),
///     VivecoSceneOf(scene),
/// ));
/// # }
/// ```
///
/// Point several instances at the same [`VivecoScene`] to Z-order their
/// artwork together, or at different scenes to render them independently (for
/// example, one per camera or render layer).
///
/// An instance without this component joins the scene marked
/// [`VivecoDefaultScene`], if one exists; otherwise it is not rendered. A
/// [`VivecoChildOf`] child is drawn through its host and does not need this
/// component.
///
/// This is a Bevy relationship: despawning the scene removes the component,
/// and the scene's [`VivecoSceneMembers`] stays up to date automatically.
#[derive(Component, Clone, Copy, PartialEq, Eq, Debug)]
#[relationship(relationship_target = VivecoSceneMembers)]
pub struct VivecoSceneOf(pub Entity);

/// The entities that draw into one [`VivecoScene`].
///
/// Maintained automatically by [`VivecoSceneOf`]. The fragment-based renderer
/// does not require it, but it stays up to date for inspection and cleanup.
#[derive(Component, Default, Debug)]
#[relationship_target(relationship = VivecoSceneOf)]
pub struct VivecoSceneMembers(Vec<Entity>);

/// Draws a [`VivecoInstance`]'s artwork inside one shape of another instance.
///
/// A child is not drawn as a top-level instance. Instead, the host's producer
/// paints it while drawing the host shape selected by
/// [`shape`](Self::shape): the child inherits that shape's clip layer and is
/// ordered among the shape's children by [`z`](Self::z). Its
/// [`VivecoInstance::transform`] places it in the host document's coordinate
/// space, so no other placement is applied. The child entity's Bevy
/// [`Transform`] is ignored: the child has no independent place in the scene,
/// so move it with [`VivecoInstance::transform`] instead.
///
/// Spawn the child with a [`VivecoInstance`] beside this component. The child
/// keeps its own asset and
/// [`mixer`](VivecoInstance::mixer), so it animates independently of the host:
///
/// ```
/// # use bevy::prelude::*;
/// # use viveco::{Asset, VivecoChildOf, VivecoInstance, VivecoScene, VivecoSceneOf};
/// # fn spawn(commands: &mut Commands, tv: Handle<Asset>, animation: Handle<Asset>) {
/// let scene = commands.spawn(VivecoScene::default()).id();
/// let tv_entity = commands
///     .spawn((VivecoInstance::from_asset(tv), VivecoSceneOf(scene)))
///     .id();
///
/// // Draw the animation inside the TV's shape named "Screen".
/// commands.spawn((
///     VivecoInstance::from_animation(animation, 0),
///     VivecoChildOf::new(tv_entity, "Screen"),
/// ));
/// # }
/// ```
///
/// A child should not also carry [`VivecoSceneOf`]; that component would name a
/// scene the child does not need. Children are drawn one level deep, so a
/// child's own [`VivecoChildren`] is ignored.
///
/// This is a Bevy relationship: the host's [`VivecoChildren`] stays up to date
/// automatically, and despawning the host unlinks its children without
/// despawning them.
#[derive(Component, Clone, Debug, PartialEq)]
#[relationship(relationship_target = VivecoChildren)]
pub struct VivecoChildOf {
    /// The instance whose asset contains the shape this child is drawn into.
    #[relationship]
    pub host: Entity,
    /// The shape of the host's asset to draw inside.
    pub shape: ShapeSelector,
    /// Sort key among the host shape's children and other children.
    ///
    /// Equal values paint the host shape's own children first, then children in
    /// the order they were added to the host.
    pub z: f32,
}

impl VivecoChildOf {
    /// Draws this entity's instance inside the shape chosen by `shape`.
    ///
    /// `shape` accepts a [`ShapeIndex`] or a shape name.
    pub fn new(host: Entity, shape: impl Into<ShapeSelector>) -> Self {
        Self {
            host,
            shape: shape.into(),
            z: 0.0,
        }
    }

    /// Sets the paint order among the host shape's children.
    pub fn with_z(mut self, z: f32) -> Self {
        self.z = z;
        self
    }
}

/// The [`VivecoChildOf`] children of a host instance.
///
/// Maintained automatically by [`VivecoChildOf`]. The plugin reads it to find
/// the artwork it draws into each host shape, and user systems can inspect it.
#[derive(Component, Default, Debug)]
#[relationship_target(relationship = VivecoChildOf)]
pub struct VivecoChildren(Vec<Entity>);

impl VivecoChildren {
    /// The child entities, in the order they were added.
    pub fn iter(&self) -> impl Iterator<Item = Entity> + '_ {
        self.0.iter().copied()
    }
}

/// Identifies one shape of a host asset for [`VivecoChildOf`].
///
/// Both variants are resolved against the host asset when it is rendered.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ShapeSelector {
    /// A depth-first pre-order shape index.
    Index(ShapeIndex),
    /// The first shape with this name, as found by
    /// [`Asset::shape_index_by_name`](crate::Asset::shape_index_by_name).
    ///
    /// Names are stable when shapes are reordered, so the constants generated
    /// by [`codegen`](crate::codegen) are the simplest way to name a shape.
    Name(String),
}

impl Default for ShapeSelector {
    fn default() -> Self {
        Self::Index(0)
    }
}

impl From<ShapeIndex> for ShapeSelector {
    fn from(index: ShapeIndex) -> Self {
        Self::Index(index)
    }
}

impl From<&str> for ShapeSelector {
    fn from(name: &str) -> Self {
        Self::Name(name.to_owned())
    }
}

impl From<String> for ShapeSelector {
    fn from(name: String) -> Self {
        Self::Name(name)
    }
}

/// An entity that carries one Vello scene.
///
/// Spawn one for every independently rendered scene. Spawning it also adds the
/// [`VelloScene2d`] the plugin paints into:
///
/// ```
/// # use bevy::prelude::*;
/// # use viveco::VivecoScene;
/// # fn spawn(commands: &mut Commands) -> Entity {
/// commands.spawn(VivecoScene::default()).id()
/// # }
/// ```
///
/// The scene's content is rebuilt every frame from [`SceneFragment`]s. The
/// plugin's default producer contributes the artwork of every [`VivecoInstance`]
/// linked to this scene; your own systems add fragments by querying `&mut
/// VivecoScene` and running in [`VivecoSceneSystems::Build`].
///
/// Fragments are sorted by Z across all producers before the scene is painted,
/// so custom geometry can be interleaved with instance artwork at any depth.
/// Viveco never spawns a scene on its own.
///
/// Leave the entity's transform at its default to display each fragment at its
/// own Bevy transform. A transform here applies to the whole scene.
#[derive(Component, Default)]
#[require(VelloScene2d, NoFrustumCulling)]
pub struct VivecoScene {
    fragments: Vec<SceneFragment>,
}

impl VivecoScene {
    /// Appends one fragment. Fragments are sorted by Z at the end of the frame.
    pub fn push(&mut self, fragment: SceneFragment) {
        self.fragments.push(fragment);
    }

    /// Appends every fragment from `fragments`.
    pub fn push_fragments(&mut self, fragments: impl IntoIterator<Item = SceneFragment>) {
        self.fragments.extend(fragments);
    }

    /// The fragments queued for this frame, before Z sorting.
    pub fn fragments(&self) -> &[SceneFragment] {
        &self.fragments
    }
}

/// The `PostUpdate` stages of the fragment-based scene pipeline.
///
/// Every frame the plugin clears every [`VivecoScene`] in [`Clear`](Self::Clear),
/// lets producers add fragments in [`Build`](Self::Build), then sorts and
/// paints them in [`Finalize`](Self::Finalize). Custom producer systems belong
/// in [`Build`](Self::Build).
#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub enum VivecoSceneSystems {
    /// Clears every scene's fragment list. Plugin-internal.
    Clear,
    /// Producers push fragments into scenes.
    Build,
    /// Sorts fragments by Z and paints the Vello scene. Plugin-internal.
    Finalize,
}

/// Marks the [`VivecoScene`] that instances without [`VivecoSceneOf`] join.
///
/// Add this component to one [`VivecoScene`] to opt into a default scene. The
/// plugin then links every [`VivecoInstance`] that does not name a scene to it,
/// so instances can be spawned without picking a scene:
///
/// ```
/// # use bevy::prelude::*;
/// # use viveco::{Asset, VivecoDefaultScene, VivecoInstance, VivecoScene};
/// # fn spawn(commands: &mut Commands, asset: Handle<Asset>) {
/// commands.spawn((VivecoScene::default(), VivecoDefaultScene));
/// commands.spawn(VivecoInstance::from_animation(asset, 0));
/// # }
/// ```
///
/// Linking happens once per instance; an explicit [`VivecoSceneOf`] always
/// wins. If several scenes are marked, the one with the lowest entity ID is
/// used. With no marked scene, instances without [`VivecoSceneOf`] are not
/// rendered. Custom producer systems can query this marker to target the same
/// scene.
#[derive(Component, Default)]
pub struct VivecoDefaultScene;

/// Keeps strokes at a fixed size on screen.
///
/// Attach this to a [`VivecoInstance`], or to its [`VivecoScene`], to have the
/// plugin rewrite the instance's
/// [`stroke_width`](VivecoInstance::stroke_width) so painted strokes keep
/// `width` logical pixels. Every scale that affects the painted size is
/// compensated: the instance's [`transform`](VivecoInstance::transform), the
/// instance entity, its scene, and the observing camera. A component on the
/// instance overrides one on its scene.
///
/// The camera factor comes from the `Camera2d` with `VelloView` whose
/// `RenderLayers` overlap the scene's. If several cameras match, the most
/// zoomed-in one wins; if none match, only the entity and scene scales are
/// compensated. Without this component the instance's `stroke_width` is used
/// exactly as authored.
///
/// ```no_run
/// # use bevy::prelude::*;
/// # use viveco::{Asset, VivecoInstance, VivecoStrokeWidthInPixels, VivecoScene, VivecoSceneOf};
/// # fn spawn(commands: &mut Commands, asset: Handle<Asset>) {
/// let scene = commands
///     .spawn((VivecoScene::default(), VivecoStrokeWidthInPixels::new(2.0)))
///     .id();
/// commands.spawn((
///     VivecoInstance::from_animation(asset, 0),
///     VivecoSceneOf(scene),
/// ));
/// # }
/// ```
#[derive(Component, Clone, Copy, Debug)]
pub struct VivecoStrokeWidthInPixels {
    /// The target stroke width in logical pixels.
    pub width: f32,
}

impl VivecoStrokeWidthInPixels {
    /// Targets `width` logical pixels.
    pub fn new(width: f32) -> Self {
        Self { width }
    }
}

impl Default for VivecoStrokeWidthInPixels {
    /// Targets one logical pixel.
    fn default() -> Self {
        Self::new(1.0)
    }
}

/// Registers the asset loader and the fragment-based scene pipeline.
///
/// The plugin owns no entities or resources of its own. It only:
///
/// - registers [`VivecoLoader`](super::VivecoLoader) so `.viveco` files load
///   through the [`AssetServer`](::bevy::asset::AssetServer),
/// - clears every [`VivecoScene`] each frame in
///   [`VivecoSceneSystems::Clear`],
/// - contributes the artwork of every [`VivecoInstance`] to its scene (or the
///   [`VivecoDefaultScene`]) in [`VivecoSceneSystems::Build`], including any
///   [`VivecoChildOf`] children inside their host shapes, and
/// - sorts the collected fragments by Z and paints them into the scene's
///   [`VelloScene2d`] in [`VivecoSceneSystems::Finalize`].
///
/// Custom producers are ordinary systems in [`VivecoSceneSystems::Build`].
/// Add the plugin after bevy_vello's `VelloPlugin`, then spawn your scenes and
/// instances yourself. See the [module docs](crate::bevy) for worked examples
/// and for the equivalent code without the plugin.
pub struct VivecoPlugin;

impl Plugin for VivecoPlugin {
    fn build(&self, app: &mut App) {
        app.init_asset::<crate::Asset>()
            .init_asset_loader::<VivecoLoader>()
            .configure_sets(
                PostUpdate,
                (
                    VivecoSceneSystems::Clear,
                    VivecoSceneSystems::Build,
                    VivecoSceneSystems::Finalize,
                )
                    .chain()
                    .after(TransformSystems::Propagate)
                    .after(VisibilitySystems::VisibilityPropagate)
                    .after(CameraUpdateSystems),
            )
            // Linking and stroke-width resolution must finish before producers
            // read the instances. `chain` flushes the relationship insertion.
            .add_systems(
                PostUpdate,
                (attach_default_scene, normalize_viveco_stroke_widths)
                    .chain()
                    .before(VivecoSceneSystems::Clear)
                    .after(TransformSystems::Propagate)
                    .after(VisibilitySystems::VisibilityPropagate)
                    .after(CameraUpdateSystems),
            )
            .add_systems(
                PostUpdate,
                (
                    clear_viveco_scenes.in_set(VivecoSceneSystems::Clear),
                    render_viveco_instances.in_set(VivecoSceneSystems::Build),
                    finalize_viveco_scenes.in_set(VivecoSceneSystems::Finalize),
                ),
            );
    }
}

/// Links instances that do not name a scene to the [`VivecoDefaultScene`].
///
/// An instance is linked once; afterwards the `Without<VivecoSceneOf>` query
/// matches nothing. The system returns immediately when no scene is marked, so
/// its steady-state cost is an empty query scan. `chain` inserts a
/// deferred-command sync point before the scene update, so a newly linked
/// instance renders in the same frame. [`VivecoChildOf`] children are skipped:
/// they are drawn through their host, not into a scene of their own.
#[allow(clippy::type_complexity)]
fn attach_default_scene(
    mut commands: Commands,
    defaults: Query<Entity, With<VivecoDefaultScene>>,
    unlinked: Query<
        Entity,
        (
            With<VivecoInstance>,
            Without<VivecoSceneOf>,
            Without<VivecoChildOf>,
        ),
    >,
) {
    // Pick the lowest index so the choice is deterministic if more than one
    // scene is marked. (`Entity`'s own ordering is not index order.)
    let Some(scene) = defaults.iter().min_by_key(|entity| entity.index_u32()) else {
        return;
    };
    for entity in &unlinked {
        commands.entity(entity).insert(VivecoSceneOf(scene));
    }
}

/// Keeps instances with [`VivecoStrokeWidthInPixels`] at a constant on-screen
/// stroke width.
///
/// The painted width scales with the instance's Vello transform, the instance
/// entity, its scene, and the observing camera, so this divides the target
/// width by all of those factors. The component can live on the instance or on
/// its scene; the instance's value wins. The width is written only when it
/// changes, so a static zoom does not mark the instance changed every frame.
///
/// Children are compensated through the same host placement they are painted
/// with, so a child of a scaled instance also keeps its width on screen. A
/// child uses its own component when it has one, and otherwise inherits its
/// host scene's.
#[allow(clippy::type_complexity)]
fn normalize_viveco_stroke_widths(
    mut instances: Query<(
        &mut VivecoInstance,
        &VivecoSceneOf,
        &GlobalTransform,
        Option<&VivecoStrokeWidthInPixels>,
        Option<&VivecoChildren>,
    )>,
    mut children: Query<
        (&mut VivecoInstance, Option<&VivecoStrokeWidthInPixels>),
        (With<VivecoChildOf>, Without<VivecoSceneOf>),
    >,
    scenes: Query<
        (
            &GlobalTransform,
            Option<&RenderLayers>,
            Option<&VivecoStrokeWidthInPixels>,
        ),
        With<VivecoScene>,
    >,
    cameras: Query<
        (
            &Camera,
            &Projection,
            &GlobalTransform,
            Option<&RenderLayers>,
        ),
        (With<Camera2d>, With<VelloView>),
    >,
) {
    for (mut instance, scene_of, transform, pixels, host_children) in &mut instances {
        let Ok((scene_transform, scene_layers, scene_pixels)) = scenes.get(scene_of.0) else {
            continue;
        };

        // bevy_vello renders a scene only for cameras whose render layers
        // overlap the scene's, so use the same rule here.
        let scene_layers = scene_layers.cloned().unwrap_or_default();
        let mut pixels_per_unit = 0.0_f32;
        for (camera, projection, camera_transform, camera_layers) in &cameras {
            let camera_layers = camera_layers.cloned().unwrap_or_default();
            if scene_layers.intersects(&camera_layers) {
                pixels_per_unit = pixels_per_unit.max(camera_pixels_per_unit(
                    camera,
                    projection,
                    camera_transform,
                ));
            }
        }
        // With no matching camera the entity and scene scales are still
        // compensated; assume an unzoomed camera.
        if pixels_per_unit <= 0.0 {
            pixels_per_unit = 1.0;
        }

        let instance_scale = max_scale(transform) * max_affine_scale(&instance.transform);
        let scene_scale = max_scale(scene_transform);

        if let Some(pixels) = pixels.or(scene_pixels) {
            let world_scale = (instance_scale * scene_scale * pixels_per_unit).max(f32::EPSILON);
            let width = pixels.width / world_scale;
            if instance.stroke_width != width {
                instance.stroke_width = width;
            }
        }

        let Some(host_children) = host_children else {
            continue;
        };
        for child_entity in host_children.iter() {
            let Ok((mut child, child_pixels)) = children.get_mut(child_entity) else {
                continue;
            };
            let Some(child_pixels) = child_pixels.or(scene_pixels) else {
                continue;
            };
            let child_scale = instance_scale * max_affine_scale(&child.transform);
            let world_scale = (child_scale * scene_scale * pixels_per_unit).max(f32::EPSILON);
            let width = child_pixels.width / world_scale;
            if child.stroke_width != width {
                child.stroke_width = width;
            }
        }
    }
}

/// Pixels per world unit for one 2D camera, combining its transform scale and
/// its projection's zoom.
fn camera_pixels_per_unit(
    camera: &Camera,
    projection: &Projection,
    transform: &GlobalTransform,
) -> f32 {
    let Some(viewport) = camera.logical_viewport_size() else {
        return 1.0;
    };
    // `clip_from_view` maps view space to NDC; NDC spans two units across the
    // viewport, so this is the view-to-pixel scale. The camera transform then
    // scales world units into view units.
    let view_to_pixels = projection.get_clip_from_view().x_axis.x.abs() * viewport.x * 0.5;
    (view_to_pixels / max_scale(transform)).max(f32::EPSILON)
}

/// The larger of a transform's absolute X and Y scales.
fn max_scale(transform: &GlobalTransform) -> f32 {
    let scale = transform.scale();
    scale.x.abs().max(scale.y.abs())
}

/// The largest factor by which an affine stretches geometry.
///
/// This is the largest singular value of the affine's linear part, so it is
/// the scale that strokes painted through it inherit even when the transform
/// combines rotation, shear, and non-uniform scale.
fn max_affine_scale(affine: &Affine) -> f32 {
    let [a, b, c, d, ..] = affine.as_coeffs();
    // The singular values of `A` are the square roots of the eigenvalues of
    // `AᵀA`, whose 2x2 entries are gathered here.
    let (m00, m01, m11) = (a * a + c * c, a * b + c * d, b * b + d * d);
    let half_trace = (m00 + m11) * 0.5;
    let half_diff = (m00 - m11) * 0.5;
    let largest_eigenvalue = half_trace + half_diff.hypot(m01);
    largest_eigenvalue.max(0.0).sqrt() as f32
}

/// Empties every scene's fragment list at the start of the frame.
fn clear_viveco_scenes(mut scenes: Query<&mut VivecoScene>) {
    for mut scene in &mut scenes {
        scene.fragments.clear();
    }
}

/// Contributes the artwork of every linked [`VivecoInstance`] to its scene.
///
/// Each top-level shape becomes one fragment carrying that shape's Z, so
/// instance artwork interleaves with fragments from other producers. Any
/// [`VivecoChildOf`] children are attached to their host shapes, so they are
/// painted inside the host's fragment with the host shape's clip.
#[allow(clippy::type_complexity)]
fn render_viveco_instances(
    mut scenes: Query<&mut VivecoScene>,
    instances: Query<
        (
            &VivecoInstance,
            &VivecoSceneOf,
            &GlobalTransform,
            &InheritedVisibility,
            Option<&VivecoChildren>,
        ),
        Without<VivecoChildOf>,
    >,
    children: Query<(&VivecoInstance, &VivecoChildOf, &InheritedVisibility)>,
    assets: Res<Assets<crate::Asset>>,
) {
    for (instance, scene_of, transform, visibility, host_children) in &instances {
        if !visibility.get() || max_scale(transform) <= f32::EPSILON {
            continue;
        }
        let Some(asset) = assets.get(&instance.asset) else {
            continue;
        };
        let Ok(mut scene) = scenes.get_mut(scene_of.0) else {
            continue;
        };

        // The instance's own affine applies in the asset's local space, before
        // the entity's transform places it in the scene.
        let placement = scene_affine(transform) * instance.transform;
        let posed = SceneInstance::animated(asset, &instance.mixer)
            .with_transform(placement)
            .with_stroke_width(instance.stroke_width);
        let mut builder = SceneBuilder::new();
        builder.add_instance(&posed);

        if let Some(host_children) = host_children {
            for child_entity in host_children.iter() {
                let Ok((child, child_of, child_visibility)) = children.get(child_entity) else {
                    continue;
                };
                if !child_visibility.get() {
                    continue;
                }
                let Some(child_asset) = assets.get(&child.asset) else {
                    continue;
                };
                let Some(shape) = resolve_shape(asset, &child_of.shape) else {
                    ::bevy::log::warn!(
                        "VivecoChildOf on {child_entity:?} names a shape that does not exist \
                         in its host's asset; the child is not drawn"
                    );
                    continue;
                };
                let child_posed = SceneInstance::animated(child_asset, &child.mixer)
                    .with_transform(child.transform)
                    .with_stroke_width(child.stroke_width);
                builder.add_attachment(ShapeAttachment {
                    shape,
                    z: child_of.z,
                    instance: child_posed,
                });
            }
        }

        scene.push_fragments(builder.into_fragments());
    }
}

/// Resolves a [`ShapeSelector`] against a loaded asset.
fn resolve_shape(asset: &crate::Asset, selector: &ShapeSelector) -> Option<ShapeIndex> {
    match selector {
        ShapeSelector::Index(index) => (*index < asset.shape_count()).then_some(*index),
        ShapeSelector::Name(name) => asset.shape_index_by_name(name),
    }
}

/// Sorts every scene's fragments by Z and paints them into its [`VelloScene2d`].
fn finalize_viveco_scenes(mut scenes: Query<(&mut VivecoScene, &mut VelloScene2d)>) {
    for (mut scene, mut vello) in &mut scenes {
        // Stable: fragments with equal Z keep the order they were pushed.
        scene
            .fragments
            .sort_by(|a, b| a.z.partial_cmp(&b.z).unwrap_or(Ordering::Equal));

        let mut painted = Scene::new();
        for fragment in &scene.fragments {
            painted.append(&fragment.scene, None);
        }
        *vello = VelloScene2d::from(painted);
    }
}

/// Converts a Bevy world transform into scene space.
///
/// Use this to place custom fragments: build geometry in the entity's local
/// space, then store this affine in the encoded geometry or apply it when
/// drawing.
///
/// Scene geometry is Y-down, while Bevy's world is Y-up. `bevy_vello` applies
/// that conversion itself when it places a [`VelloScene2d`]; for a transform
/// baked into the geometry the conversion conjugates the transform by a Y flip,
/// which negates the off-diagonal terms and the translation's Y.
///
/// This matches the per-entity path for an unrotated 2D camera. A rolled
/// camera's screen transform cannot be reproduced from an entity transform
/// alone, because it would depend on the camera.
pub fn scene_affine(transform: &GlobalTransform) -> Affine {
    let matrix = transform.to_matrix();
    Affine::new([
        f64::from(matrix.x_axis.x),
        -f64::from(matrix.x_axis.y),
        -f64::from(matrix.y_axis.x),
        f64::from(matrix.y_axis.y),
        f64::from(matrix.w_axis.x),
        -f64::from(matrix.w_axis.y),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Path as VivecoPath, Rgba8, Shape, Style};

    fn test_app() -> App {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, AssetPlugin::default(), TransformPlugin))
            .add_plugins(VivecoPlugin);
        app
    }

    fn filled_shape() -> Shape {
        Shape {
            paths: vec![VivecoPath::new([0.0, 0.0]).line_to([10.0, 10.0])],
            style: Style {
                fill: Some(Rgba8::rgb(255, 0, 0)),
                stroke: None,
            },
            ..Default::default()
        }
    }

    fn add_asset(app: &mut App) -> Handle<crate::Asset> {
        app.world_mut()
            .resource_mut::<Assets<crate::Asset>>()
            .add(crate::Asset {
                shapes: vec![filled_shape()],
                ..Default::default()
            })
    }

    fn spawn_scene(app: &mut App) -> Entity {
        app.world_mut().spawn(VivecoScene::default()).id()
    }

    fn spawn_instance(
        app: &mut App,
        scene: Entity,
        asset: Handle<crate::Asset>,
        transform: Transform,
    ) -> Entity {
        app.world_mut()
            .spawn((
                VivecoInstance::new(asset, Vec::new()),
                VivecoSceneOf(scene),
                transform,
                InheritedVisibility::VISIBLE,
            ))
            .id()
    }

    fn scene_path_counts(app: &mut App) -> Vec<u32> {
        let mut query = app.world_mut().query::<&VelloScene2d>();
        let mut counts: Vec<u32> = query
            .iter(app.world())
            .map(|scene| scene.encoding().n_paths)
            .collect();
        counts.sort_unstable();
        counts
    }

    fn scene_path_count(app: &mut App, entity: Entity) -> u32 {
        let mut query = app.world_mut().query::<&VelloScene2d>();
        query
            .get(app.world(), entity)
            .map_or(0, |scene| scene.encoding().n_paths)
    }

    fn spawn_unlinked_instance(app: &mut App, asset: Handle<crate::Asset>) -> Entity {
        app.world_mut()
            .spawn((
                VivecoInstance::new(asset, Vec::new()),
                Transform::default(),
                InheritedVisibility::VISIBLE,
            ))
            .id()
    }

    /// A stand-in for a user's custom producer: draws a line at `z`.
    #[derive(Component)]
    struct TestFragment {
        z: f32,
    }

    fn push_test_fragments(
        mut scenes: Query<&mut VivecoScene>,
        fragments: Query<(&TestFragment, &VivecoSceneOf, &GlobalTransform)>,
    ) {
        for (spec, scene_of, transform) in &fragments {
            let Ok(mut scene) = scenes.get_mut(scene_of.0) else {
                continue;
            };
            let mut fragment = Scene::new();
            fragment.stroke(
                &vello::kurbo::Stroke::new(1.0),
                scene_affine(transform),
                vello::peniko::Color::from_rgba8(255, 255, 255, 255),
                None,
                &vello::kurbo::Line::new((0.0, 0.0), (10.0, 0.0)),
            );
            scene.push(SceneFragment {
                z: spec.z,
                scene: fragment,
            });
        }
    }

    fn push_one_fragment_once(mut scenes: Query<&mut VivecoScene>, mut pushed: Local<bool>) {
        if *pushed {
            return;
        }
        for mut scene in &mut scenes {
            let mut fragment = Scene::new();
            fragment.fill(
                vello::peniko::Fill::NonZero,
                Affine::IDENTITY,
                vello::peniko::Color::from_rgba8(255, 255, 255, 255),
                None,
                &vello::kurbo::Rect::new(0.0, 0.0, 1.0, 1.0),
            );
            scene.push(SceneFragment {
                z: 0.0,
                scene: fragment,
            });
            *pushed = true;
        }
    }

    #[test]
    fn custom_fragments_interleave_with_instance_shapes_by_z() {
        let mut app = test_app();
        app.add_systems(
            PostUpdate,
            push_test_fragments.in_set(VivecoSceneSystems::Build),
        );
        let scene = spawn_scene(&mut app);
        let asset = add_asset(&mut app);

        // The instance's shape has Z = 0; the custom fragment goes behind it.
        spawn_instance(&mut app, scene, asset, Transform::from_xyz(10.0, 0.0, 0.0));
        app.world_mut().spawn((
            TestFragment { z: -1.0 },
            VivecoSceneOf(scene),
            Transform::from_xyz(5.0, 0.0, 0.0),
        ));

        app.update();

        let mut query = app.world_mut().query::<&VelloScene2d>();
        let vello = query.get(app.world(), scene).unwrap();
        assert_eq!(vello.encoding().n_paths, 2);
        let translations: Vec<[f32; 2]> = vello
            .encoding()
            .transforms
            .iter()
            .map(|transform| transform.translation)
            .collect();
        assert_eq!(translations, vec![[5.0, 0.0], [10.0, 0.0]]);
    }

    #[test]
    fn fragments_do_not_persist_across_frames() {
        let mut app = test_app();
        app.add_systems(
            PostUpdate,
            push_one_fragment_once.in_set(VivecoSceneSystems::Build),
        );
        let scene = spawn_scene(&mut app);
        app.update();
        assert_eq!(scene_path_count(&mut app, scene), 1);

        app.update();
        assert_eq!(scene_path_count(&mut app, scene), 0);
    }

    #[test]
    fn one_scene_combines_every_instance() {
        let mut app = test_app();
        let scene = spawn_scene(&mut app);
        let asset = add_asset(&mut app);
        spawn_instance(
            &mut app,
            scene,
            asset.clone(),
            Transform::from_xyz(5.0, 6.0, 0.0),
        );
        spawn_instance(&mut app, scene, asset, Transform::default());

        app.update();

        assert_eq!(scene_path_counts(&mut app), vec![2]);
        let mut query = app.world_mut().query::<&VelloScene2d>();
        let scene = query.iter(app.world()).next().unwrap();
        assert!(
            scene
                .encoding()
                .transforms
                .iter()
                .any(|transform| transform.translation == [5.0, -6.0]),
            "the entity transform is baked into scene space"
        );
    }

    #[test]
    fn instances_can_target_their_own_scene() {
        let mut app = test_app();
        let first = spawn_scene(&mut app);
        let asset = add_asset(&mut app);
        spawn_instance(&mut app, first, asset, Transform::default());
        app.update();

        // A second scene with a document that has two shapes, so the counts
        // show which members landed in which scene.
        let two_shapes = app
            .world_mut()
            .resource_mut::<Assets<crate::Asset>>()
            .add(crate::Asset {
                shapes: vec![filled_shape(), filled_shape()],
                ..Default::default()
            });
        let second = spawn_scene(&mut app);
        spawn_instance(&mut app, second, two_shapes, Transform::default());
        app.update();

        assert_eq!(scene_path_counts(&mut app), vec![1, 2]);
    }

    #[test]
    fn an_unlinked_instance_is_not_rendered_without_a_default_scene() {
        let mut app = test_app();
        let scene = spawn_scene(&mut app);
        let asset = add_asset(&mut app);
        spawn_unlinked_instance(&mut app, asset);

        app.update();

        // The scene exists but stays empty; nothing was assigned implicitly.
        assert_eq!(scene_path_counts(&mut app), vec![0]);
        assert_eq!(scene_path_count(&mut app, scene), 0);
    }

    #[test]
    fn a_default_scene_renders_unlinked_instances() {
        let mut app = test_app();
        let scene = app
            .world_mut()
            .spawn((VivecoScene::default(), VivecoDefaultScene))
            .id();
        let asset = add_asset(&mut app);
        let instance = spawn_unlinked_instance(&mut app, asset);

        app.update();

        assert_eq!(scene_path_count(&mut app, scene), 1);
        // The link is real: it happened once and is now a normal relationship.
        assert_eq!(
            app.world().get::<VivecoSceneOf>(instance),
            Some(&VivecoSceneOf(scene))
        );
    }

    #[test]
    fn an_explicit_scene_wins_over_the_default() {
        let mut app = test_app();
        let default = app
            .world_mut()
            .spawn((VivecoScene::default(), VivecoDefaultScene))
            .id();
        let explicit = spawn_scene(&mut app);
        let asset = add_asset(&mut app);
        spawn_instance(&mut app, explicit, asset, Transform::default());

        app.update();

        assert_eq!(scene_path_count(&mut app, explicit), 1);
        assert_eq!(scene_path_count(&mut app, default), 0);
    }

    #[test]
    fn members_rejoin_a_replacement_default_scene() {
        let mut app = test_app();
        let old_default = app
            .world_mut()
            .spawn((VivecoScene::default(), VivecoDefaultScene))
            .id();
        let asset = add_asset(&mut app);
        let instance = spawn_unlinked_instance(&mut app, asset);
        app.update();
        assert_eq!(scene_path_count(&mut app, old_default), 1);

        // Despawning the scene removes `VivecoSceneOf` from its members.
        app.world_mut().entity_mut(old_default).despawn();
        app.update();
        assert!(scene_path_counts(&mut app).is_empty());
        assert!(app.world().get::<VivecoSceneOf>(instance).is_none());

        let replacement = app
            .world_mut()
            .spawn((VivecoScene::default(), VivecoDefaultScene))
            .id();
        app.update();

        assert_eq!(scene_path_count(&mut app, replacement), 1);
        assert_eq!(
            app.world().get::<VivecoSceneOf>(instance),
            Some(&VivecoSceneOf(replacement))
        );
    }

    #[test]
    fn the_lowest_marked_scene_is_the_default() {
        let mut app = test_app();
        let first = app
            .world_mut()
            .spawn((VivecoScene::default(), VivecoDefaultScene))
            .id();
        let second = app
            .world_mut()
            .spawn((VivecoScene::default(), VivecoDefaultScene))
            .id();
        let asset = add_asset(&mut app);
        spawn_unlinked_instance(&mut app, asset);

        app.update();

        assert_eq!(scene_path_count(&mut app, first), 1);
        assert_eq!(scene_path_count(&mut app, second), 0);
    }

    #[test]
    fn stroke_width_in_pixels_compensates_entity_and_scene_scale() {
        let mut app = test_app();
        let scene = app
            .world_mut()
            .spawn((
                VivecoScene::default(),
                Transform::default().with_scale(Vec3::splat(2.0)),
            ))
            .id();
        let asset = add_asset(&mut app);
        let entity = app
            .world_mut()
            .spawn((
                VivecoInstance::new(asset, Vec::new()),
                VivecoSceneOf(scene),
                VivecoStrokeWidthInPixels::new(4.0),
                Transform::default().with_scale(Vec3::splat(2.0)),
                InheritedVisibility::VISIBLE,
            ))
            .id();

        app.update();

        // With no camera, only the entity and scene scales apply: 4 / (2 * 2).
        let instance = app.world().get::<VivecoInstance>(entity).unwrap();
        assert!((instance.stroke_width - 1.0).abs() < 1e-6);
    }

    #[test]
    fn stroke_width_in_pixels_on_the_scene_applies_to_its_members() {
        let mut app = test_app();
        let scene = app
            .world_mut()
            .spawn((
                VivecoScene::default(),
                VivecoStrokeWidthInPixels::new(6.0),
                Transform::default().with_scale(Vec3::splat(3.0)),
            ))
            .id();
        let asset = add_asset(&mut app);
        let transform = Transform::default().with_scale(Vec3::splat(2.0));
        let member = app
            .world_mut()
            .spawn((
                VivecoInstance::new(asset.clone(), Vec::new()),
                VivecoSceneOf(scene),
                transform,
                InheritedVisibility::VISIBLE,
            ))
            .id();
        // An instance-level component overrides the scene's.
        let overridden = app
            .world_mut()
            .spawn((
                VivecoInstance::new(asset, Vec::new()),
                VivecoSceneOf(scene),
                VivecoStrokeWidthInPixels::new(12.0),
                transform,
                InheritedVisibility::VISIBLE,
            ))
            .id();

        app.update();

        // 6 / (2 * 3) from the scene, and 12 / (2 * 3) from the override.
        let world = app.world();
        let member_instance = world.get::<VivecoInstance>(member).unwrap();
        assert!((member_instance.stroke_width - 1.0).abs() < 1e-6);
        let overridden_instance = world.get::<VivecoInstance>(overridden).unwrap();
        assert!((overridden_instance.stroke_width - 2.0).abs() < 1e-6);
    }

    /// Builds a 2D camera at the given viewport size and returns the pixels
    /// per world unit at that camera's scale.
    fn camera_pixels_at(viewport: f32, projection_scale: f32, camera_scale: f32) -> f32 {
        use ::bevy::camera::{CameraProjection, ComputedCameraValues, RenderTargetInfo, Viewport};

        let size = UVec2::splat(viewport as u32);
        let camera = Camera {
            viewport: Some(Viewport {
                physical_size: size,
                ..default()
            }),
            computed: ComputedCameraValues {
                target_info: Some(RenderTargetInfo {
                    physical_size: size,
                    scale_factor: 1.0,
                }),
                ..default()
            },
            ..default()
        };
        let mut projection = OrthographicProjection::default_2d();
        projection.scale = projection_scale;
        let logical = size.as_vec2();
        projection.update(logical.x, logical.y);
        let transform =
            GlobalTransform::from(Transform::default().with_scale(Vec3::splat(camera_scale)));

        camera_pixels_per_unit(&camera, &Projection::Orthographic(projection), &transform)
    }

    #[test]
    fn camera_scale_and_projection_zoom_both_count() {
        // Window-sized ortho projection: one world unit is one logical pixel.
        assert!((camera_pixels_at(400.0, 1.0, 1.0) - 1.0).abs() < 1e-5);
        // Projection scale 2 zooms out, halving the pixels per world unit.
        assert!((camera_pixels_at(400.0, 2.0, 1.0) - 0.5).abs() < 1e-5);
        // Scaling the camera entity also zooms out.
        assert!((camera_pixels_at(400.0, 1.0, 2.0) - 0.5).abs() < 1e-5);
    }

    #[test]
    fn stroke_width_is_used_as_authored_without_a_pixel_width() {
        let mut app = test_app();
        let scene = spawn_scene(&mut app);
        let asset = add_asset(&mut app);
        let entity = app
            .world_mut()
            .spawn((
                VivecoInstance::new(asset, Vec::new()).with_stroke_width(0.25),
                VivecoSceneOf(scene),
                Transform::default().with_scale(Vec3::splat(3.0)),
                InheritedVisibility::VISIBLE,
            ))
            .id();

        app.update();

        let instance = app.world().get::<VivecoInstance>(entity).unwrap();
        assert_eq!(instance.stroke_width, 0.25);
    }

    #[test]
    fn a_scene_clears_when_its_members_go_away() {
        let mut app = test_app();
        let scene = spawn_scene(&mut app);
        let asset = add_asset(&mut app);
        let entity = spawn_instance(&mut app, scene, asset, Transform::default());
        app.update();
        assert_eq!(scene_path_counts(&mut app), vec![1]);

        app.world_mut().entity_mut(entity).despawn();
        app.update();

        assert_eq!(scene_path_counts(&mut app), vec![0]);
    }

    #[test]
    fn the_instance_transform_is_applied_before_the_entity_transform() {
        let mut app = test_app();
        let scene = spawn_scene(&mut app);
        let asset = add_asset(&mut app);
        app.world_mut().spawn((
            VivecoInstance::new(asset, Vec::new()).with_transform(Affine::translate((2.0, 3.0))),
            VivecoSceneOf(scene),
            Transform::from_xyz(10.0, 5.0, 0.0),
            InheritedVisibility::VISIBLE,
        ));

        app.update();

        // The entity sits at scene-space (10, -5); the asset's local +3 in
        // Y-down space moves it down to -2.
        let mut query = app.world_mut().query::<&VelloScene2d>();
        let vello = query.get(app.world(), scene).unwrap();
        assert!(
            vello
                .encoding()
                .transforms
                .iter()
                .any(|transform| transform.translation == [12.0, -2.0]),
            "expected the instance transform composed under the entity transform"
        );
    }

    #[test]
    fn stroke_width_in_pixels_compensates_the_instance_transform_scale() {
        let mut app = test_app();
        let scene = spawn_scene(&mut app);
        let asset = add_asset(&mut app);
        let entity = app
            .world_mut()
            .spawn((
                VivecoInstance::new(asset, Vec::new()).with_transform(Affine::scale(2.0)),
                VivecoSceneOf(scene),
                VivecoStrokeWidthInPixels::new(4.0),
                Transform::default().with_scale(Vec3::splat(2.0)),
                InheritedVisibility::VISIBLE,
            ))
            .id();

        app.update();

        // 4 / (2 from the affine * 2 from the entity).
        let instance = app.world().get::<VivecoInstance>(entity).unwrap();
        assert!((instance.stroke_width - 1.0).abs() < 1e-6);
    }

    #[test]
    fn max_affine_scale_measures_rotation_scale_and_shear() {
        assert!((max_affine_scale(&Affine::IDENTITY) - 1.0).abs() < 1e-6);
        assert!((max_affine_scale(&Affine::FLIP_X) - 1.0).abs() < 1e-6);
        assert!((max_affine_scale(&Affine::scale(3.0)) - 3.0).abs() < 1e-6);

        let scaled_rotation = Affine::rotate(std::f64::consts::FRAC_PI_4) * Affine::scale(2.0);
        assert!((max_affine_scale(&scaled_rotation) - 2.0).abs() < 1e-6);

        // A unit shear has AᵀA = [[1, 1], [1, 2]], whose largest eigenvalue
        // is (3 + sqrt(5)) / 2.
        let expected = ((3.0 + 5.0_f64.sqrt()) / 2.0).sqrt() as f32;
        assert!((max_affine_scale(&Affine::skew(1.0, 0.0)) - expected).abs() < 1e-6);
    }

    /// `bevy_vello` negates the off-diagonal terms of a scene entity's
    /// prepared matrix to convert to Vello's Y-down space.
    fn flip_off_diagonals(affine: Affine) -> Affine {
        let [a, b, c, d, e, f] = affine.as_coeffs();
        Affine::new([a, -b, -c, d, e, f])
    }

    #[test]
    fn baked_transform_matches_a_per_entity_scene() {
        let transform = GlobalTransform::from(
            Transform::from_xyz(10.0, 20.0, 0.0)
                .with_scale(Vec3::splat(2.0))
                .with_rotation(Quat::from_rotation_z(0.7)),
        );

        // A camera with no roll, so its screen transform is diagonal.
        let view_projection = Affine::new([2.0, 0.0, 0.0, 3.0, 100.0, 50.0]);

        // The model matrix `bevy_vello` uses for a per-entity scene.
        let matrix = transform.to_matrix();
        let model = Affine::new([
            f64::from(matrix.x_axis.x),
            f64::from(matrix.x_axis.y),
            f64::from(matrix.y_axis.x),
            f64::from(matrix.y_axis.y),
            f64::from(matrix.w_axis.x),
            -f64::from(matrix.w_axis.y),
        ]);

        let per_entity = flip_off_diagonals(view_projection * model);
        let shared = flip_off_diagonals(view_projection) * scene_affine(&transform);

        for (actual, expected) in shared.as_coeffs().into_iter().zip(per_entity.as_coeffs()) {
            assert!(
                (actual - expected).abs() < 1e-6,
                "{actual} != {expected} (all: {:?} vs {:?})",
                shared.as_coeffs(),
                per_entity.as_coeffs()
            );
        }
    }

    #[test]
    fn scene_affine_flips_the_translation_y() {
        let transform = GlobalTransform::from(
            Transform::from_xyz(10.0, 20.0, 0.0)
                .with_scale(Vec3::splat(2.0))
                .with_rotation(Quat::from_rotation_z(std::f32::consts::FRAC_PI_2)),
        );

        // `Transform` applies scale before the +90° rotation, giving the
        // linear part [[0, -2], [2, 0]]. Conjugating by the Y flip negates
        // the off-diagonal terms, and the translation's Y is flipped too.
        let coeffs = scene_affine(&transform).as_coeffs();
        let expected = [0.0, -2.0, 2.0, 0.0, 10.0, -20.0];
        for (actual, expected) in coeffs.into_iter().zip(expected) {
            assert!(
                (actual - expected).abs() < 1e-6,
                "{actual} != {expected} (all: {coeffs:?})"
            );
        }
    }

    fn add_named_asset(app: &mut App, name: &str) -> Handle<crate::Asset> {
        let mut shape = filled_shape();
        shape.name = name.to_owned();
        app.world_mut()
            .resource_mut::<Assets<crate::Asset>>()
            .add(crate::Asset {
                shapes: vec![shape],
                ..Default::default()
            })
    }

    fn spawn_child(app: &mut App, asset: Handle<crate::Asset>, child_of: VivecoChildOf) -> Entity {
        app.world_mut()
            .spawn((
                VivecoInstance::new(asset, Vec::new()),
                child_of,
                Transform::default(),
                InheritedVisibility::VISIBLE,
            ))
            .id()
    }

    fn scene_clip_count(app: &mut App, entity: Entity) -> u32 {
        let mut query = app.world_mut().query::<&VelloScene2d>();
        query
            .get(app.world(), entity)
            .map_or(0, |scene| scene.encoding().n_clips)
    }

    #[test]
    fn a_child_is_drawn_inside_its_host_shape() {
        let mut app = test_app();
        let scene = spawn_scene(&mut app);
        let host_asset = add_asset(&mut app);
        let host = spawn_instance(&mut app, scene, host_asset, Transform::default());
        let child_asset = add_asset(&mut app);
        spawn_child(&mut app, child_asset, VivecoChildOf::new(host, 0usize));

        app.update();

        // The host and child fills, plus the clip layer's bookkeeping paths.
        assert_eq!(scene_path_count(&mut app, scene), 4);
        // The child is painted inside the host shape's clip layer.
        assert_eq!(scene_clip_count(&mut app, scene), 2);
    }

    #[test]
    fn a_child_resolves_its_host_shape_by_name() {
        let mut app = test_app();
        let scene = spawn_scene(&mut app);
        let host_asset = add_named_asset(&mut app, "Screen");
        let host = spawn_instance(&mut app, scene, host_asset, Transform::default());
        let child_asset = add_asset(&mut app);
        spawn_child(&mut app, child_asset, VivecoChildOf::new(host, "Screen"));

        app.update();

        assert_eq!(scene_path_count(&mut app, scene), 4);
    }

    #[test]
    fn a_child_of_a_missing_shape_is_not_drawn() {
        let mut app = test_app();
        let scene = spawn_scene(&mut app);
        let host_asset = add_asset(&mut app);
        let host = spawn_instance(&mut app, scene, host_asset, Transform::default());
        let child_asset = add_asset(&mut app);
        spawn_child(&mut app, child_asset, VivecoChildOf::new(host, "Missing"));

        app.update();

        assert_eq!(scene_path_count(&mut app, scene), 1);
    }

    #[test]
    fn a_child_is_not_drawn_as_a_top_level_instance() {
        let mut app = test_app();
        let default = app
            .world_mut()
            .spawn((VivecoScene::default(), VivecoDefaultScene))
            .id();
        let host_scene = spawn_scene(&mut app);
        let host_asset = add_asset(&mut app);
        let host = spawn_instance(&mut app, host_scene, host_asset, Transform::default());
        let child_asset = add_asset(&mut app);
        let child = spawn_child(&mut app, child_asset, VivecoChildOf::new(host, 0usize));

        app.update();

        // The child draws into the host's scene only and is not linked to the
        // default scene.
        assert_eq!(scene_path_count(&mut app, default), 0);
        assert_eq!(scene_path_count(&mut app, host_scene), 4);
        assert!(app.world().get::<VivecoSceneOf>(child).is_none());
    }

    #[test]
    fn an_invisible_host_hides_its_children() {
        let mut app = test_app();
        let scene = spawn_scene(&mut app);
        let host_asset = add_asset(&mut app);
        let host = app
            .world_mut()
            .spawn((
                VivecoInstance::new(host_asset, Vec::new()),
                VivecoSceneOf(scene),
                Transform::default(),
                InheritedVisibility::HIDDEN,
            ))
            .id();
        let child_asset = add_asset(&mut app);
        spawn_child(&mut app, child_asset, VivecoChildOf::new(host, 0usize));

        app.update();

        assert_eq!(scene_path_count(&mut app, scene), 0);
    }

    #[test]
    fn a_pixel_width_child_compensates_the_host_scale() {
        let mut app = test_app();
        let scene = spawn_scene(&mut app);
        let host_asset = add_asset(&mut app);
        let host = spawn_instance(
            &mut app,
            scene,
            host_asset,
            Transform::default().with_scale(Vec3::splat(2.0)),
        );
        let child_asset = add_asset(&mut app);
        let child = app
            .world_mut()
            .spawn((
                VivecoInstance::new(child_asset, Vec::new()).with_transform(Affine::scale(2.0)),
                VivecoChildOf::new(host, 0usize),
                VivecoStrokeWidthInPixels::new(4.0),
                Transform::default(),
                InheritedVisibility::VISIBLE,
            ))
            .id();

        app.update();

        // 4 / (the host entity's 2 * the child's affine 2).
        let instance = app.world().get::<VivecoInstance>(child).unwrap();
        assert!((instance.stroke_width - 1.0).abs() < 1e-6);
    }
}
