//! The instance component, scene content, and the systems that render them.

use std::cmp::Ordering;
use std::collections::HashSet;

use ::bevy::camera::CameraUpdateSystems;
use ::bevy::camera::visibility::{NoFrustumCulling, RenderLayers, VisibilitySystems};
use ::bevy::prelude::*;
use ::bevy::transform::TransformSystems;
use ::bevy_vello::prelude::{VelloScene2d, VelloView};
use ::vello::kurbo::Affine;

use crate::render::{DrawChild, DrawNode, SceneBuilder, SceneFragment, SceneInstance};
use crate::{AnimationInstance, ShapeIndex};

use super::VivecoLoader;

/// Renders a loaded [`Asset`](crate::Asset) by blending animations at the
/// given times.
///
/// Each animation instance is sampled at its time, the poses are blended by
/// weight, and the resulting geometry is drawn wherever this entity's
/// [`VivecoChildOf`] link places it: at a scene's root, inside a shape of
/// another node, or at another node's root. An unparented instance joins the
/// scene marked [`VivecoDefaultScene`], if any; otherwise it is not rendered.
/// [`VivecoPlugin`] never spawns a scene, so spawn a [`VivecoScene`] yourself
/// and link the instance to it (or mark the scene as the default).
///
/// Nodes nest to any depth. Add [`VivecoChildOf::shape`] to paint the instance
/// inside a shape of another document, or [`VivecoChildOf::root`] to paint it
/// at another node's paint level.
///
/// ```
/// # use bevy::prelude::*;
/// # use viveco::{Asset, VivecoInstance, VivecoScene, VivecoChildOf};
/// # fn spawn(commands: &mut Commands, asset: Handle<Asset>) {
/// let scene = commands.spawn(VivecoScene::default()).id();
/// commands.spawn((
///     VivecoInstance::from_animation(asset, 0),
///     VivecoChildOf::root(scene),
/// ));
/// # }
/// ```
///
/// Every node is placed by its own [`GlobalTransform`] (including any Bevy
/// [`ChildOf`] ancestors) times its [`transform`](Self::transform). Each
/// top-level shape is Z-ordered against every other fragment in the scene. Both
/// non-cyclic and cyclic animations use the sampling behavior of
/// [`Animation::sample_pose`](crate::Animation::sample_pose).
///
/// The [`transform`](Self::transform) field adds a Vello [`Affine`] of its own,
/// applied in the node's parent space. It is expressed in the asset's local
/// Y-down space, so it can mirror, skew, or offset the asset independently of
/// the Bevy hierarchy above it.
///
/// Stroke widths are explicit and in the asset's local units: they are painted
/// like the rest of the geometry, so scaling the instance's transform, any
/// ancestor placement, its scene, or the camera scales the stroke along with the
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

/// The context of a parent that a [`VivecoChildOf`] node draws into.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum VivecoContext {
    /// The parent's root paint level.
    ///
    /// A scene's root is its flat, Z-sorted fragment list. A document's root is
    /// its unclipped top level.
    #[default]
    Root,
    /// Inside one shape of the parent's document.
    ///
    /// This is a flat [`ShapeIndex`]. Resolve a name with
    /// [`Asset::shape_index_by_name`](crate::Asset::shape_index_by_name) or use
    /// the index constants generated by [`codegen`](crate::codegen).
    Shape(ShapeIndex),
}

/// Draws this entity's content inside another entity's paint tree.
///
/// Every drawable node is a child of a scene root or of another node:
///
/// - a [`VivecoInstance`] contributes its document,
/// - [`VivecoFragments`] contributes custom Vello scenes,
/// - an entity with only children groups them.
///
/// A node attached with [`VivecoContext::Root`] shares its parent's paint level;
/// a node attached with [`VivecoContext::Shape`] is clipped by that shape and
/// competes with its children by Z.
///
/// The link only selects the paint level and clip; it does not propagate
/// transforms. Every node is placed in scene space by its own
/// [`GlobalTransform`] (so Bevy [`ChildOf`] ancestors apply) times its
/// [`VivecoInstance::transform`]. To make a child follow its host, make it a
/// Bevy child of that entity as well.
///
/// Children nest to any depth. The parent's [`VivecoChildren`] stays up to date
/// automatically, and despawning a parent unlinks its children without
/// despawning them.
///
/// ```
/// # use bevy::prelude::*;
/// # use viveco::{Asset, VivecoChildOf, VivecoInstance, VivecoScene};
/// # fn spawn(commands: &mut Commands, tv: Handle<Asset>, animation: Handle<Asset>) {
/// let scene = commands.spawn(VivecoScene::default()).id();
/// let tv_entity = commands
///     .spawn((VivecoInstance::from_asset(tv), VivecoChildOf::root(scene)))
///     .id();
///
/// // Draw the animation inside the TV's shape named "Screen". Resolve the name
/// // once with `asset.shape_index_by_name` or a generated index constant.
/// let screen = 5usize;
/// commands.spawn((
///     VivecoInstance::from_animation(animation, 0),
///     VivecoChildOf::shape(tv_entity, screen),
/// ));
/// # }
/// ```
#[derive(Component, Clone, Debug, PartialEq)]
#[require(Transform, Visibility)]
#[relationship(relationship_target = VivecoChildren)]
pub struct VivecoChildOf {
    /// The entity this node draws into.
    #[relationship]
    pub parent: Entity,
    /// Which context of `parent` this node attaches to.
    pub context: VivecoContext,
}

