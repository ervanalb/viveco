//! The instance component, scene content, and the systems that render them.

use std::cmp::Ordering;

use ::bevy::camera::CameraUpdateSystems;
use ::bevy::camera::visibility::{NoFrustumCulling, RenderLayers, VisibilitySystems};
use ::bevy::prelude::*;
use ::bevy::transform::TransformSystems;
use ::bevy_vello::prelude::{VelloScene2d, VelloView};
use ::vello::Scene;
use ::vello::kurbo::Affine;

use crate::AnimationInstance;
use crate::render::{SceneBuilder, SceneFragment, SceneInstance};

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
/// Stroke widths are explicit and in the asset's local units: they are painted
/// like the rest of the geometry, so scaling the instance entity, its scene, or
/// the camera scales the stroke along with the artwork. Add
/// [`VivecoStrokeWidthInPixels`] to keep an on-screen width instead.
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
            stroke_width: 1.0,
        }
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
/// [`VivecoDefaultScene`], if one exists; otherwise it is not rendered.
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
/// `width` logical pixels. All three scales that affect the painted size are
/// compensated: the instance entity, its scene, and the observing camera. A
/// component on the instance overrides one on its scene.
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
///   [`VivecoDefaultScene`]) in [`VivecoSceneSystems::Build`], and
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
/// instance renders in the same frame.
fn attach_default_scene(
    mut commands: Commands,
    defaults: Query<Entity, With<VivecoDefaultScene>>,
    unlinked: Query<Entity, (With<VivecoInstance>, Without<VivecoSceneOf>)>,
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
/// The painted width scales with the instance entity, its scene, and the
/// observing camera, so this divides the target width by all three factors.
/// The component can live on the instance or on its scene; the instance's
/// value wins. The width is written only when it changes, so a static zoom does
/// not mark the instance changed every frame.
#[allow(clippy::type_complexity)]
fn normalize_viveco_stroke_widths(
    mut instances: Query<(
        &mut VivecoInstance,
        &VivecoSceneOf,
        &GlobalTransform,
        Option<&VivecoStrokeWidthInPixels>,
    )>,
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
    for (mut instance, scene_of, transform, pixels) in &mut instances {
        let Ok((scene_transform, scene_layers, scene_pixels)) = scenes.get(scene_of.0) else {
            continue;
        };
        let Some(pixels) = pixels.or(scene_pixels) else {
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

        let world_scale =
            (max_scale(transform) * max_scale(scene_transform) * pixels_per_unit).max(f32::EPSILON);
        let width = pixels.width / world_scale;
        if instance.stroke_width != width {
            instance.stroke_width = width;
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

/// Empties every scene's fragment list at the start of the frame.
fn clear_viveco_scenes(mut scenes: Query<&mut VivecoScene>) {
    for mut scene in &mut scenes {
        scene.fragments.clear();
    }
}

/// Contributes the artwork of every linked [`VivecoInstance`] to its scene.
///
/// Each top-level shape becomes one fragment carrying that shape's Z, so
/// instance artwork interleaves with fragments from other producers.
fn render_viveco_instances(
    mut scenes: Query<&mut VivecoScene>,
    instances: Query<(
        &VivecoInstance,
        &VivecoSceneOf,
        &GlobalTransform,
        &InheritedVisibility,
    )>,
    assets: Res<Assets<crate::Asset>>,
) {
    for (instance, scene_of, transform, visibility) in &instances {
        if !visibility.get() || max_scale(transform) <= f32::EPSILON {
            continue;
        }
        let Some(asset) = assets.get(&instance.asset) else {
            continue;
        };
        let Ok(mut scene) = scenes.get_mut(scene_of.0) else {
            continue;
        };

        let deformed = SceneInstance::animated(asset, &instance.mixer)
            .with_transform(scene_affine(transform))
            .with_stroke_width(instance.stroke_width);
        let mut builder = SceneBuilder::new();
        builder.add_instance(&deformed);
        scene.push_fragments(builder.into_fragments());
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
}