impl VivecoChildOf {
    /// Attaches at `parent`'s root paint level.
    pub fn root(parent: Entity) -> Self {
        Self {
            parent,
            context: VivecoContext::Root,
        }
    }

    /// Attaches inside `shape` of `parent`'s document.
    ///
    /// `shape` is a flat [`ShapeIndex`].
    pub fn shape(parent: Entity, shape: ShapeIndex) -> Self {
        Self {
            parent,
            context: VivecoContext::Shape(shape),
        }
    }
}

/// The [`VivecoChildOf`] children of a scene or node.
///
/// Maintained automatically by [`VivecoChildOf`]. The plugin reads it to walk
/// the paint tree, and user systems can inspect it.
#[derive(Component, Default, Debug)]
#[relationship_target(relationship = VivecoChildOf)]
pub struct VivecoChildren(Vec<Entity>);

impl VivecoChildren {
    /// The child entities, in the order they were added.
    pub fn iter(&self) -> impl Iterator<Item = Entity> + '_ {
        self.0.iter().copied()
    }
}

/// A per-frame source of custom Vello scenes owned by an entity.
///
/// Producers fill it in [`VivecoSceneSystems::Build`]; the plugin draws the
/// scenes and clears them each frame. Each scene targets a context:
///
/// - [`push`](Self::push) draws at the entity's root paint level (or, when the
///   entity is attached to a shape with [`VivecoChildOf::shape`], at that
///   shape's level);
/// - [`push_at_shape`](Self::push_at_shape) draws inside a shape of the entity's
///   *own* document, clipped by it and competing with its children by Z.
///
/// Content is authored in the entity's local space and placed by the entity's
/// [`GlobalTransform`] (including any Bevy [`ChildOf`] ancestors) in every
/// context. A shape target resolves against the entity's own document, so the
/// entity must also carry a loaded [`VivecoInstance`].
///
/// ```
/// # use bevy::prelude::*;
/// # use viveco::{VivecoChildOf, VivecoFragments, VivecoScene};
/// # fn spawn(commands: &mut Commands) {
/// let scene = commands.spawn(VivecoScene::default()).id();
/// commands.spawn((VivecoFragments::default(), VivecoChildOf::root(scene)));
/// # }
/// ```
#[derive(Component, Default)]
#[require(Transform, Visibility)]
pub struct VivecoFragments {
    entries: Vec<(Option<ShapeIndex>, SceneFragment)>,
}

impl VivecoFragments {
    /// Queues one scene at the entity's root paint level.
    pub fn push(&mut self, fragment: SceneFragment) {
        self.entries.push((None, fragment));
    }

    /// Queues one scene inside `shape` of the entity's own document.
    ///
    /// `shape` is a flat [`ShapeIndex`], so this is O(1) on the render path.
    /// Resolve a name once with
    /// [`Asset::shape_index_by_name`](crate::Asset::shape_index_by_name), or use
    /// the index constants generated by [`codegen`](crate::codegen).
    pub fn push_at_shape(&mut self, shape: ShapeIndex, fragment: SceneFragment) {
        self.entries.push((Some(shape), fragment));
    }

    /// The queued scenes and their target shapes.
    pub fn entries(&self) -> impl Iterator<Item = (Option<ShapeIndex>, &SceneFragment)> + '_ {
        self.entries
            .iter()
            .map(|(shape, fragment)| (*shape, fragment))
    }

    /// Empties the queue while preserving its allocation.
    pub(crate) fn drain(&mut self) -> std::vec::Drain<'_, (Option<ShapeIndex>, SceneFragment)> {
        self.entries.drain(..)
    }

    /// Empties the queue.
    pub(crate) fn clear(&mut self) {
        self.entries.clear();
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
/// A scene is the root of a draw tree. The plugin draws every node linked to it
/// through [`VivecoChildOf`], including documents, nested attachments, and
/// custom [`VivecoFragments`]; your own systems can also add scene-space
/// fragments by querying `&mut VivecoScene` and running in
/// [`VivecoSceneSystems::Build`].
///
/// Fragments are sorted by Z across all producers before the scene is painted,
/// so custom geometry interleaves with instance artwork. Viveco never spawns a
/// scene on its own.
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
/// Every frame the plugin clears every [`VivecoScene`] and [`VivecoFragments`]
/// buffer in [`Clear`](Self::Clear), lets producers add content in
/// [`Build`](Self::Build), walks the draw tree in [`Instances`](Self::Instances),
/// then sorts and paints fragments in [`Finalize`](Self::Finalize). Custom
/// producer systems belong in [`Build`](Self::Build).
#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub enum VivecoSceneSystems {
    /// Clears every scene's fragments and every custom fragment buffer.
    /// Plugin-internal.
    Clear,
    /// Producers push fragments into scenes and custom fragment buffers.
    Build,
    /// Walks the draw tree and contributes fragments to every scene.
    /// Plugin-internal.
    Instances,
    /// Sorts fragments by Z and paints the Vello scene. Plugin-internal.
    Finalize,
}

/// Marks the [`VivecoScene`] that unparented drawable nodes join.
///
/// Add this component to one [`VivecoScene`] to opt into a default scene. The
/// plugin then links every unparented [`VivecoInstance`] and
/// [`VivecoFragments`] node to it, so nodes can be spawned without picking a
/// scene:
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
/// Linking happens once per node; an explicit [`VivecoChildOf`] always wins. If
/// several scenes are marked, the one with the lowest entity ID is used. With
/// no marked scene, unparented nodes are not rendered. Custom producer systems
/// can query this marker to target the same scene.
#[derive(Component, Default)]
pub struct VivecoDefaultScene;

/// Keeps strokes at a fixed size on screen.
///
/// Attach this to a [`VivecoInstance`], or to its [`VivecoScene`], to have the
/// plugin rewrite the instance's
/// [`stroke_width`](VivecoInstance::stroke_width) so painted strokes keep
/// `width` logical pixels. Every scale that affects the painted size is
/// compensated: the instance's [`transform`](VivecoInstance::transform), every
/// placement above it, its scene, and the observing camera. A component on the
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
/// # use viveco::{Asset, VivecoInstance, VivecoStrokeWidthInPixels, VivecoScene, VivecoChildOf};
/// # fn spawn(commands: &mut Commands, asset: Handle<Asset>) {
/// let scene = commands
///     .spawn((VivecoScene::default(), VivecoStrokeWidthInPixels::new(2.0)))
///     .id();
/// commands.spawn((
///     VivecoInstance::from_animation(asset, 0),
///     VivecoChildOf::root(scene),
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
/// - clears every [`VivecoScene`] and [`VivecoFragments`] buffer each frame in
///   [`VivecoSceneSystems::Clear`],
/// - walks every scene's [`VivecoChildOf`] tree in
///   [`VivecoSceneSystems::Instances`], drawing documents and custom fragments
///   everywhere their links place them, and
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
                    VivecoSceneSystems::Instances,
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
                    (clear_viveco_scenes, clear_viveco_fragments).in_set(VivecoSceneSystems::Clear),
                    render_viveco_scenes.in_set(VivecoSceneSystems::Instances),
                    finalize_viveco_scenes.in_set(VivecoSceneSystems::Finalize),
                ),
            );
    }
}

/// Links unparented drawable nodes to the [`VivecoDefaultScene`] root.
///
/// A node is linked once; afterwards the `Without<VivecoChildOf>` query matches
/// nothing. The system returns immediately when no scene is marked, so its
/// steady-state cost is an empty query scan. `chain` inserts a deferred-command
/// sync point before the scene update, so a newly linked node renders in the
/// same frame. Scenes themselves are never linked.
#[allow(clippy::type_complexity)]
fn attach_default_scene(
    mut commands: Commands,
    defaults: Query<Entity, With<VivecoDefaultScene>>,
    unlinked: Query<
        Entity,
        (
            Without<VivecoChildOf>,
            Without<VivecoScene>,
            Or<(
                With<VivecoInstance>,
                With<VivecoFragments>,
                With<VivecoChildren>,
            )>,
        ),
    >,
) {
    // Pick the lowest index so the choice is deterministic if more than one
    // scene is marked. (`Entity`'s own ordering is not index order.)
    let Some(scene) = defaults.iter().min_by_key(|entity| entity.index_u32()) else {
        return;
    };
    for entity in &unlinked {
        commands.entity(entity).insert(VivecoChildOf::root(scene));
    }
}

/// Keeps instances with [`VivecoStrokeWidthInPixels`] at a constant on-screen
/// stroke width.
///
/// The painted width scales with the instance's own [`GlobalTransform`] (which
/// includes any Bevy [`ChildOf`] ancestors), its local
/// [`transform`](VivecoInstance::transform), its scene, and the observing
/// camera. This walks the Viveco tree from every scene root to find the nodes
/// belonging to each scene and divides the target width by those factors. The
/// component can live on an instance or on its scene; the instance's value
/// wins. The width is written only when it changes, so a static zoom does not
/// mark the instance changed every frame.
#[allow(clippy::type_complexity)]
fn normalize_viveco_stroke_widths(
    mut instances: Query<&mut VivecoInstance>,
    transforms: Query<&GlobalTransform>,
    child_lists: Query<&VivecoChildren>,
    pixel_widths: Query<&VivecoStrokeWidthInPixels>,
    scenes: Query<
        (
            &GlobalTransform,
            Option<&RenderLayers>,
            Option<&VivecoStrokeWidthInPixels>,
            Option<&VivecoChildren>,
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
    mut visited: Local<HashSet<Entity>>,
) {
    for (scene_transform, scene_layers, scene_pixels, scene_children) in &scenes {
        let scene_scale = max_scale(scene_transform);
        let pixels_per_unit = pixels_per_unit_for(scene_layers, &cameras);
        visited.clear();
        let Some(scene_children) = scene_children else {
            continue;
        };
        for child in scene_children.iter() {
            visit_scale(
                child,
                scene_scale,
                pixels_per_unit,
                scene_pixels,
                &mut instances,
                &transforms,
                &child_lists,
                &pixel_widths,
                &mut visited,
            );
        }
    }
}

/// Writes stroke widths for one node and its Viveco descendants.
#[allow(clippy::too_many_arguments)]
fn visit_scale(
    entity: Entity,
    scene_scale: f32,
    pixels_per_unit: f32,
    scene_pixels: Option<&VivecoStrokeWidthInPixels>,
    instances: &mut Query<&mut VivecoInstance>,
    transforms: &Query<&GlobalTransform>,
    child_lists: &Query<&VivecoChildren>,
    pixel_widths: &Query<&VivecoStrokeWidthInPixels>,
    visited: &mut HashSet<Entity>,
) {
    if !visited.insert(entity) {
        return;
    }

    // A node's placement is its own GlobalTransform (which includes any Bevy
    // ChildOf ancestors) times its local document transform.
    let node_scale = transforms.get(entity).map_or(1.0, max_scale)
        * instances
            .get(entity)
            .map_or(1.0, |instance| max_affine_scale(&instance.transform));

    if let (Some(pixels), Ok(mut instance)) = (
        pixel_widths.get(entity).ok().or(scene_pixels),
        instances.get_mut(entity),
    ) {
        let world_scale = (node_scale * scene_scale * pixels_per_unit).max(f32::EPSILON);
        let width = pixels.width / world_scale;
        if instance.stroke_width != width {
            instance.stroke_width = width;
        }
    }

    if let Ok(children) = child_lists.get(entity) {
        for child in children.iter() {
            visit_scale(
                child,
                scene_scale,
                pixels_per_unit,
                scene_pixels,
                instances,
                transforms,
                child_lists,
                pixel_widths,
                visited,
            );
        }
    }
}

/// Pixels per world unit for the most zoomed-in camera that renders
/// `scene_layers`.
///
/// bevy_vello renders a scene only for cameras whose render layers overlap the
/// scene's, so this uses the same rule. With no matching camera the entity and
/// scene scales are still compensated; assume an unzoomed camera.
#[allow(clippy::type_complexity)]
fn pixels_per_unit_for(
    scene_layers: Option<&RenderLayers>,
    cameras: &Query<
        (
            &Camera,
            &Projection,
            &GlobalTransform,
            Option<&RenderLayers>,
        ),
        (With<Camera2d>, With<VelloView>),
    >,
) -> f32 {
    let scene_layers = scene_layers.cloned().unwrap_or_default();
    let mut pixels_per_unit = 0.0_f32;
    for (camera, projection, camera_transform, camera_layers) in cameras {
        let camera_layers = camera_layers.cloned().unwrap_or_default();
        if scene_layers.intersects(&camera_layers) {
            pixels_per_unit =
                pixels_per_unit.max(camera_pixels_per_unit(camera, projection, camera_transform));
        }
    }
    if pixels_per_unit <= 0.0 {
        1.0
    } else {
        pixels_per_unit
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
        // Keep the fragment vec's capacity.
        scene.fragments.clear();
    }
}

/// Empties every custom fragment buffer at the start of the frame.
fn clear_viveco_fragments(mut buffers: Query<&mut VivecoFragments>) {
    for mut buffer in &mut buffers {
        buffer.clear();
    }
}

/// Contributes the artwork of every linked [`VivecoInstance`] to its scene.
///
/// Each top-level shape becomes one fragment carrying that shape's Z, so
/// instance artwork interleaves with fragments from other producers.
///
/// Walks every scene's draw tree. Nodes attached at a scene's root become
/// top-level fragments; shape-context children are painted inside their shape's clip
/// layer and compete with its children by Z. Custom [`VivecoFragments`] are
/// consumed in the same walk.
#[allow(clippy::type_complexity)]
fn render_viveco_scenes(
    mut scenes: Query<(Entity, &mut VivecoScene, Option<&VivecoChildren>)>,
    mut nodes: Query<
        (
            Option<&VivecoInstance>,
            Option<&mut VivecoFragments>,
            &GlobalTransform,
            &InheritedVisibility,
        ),
        Without<VivecoScene>,
    >,
    child_lists: Query<&VivecoChildren>,
    contexts: Query<&VivecoChildOf>,
    assets: Res<Assets<crate::Asset>>,
    mut visited: Local<HashSet<Entity>>,
) {
    for (_, mut scene, scene_children) in &mut scenes {
        let Some(scene_children) = scene_children else {
            continue;
        };
        let mut builder = SceneBuilder::new();
        visited.clear();
        for child in scene_children.iter() {
            let Ok(child_of) = contexts.get(child) else {
                continue;
            };
            if child_of.context != VivecoContext::Root {
                ::bevy::log::warn!("child {child:?} of a scene must use VivecoContext::Root");
                continue;
            }
            if let Some(node) = build_draw_node(
                child,
                true,
                &mut nodes,
                &child_lists,
                &contexts,
                &assets,
                &mut visited,
            ) {
                builder.add_node(node);
            }
        }
        scene.push_fragments(builder.into_fragments());
    }
}

/// Builds the draw subtree rooted at `entity`.
///
/// Every node is placed by its own [`GlobalTransform`], so a Bevy [`ChildOf`]
/// hierarchy applies and the Viveco parent does not affect the transform.
/// Visibility is composed down the Viveco tree, and `visited` guards against
/// relationship cycles.
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn build_draw_node<'a>(
    entity: Entity,
    parent_visible: bool,
    nodes: &mut Query<
        (
            Option<&VivecoInstance>,
            Option<&mut VivecoFragments>,
            &GlobalTransform,
            &InheritedVisibility,
        ),
        Without<VivecoScene>,
    >,
    child_lists: &Query<&VivecoChildren>,
    contexts: &Query<&VivecoChildOf>,
    assets: &'a Assets<crate::Asset>,
    visited: &mut HashSet<Entity>,
) -> Option<DrawNode<'a>> {
    if !visited.insert(entity) {
        return None;
    }

    let (mut node, visible) = {
        let Ok((instance, fragments, transform, visibility)) = nodes.get_mut(entity) else {
            return None;
        };
        let visible = parent_visible && visibility.get();
        if !visible || max_scale(transform) <= f32::EPSILON {
            return None;
        }
        let placement = scene_affine(transform)
            * instance.map_or(Affine::IDENTITY, |instance| instance.transform);
        let instance = instance.and_then(|instance| {
            assets.get(&instance.asset).map(|asset| {
                SceneInstance::animated(asset, &instance.mixer)
                    .with_stroke_width(instance.stroke_width)
            })
        });

        let mut node = DrawNode::new(placement);
        node.instance = instance;

        // Custom scenes are added before child nodes so equal Z keeps fragment
        // content ahead of nested nodes. Draining preserves the buffer's
        // capacity for the next frame.
        if let Some(mut fragments) = fragments {
            for (shape, fragment) in fragments.drain() {
                match shape {
                    None => node.children.push(DrawChild::root_fragment(fragment)),
                    Some(shape) => {
                        let Some(asset) = node.instance.as_ref().map(|instance| instance.asset)
                        else {
                            ::bevy::log::warn!(
                                "fragment on {entity:?} targets a shape, but the entity has no \
                                 document"
                            );
                            continue;
                        };
                        if shape >= asset.shape_count() {
                            ::bevy::log::warn!(
                                "fragment on {entity:?} targets shape {shape}, which is missing \
                                 from its document; the fragment is not drawn"
                            );
                            continue;
                        }
                        node.children
                            .push(DrawChild::shape_fragment(shape, fragment));
                    }
                }
            }
        }

        (node, visible)
    };

    // Read the child list through a separate query so it does not have to be
    // copied out before recursing.
    if let Ok(children) = child_lists.get(entity) {
        for child in children.iter() {
            let Ok(child_of) = contexts.get(child) else {
                continue;
            };
            match &child_of.context {
                VivecoContext::Root => {
                    let Some(child_node) = build_draw_node(
                        child,
                        visible,
                        nodes,
                        child_lists,
                        contexts,
                        assets,
                        visited,
                    ) else {
                        continue;
                    };
                    node.children.push(DrawChild::root_node(child_node));
                }
                VivecoContext::Shape(shape) => {
                    let shape = *shape;
                    let Some(asset) = node.instance.as_ref().map(|instance| instance.asset) else {
                        ::bevy::log::warn!(
                            "child {child:?} is attached to a shape, but {entity:?} has no \
                             document"
                        );
                        continue;
                    };
                    if shape >= asset.shape_count() {
                        ::bevy::log::warn!(
                            "child {child:?} targets shape {shape}, which is missing from \
                             {entity:?}'s document; the child is not drawn"
                        );
                        continue;
                    }
                    let Some(child_node) = build_draw_node(
                        child,
                        visible,
                        nodes,
                        child_lists,
                        contexts,
                        assets,
                        visited,
                    ) else {
                        continue;
                    };
                    node.children.push(DrawChild::shape_node(shape, child_node));
                }
            }
        }
    }

    node.sort_children();
    Some(node)
}

/// Sorts every scene's fragments by Z and paints them into its [`VelloScene2d`].
fn finalize_viveco_scenes(mut scenes: Query<(&mut VivecoScene, &mut VelloScene2d)>) {
    for (mut scene, mut vello) in &mut scenes {
        // Stable: fragments with equal Z keep the order they were pushed.
        scene
            .fragments
            .sort_by(|a, b| a.z.partial_cmp(&b.z).unwrap_or(Ordering::Equal));

        // Reset in place so the scene's encoded buffers keep their capacity
        // across frames instead of allocating a new Scene every frame.
        vello.reset();
        for fragment in &scene.fragments {
            let transform = (fragment.transform != Affine::IDENTITY).then_some(fragment.transform);
            vello.append(&fragment.scene, transform);
        }
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
    use ::vello::Scene;

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
                VivecoChildOf::root(scene),
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
        fragments: Query<(&TestFragment, &VivecoChildOf, &GlobalTransform)>,
    ) {
        for (spec, scene_of, transform) in &fragments {
            let Ok(mut scene) = scenes.get_mut(scene_of.parent) else {
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
            scene.push(SceneFragment::new(spec.z, fragment));
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
            scene.push(SceneFragment::new(0.0, fragment));
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
            VivecoChildOf::root(scene),
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
            app.world().get::<VivecoChildOf>(instance),
            Some(&VivecoChildOf::root(scene))
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

        // Despawning the scene removes `VivecoChildOf` from its members.
        app.world_mut().entity_mut(old_default).despawn();
        app.update();
        assert!(scene_path_counts(&mut app).is_empty());
        assert!(app.world().get::<VivecoChildOf>(instance).is_none());

        let replacement = app
            .world_mut()
            .spawn((VivecoScene::default(), VivecoDefaultScene))
            .id();
        app.update();

        assert_eq!(scene_path_count(&mut app, replacement), 1);
        assert_eq!(
            app.world().get::<VivecoChildOf>(instance),
            Some(&VivecoChildOf::root(replacement))
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
                VivecoChildOf::root(scene),
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
                VivecoChildOf::root(scene),
                transform,
                InheritedVisibility::VISIBLE,
            ))
            .id();
        // An instance-level component overrides the scene's.
        let overridden = app
            .world_mut()
            .spawn((
                VivecoInstance::new(asset, Vec::new()),
                VivecoChildOf::root(scene),
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
                VivecoChildOf::root(scene),
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
            VivecoChildOf::root(scene),
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
                VivecoChildOf::root(scene),
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
        spawn_child(&mut app, child_asset, VivecoChildOf::shape(host, 0usize));

        app.update();

        // The host and child fills, plus the clip layer's bookkeeping paths.
        assert_eq!(scene_path_count(&mut app, scene), 4);
        // The child is painted inside the host shape's clip layer.
        assert_eq!(scene_clip_count(&mut app, scene), 2);
    }

    #[test]
    fn a_child_of_a_missing_shape_is_not_drawn() {
        let mut app = test_app();
        let scene = spawn_scene(&mut app);
        let host_asset = add_asset(&mut app);
        let host = spawn_instance(&mut app, scene, host_asset, Transform::default());
        let child_asset = add_asset(&mut app);
        spawn_child(&mut app, child_asset, VivecoChildOf::shape(host, 99usize));

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
        let child = spawn_child(&mut app, child_asset, VivecoChildOf::shape(host, 0usize));

        app.update();

        // The child draws into the host's scene only and is not linked to the
        // default scene.
        assert_eq!(scene_path_count(&mut app, default), 0);
        assert_eq!(scene_path_count(&mut app, host_scene), 4);
        assert_eq!(
            app.world()
                .get::<VivecoChildOf>(child)
                .map(|link| link.parent),
            Some(host)
        );
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
                VivecoChildOf::root(scene),
                Transform::default(),
                InheritedVisibility::HIDDEN,
            ))
            .id();
        let child_asset = add_asset(&mut app);
        spawn_child(&mut app, child_asset, VivecoChildOf::shape(host, 0usize));

        app.update();

        assert_eq!(scene_path_count(&mut app, scene), 0);
    }

    #[test]
    fn a_viveco_child_does_not_inherit_its_hosts_transform() {
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
                VivecoChildOf::shape(host, 0usize),
                VivecoStrokeWidthInPixels::new(4.0),
                Transform::default(),
                InheritedVisibility::VISIBLE,
            ))
            .id();

        app.update();

        // The host's Viveco placement is not part of the child's transform, so
        // only the child's own affine (2) applies: 4 / 2.
        let instance = app.world().get::<VivecoInstance>(child).unwrap();
        assert!((instance.stroke_width - 2.0).abs() < 1e-6);
    }

    #[test]
    fn a_bevy_child_of_its_viveco_host_inherits_the_bevy_hierarchy() {
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
                VivecoChildOf::shape(host, 0usize),
                ChildOf(host),
                VivecoStrokeWidthInPixels::new(4.0),
                Transform::default(),
                InheritedVisibility::VISIBLE,
            ))
            .id();

        app.update();

        // The Bevy ChildOf hierarchy contributes the host's scale 2, and the
        // child's affine contributes 2 more: 4 / 4.
        let instance = app.world().get::<VivecoInstance>(child).unwrap();
        assert!((instance.stroke_width - 1.0).abs() < 1e-6);
    }

    /// A producer that queues one custom scene into every source on the first
    /// frame it runs.
    fn push_one_custom_fragment(mut sources: Query<&mut VivecoFragments>, mut pushed: Local<bool>) {
        if *pushed {
            return;
        }
        for mut source in &mut sources {
            let mut fragment = Scene::new();
            fragment.fill(
                vello::peniko::Fill::NonZero,
                Affine::IDENTITY,
                vello::peniko::Color::from_rgba8(0, 0, 255, 255),
                None,
                &vello::kurbo::Rect::new(0.0, 0.0, 4.0, 4.0),
            );
            source.push(SceneFragment::new(0.0, fragment));
            *pushed = true;
        }
    }

    #[test]
    fn a_top_level_fragment_source_draws_into_its_scene() {
        let mut app = test_app();
        app.add_systems(
            PostUpdate,
            push_one_custom_fragment.in_set(VivecoSceneSystems::Build),
        );
        let scene = spawn_scene(&mut app);
        app.world_mut().spawn((
            VivecoFragments::default(),
            VivecoChildOf::root(scene),
            Transform::from_xyz(4.0, 0.0, 0.0),
            InheritedVisibility::VISIBLE,
        ));

        app.update();

        assert_eq!(scene_path_count(&mut app, scene), 1);
        let mut query = app.world_mut().query::<&VelloScene2d>();
        let vello = query.get(app.world(), scene).unwrap();
        assert!(
            vello
                .encoding()
                .transforms
                .iter()
                .any(|transform| transform.translation == [4.0, 0.0]),
            "the source entity's transform places the custom scene"
        );
    }

    #[test]
    fn a_child_fragment_source_is_clipped_by_its_host_shape() {
        let mut app = test_app();
        app.add_systems(
            PostUpdate,
            push_one_custom_fragment.in_set(VivecoSceneSystems::Build),
        );
        let scene = spawn_scene(&mut app);
        let host_asset = add_asset(&mut app);
        let host = spawn_instance(&mut app, scene, host_asset, Transform::default());
        app.world_mut().spawn((
            VivecoFragments::default(),
            VivecoChildOf::shape(host, 0usize),
            Transform::default(),
            InheritedVisibility::VISIBLE,
        ));

        app.update();

        // The host fill, the custom fill, and the clip layer's bookkeeping.
        assert_eq!(scene_path_count(&mut app, scene), 4);
        assert_eq!(scene_clip_count(&mut app, scene), 2);
    }

    fn push_one_shape_fragment(mut sources: Query<&mut VivecoFragments>, mut pushed: Local<bool>) {
        if *pushed {
            return;
        }
        for mut source in &mut sources {
            let mut fragment = Scene::new();
            fragment.fill(
                vello::peniko::Fill::NonZero,
                Affine::IDENTITY,
                vello::peniko::Color::from_rgba8(0, 0, 255, 255),
                None,
                &vello::kurbo::Rect::new(0.0, 0.0, 4.0, 4.0),
            );
            source.push_at_shape(0usize, SceneFragment::new(1.0, fragment));
            *pushed = true;
        }
    }

    #[test]
    fn a_shape_fragment_draws_inside_the_same_entity_shape() {
        let mut app = test_app();
        app.add_systems(
            PostUpdate,
            push_one_shape_fragment.in_set(VivecoSceneSystems::Build),
        );
        let scene = spawn_scene(&mut app);
        let host_asset = add_asset(&mut app);
        app.world_mut().spawn((
            VivecoInstance::from_asset(host_asset),
            VivecoChildOf::root(scene),
            VivecoFragments::default(),
            Transform::default(),
            InheritedVisibility::VISIBLE,
        ));

        app.update();

        // The host fill, the custom fill, and the clip layer's bookkeeping.
        assert_eq!(scene_path_count(&mut app, scene), 4);
        assert_eq!(scene_clip_count(&mut app, scene), 2);
    }

    #[test]
    fn a_shape_fragment_without_a_document_is_skipped() {
        let mut app = test_app();
        app.add_systems(
            PostUpdate,
            push_one_shape_fragment.in_set(VivecoSceneSystems::Build),
        );
        let scene = spawn_scene(&mut app);
        app.world_mut().spawn((
            VivecoFragments::default(),
            VivecoChildOf::root(scene),
            Transform::default(),
            InheritedVisibility::VISIBLE,
        ));

        app.update();

        assert_eq!(scene_path_count(&mut app, scene), 0);
    }

    #[test]
    fn attachments_nest_to_arbitrary_depth() {
        let mut app = test_app();
        let scene = spawn_scene(&mut app);
        let host_asset = add_asset(&mut app);
        let middle_asset = add_asset(&mut app);
        let bottom_asset = add_asset(&mut app);

        let host = spawn_instance(&mut app, scene, host_asset, Transform::default());
        let middle = app
            .world_mut()
            .spawn((
                VivecoInstance::from_asset(middle_asset),
                VivecoChildOf::shape(host, 0usize),
                Transform::default(),
                InheritedVisibility::VISIBLE,
            ))
            .id();
        app.world_mut().spawn((
            VivecoInstance::from_asset(bottom_asset),
            VivecoChildOf::shape(middle, 0usize),
            Transform::default(),
            InheritedVisibility::VISIBLE,
        ));

        app.update();

        // Three fills plus two nested clip layers' bookkeeping paths.
        assert_eq!(scene_path_count(&mut app, scene), 7);
        assert_eq!(scene_clip_count(&mut app, scene), 4);
    }

    #[test]
    fn a_root_child_joins_the_scene_level() {
        let mut app = test_app();
        let scene = spawn_scene(&mut app);
        let host_asset = add_asset(&mut app);
        let child_asset = add_asset(&mut app);
        let host = spawn_instance(&mut app, scene, host_asset, Transform::default());
        app.world_mut().spawn((
            VivecoInstance::from_asset(child_asset).with_transform(Affine::translate((5.0, 6.0))),
            VivecoChildOf::root(host),
            Transform::default(),
            InheritedVisibility::VISIBLE,
        ));

        app.update();

        // The host and the root child are separate top-level fragments.
        assert_eq!(scene_path_count(&mut app, scene), 2);
        assert_eq!(scene_clip_count(&mut app, scene), 0);
        let mut query = app.world_mut().query::<&VelloScene2d>();
        let vello = query.get(app.world(), scene).unwrap();
        assert!(
            vello
                .encoding()
                .transforms
                .iter()
                .any(|transform| transform.translation == [5.0, 6.0]),
            "the root child's instance transform places it"
        );
    }

    #[test]
    fn a_deeply_nested_bevy_hierarchy_compensates_every_scale() {
        let mut app = test_app();
        let scene = spawn_scene(&mut app);
        let host_asset = add_asset(&mut app);
        let middle_asset = add_asset(&mut app);
        let bottom_asset = add_asset(&mut app);

        let host = spawn_instance(
            &mut app,
            scene,
            host_asset,
            Transform::default().with_scale(Vec3::splat(2.0)),
        );
        let middle = app
            .world_mut()
            .spawn((
                VivecoInstance::from_asset(middle_asset),
                VivecoChildOf::shape(host, 0usize),
                ChildOf(host),
                Transform::default().with_scale(Vec3::splat(3.0)),
                InheritedVisibility::VISIBLE,
            ))
            .id();
        let bottom = app
            .world_mut()
            .spawn((
                VivecoInstance::from_asset(bottom_asset),
                VivecoChildOf::shape(middle, 0usize),
                ChildOf(middle),
                VivecoStrokeWidthInPixels::new(12.0),
                Transform::default().with_scale(Vec3::splat(2.0)),
                InheritedVisibility::VISIBLE,
            ))
            .id();

        app.update();

        // The Bevy hierarchy multiplies the entity scales: 2 * 3 * 2.
        let instance = app.world().get::<VivecoInstance>(bottom).unwrap();
        assert!((instance.stroke_width - 1.0).abs() < 1e-6);
    }
}
