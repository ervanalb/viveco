//! The animator component and the systems that render it.

use ::bevy::asset::AssetEvent;
use ::bevy::camera::visibility::{NoFrustumCulling, VisibilitySystems};
use ::bevy::prelude::*;
use ::bevy::transform::TransformSystems;
use ::bevy_vello::prelude::VelloScene2d;
use ::vello::Scene;
use ::vello::kurbo::Affine;

use crate::AnimationInstance;
use crate::render::{SceneBuilder, SceneInstance};

use super::VivecoLoader;

/// Renders a loaded [`Asset`](crate::Asset) by blending animations at the
/// given times.
///
/// Each animation instance is sampled at its time, the poses are blended by
/// weight, and the resulting geometry is placed into the [`VivecoScene`] this
/// entity refers to. Animators without a [`VivecoSceneOf`] component join the
/// default scene. The entity's [`GlobalTransform`] is baked into scene space,
/// and its top-level shapes are Z-ordered against the other members' shapes.
/// Both non-cyclic and cyclic animations use the sampling behavior of
/// [`Animation::sample_pose`](crate::Animation::sample_pose).
///
/// A user who wants a different pose or rendering path can spawn their own
/// component instead: the underlying steps are ordinary library calls.
#[derive(Component, Clone, Debug)]
#[require(Transform, Visibility)]
pub struct VivecoAnimator {
    /// The loaded document to render.
    pub asset: Handle<crate::Asset>,
    /// The animation mixer: the instances to blend, as
    /// `(animation index, time in seconds, weight)`.
    pub mixer: Vec<AnimationInstance>,
}

impl VivecoAnimator {
    /// Creates an animator for `asset` with the given mixer.
    pub fn new(asset: Handle<crate::Asset>, mixer: Vec<AnimationInstance>) -> Self {
        Self { asset, mixer }
    }

    /// Creates an animator that plays only the animation at `index`.
    pub fn from_animation(asset: Handle<crate::Asset>, index: usize) -> Self {
        Self::new(asset, vec![(index, 0.0, 1.0)])
    }
}

/// Links a [`VivecoAnimator`] to the [`VivecoScene`] it draws into.
///
/// Animators without this component join the default scene, which
/// [`VivecoPlugin`] spawns and marks with [`VivecoDefaultScene`]. Insert this
/// component to put an animator in a specific scene, or to split animators
/// across several scenes.
///
/// This is a Bevy relationship: despawning the scene removes the component,
/// and the scene's [`VivecoSceneMembers`] stays up to date automatically.
#[derive(Component, Clone, Copy, PartialEq, Eq, Debug)]
#[relationship(relationship_target = VivecoSceneMembers)]
pub struct VivecoSceneOf(pub Entity);

/// The animators that draw into one [`VivecoScene`].
///
/// Maintained automatically by [`VivecoSceneOf`]. A scene rebuilds when its
/// membership changes or one of its members changes.
#[derive(Component, Default, Debug)]
#[relationship_target(relationship = VivecoSceneOf)]
pub struct VivecoSceneMembers(Vec<Entity>);

/// An entity that carries one Vello scene.
///
/// [`VivecoPlugin`] spawns one on startup with an empty [`VelloScene2d`] and
/// marks it with [`VivecoDefaultScene`]. Every animator that refers to this
/// scene contributes its deformed artwork, placed by its entity's
/// [`GlobalTransform`] and sorted against the other members' shapes by Z.
///
/// Leave the entity's transform at its default to display each animator at its
/// own Bevy transform. A transform here applies to the whole scene.
///
/// Spawn additional `VivecoScene` entities and point animators at them with
/// [`VivecoSceneOf`] to render independent scenes (for example, one per camera
/// or render layer).
#[derive(Component)]
#[require(VelloScene2d, NoFrustumCulling)]
pub struct VivecoScene;

/// Marks the scene that animators join when they have no [`VivecoSceneOf`].
///
/// [`VivecoPlugin`] gives this marker to the scene it spawns, or to the first
/// existing scene. Move it to another [`VivecoScene`] to change the default.
#[derive(Component, Default)]
pub struct VivecoDefaultScene;

/// Registers the asset loader and the rendering systems.
pub struct VivecoPlugin;

impl Plugin for VivecoPlugin {
    fn build(&self, app: &mut App) {
        app.init_asset::<crate::Asset>()
            .init_asset_loader::<VivecoLoader>()
            .add_systems(Startup, spawn_viveco_scene)
            .add_systems(
                PostUpdate,
                (
                    attach_animators_to_default_scene,
                    mark_reloaded_animators,
                    update_viveco_scenes,
                )
                    .chain()
                    .after(TransformSystems::Propagate)
                    .after(VisibilitySystems::VisibilityPropagate),
            );
    }
}

/// Spawns the default scene, or marks an existing one as the default.
fn spawn_viveco_scene(
    mut commands: Commands,
    scenes: Query<Entity, With<VivecoScene>>,
    defaults: Query<Entity, (With<VivecoScene>, With<VivecoDefaultScene>)>,
) {
    if !defaults.is_empty() {
        return;
    }
    match scenes.iter().next() {
        Some(scene) => {
            commands.entity(scene).insert(VivecoDefaultScene);
        }
        None => {
            commands.spawn((VivecoScene, VivecoDefaultScene));
        }
    }
}

/// Attaches animators that do not name a scene to the default one.
///
/// `chain` inserts a deferred-command sync point before the scene update, so a
/// newly attached animator is rendered in the same frame.
fn attach_animators_to_default_scene(
    mut commands: Commands,
    defaults: Query<Entity, (With<VivecoScene>, With<VivecoDefaultScene>)>,
    animators: Query<Entity, (With<VivecoAnimator>, Without<VivecoSceneOf>)>,
) {
    let Some(scene) = defaults.iter().next() else {
        return;
    };
    for entity in &animators {
        commands.entity(entity).insert(VivecoSceneOf(scene));
    }
}

/// Makes animators rebuild when their asynchronously loaded asset arrives,
/// changes, or goes away. Change detection on the component only sees edits to
/// the animation instances.
fn mark_reloaded_animators(
    mut events: MessageReader<AssetEvent<crate::Asset>>,
    mut animators: Query<&mut VivecoAnimator>,
) {
    for event in events.read() {
        let id = match event {
            AssetEvent::Added { id }
            | AssetEvent::Modified { id }
            | AssetEvent::Removed { id }
            | AssetEvent::LoadedWithDependencies { id } => *id,
            _ => continue,
        };
        for mut animator in &mut animators {
            if animator.asset.id() == id {
                animator.set_changed();
            }
        }
    }
}

/// Rebuilds each scene from its members.
///
/// A scene is rebuilt when its membership changes, or when one of its members'
/// animator, global transform, or inherited visibility changes. Every member is
/// considered once a rebuild is needed, because the combined Z order depends on
/// all of them.
// Bevy system signatures are inherently verbose; this query is still the
// clearest way to express the components the update needs.
#[allow(clippy::type_complexity)]
fn update_viveco_scenes(
    mut scenes: Query<(&mut VelloScene2d, Option<Ref<VivecoSceneMembers>>), With<VivecoScene>>,
    animators: Query<(
        Ref<VivecoAnimator>,
        Ref<GlobalTransform>,
        Ref<InheritedVisibility>,
    )>,
    assets: Res<Assets<crate::Asset>>,
) {
    for (mut scene, members) in &mut scenes {
        let Some(members) = members else {
            // Nothing refers to this scene. Clear geometry left over from
            // members that were removed.
            scene.reset();
            continue;
        };

        let changed = members.is_changed()
            || members.iter().any(|entity| {
                animators
                    .get(entity)
                    .is_ok_and(|(animator, transform, visibility)| {
                        animator.is_changed() || transform.is_changed() || visibility.is_changed()
                    })
            });
        if !changed {
            continue;
        }

        // Sorting by entity makes equal-Z ties independent of relationship
        // insertion order.
        let mut members: Vec<_> = members.iter().collect();
        members.sort_unstable();

        // Instances own their deformed geometry, but the builder borrows their
        // shapes, so they have to outlive it.
        let mut instances = Vec::new();
        for entity in members {
            let Ok((animator, transform, visibility)) = animators.get(entity) else {
                continue;
            };
            if !visibility.get() {
                continue;
            }
            let Some(asset) = assets.get(&animator.asset) else {
                continue;
            };
            let scale = transform.scale().x.abs().max(transform.scale().y.abs());
            if scale <= f32::EPSILON {
                continue;
            }

            instances.push(
                SceneInstance::animated(asset, &animator.mixer)
                    .with_transform(scene_affine(&transform))
                    .with_stroke_width(1.0 / scale),
            );
        }

        let mut builder = SceneBuilder::new();
        for instance in &instances {
            builder.add_instance(instance);
        }

        let mut combined = Scene::new();
        builder.finish(&mut combined);
        *scene = VelloScene2d::from(combined);
    }
}

/// Converts a Bevy world transform into scene space.
///
/// Scene geometry is Y-down, while Bevy's world is Y-up. `bevy_vello` applies
/// that conversion itself when it places a [`VelloScene2d`]; for a transform
/// baked into the geometry the conversion conjugates the transform by a Y flip,
/// which negates the off-diagonal terms and the translation's Y.
///
/// This matches the per-entity path for an unrotated 2D camera. A rolled
/// camera's screen transform cannot be reproduced from an entity transform
/// alone, because it would depend on the camera.
fn scene_affine(transform: &GlobalTransform) -> Affine {
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

    fn spawn_animator(app: &mut App, asset: Handle<crate::Asset>, transform: Transform) -> Entity {
        app.world_mut()
            .spawn((
                VivecoAnimator::new(asset, Vec::new()),
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

    #[test]
    fn one_scene_combines_every_animator() {
        let mut app = test_app();
        let asset = add_asset(&mut app);
        spawn_animator(&mut app, asset.clone(), Transform::from_xyz(5.0, 6.0, 0.0));
        spawn_animator(&mut app, asset, Transform::default());

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
    fn animators_can_target_their_own_scene() {
        let mut app = test_app();
        let asset = add_asset(&mut app);
        spawn_animator(&mut app, asset, Transform::default());
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
        let second = app.world_mut().spawn(VivecoScene).id();
        app.world_mut().spawn((
            VivecoAnimator::new(two_shapes, Vec::new()),
            VivecoSceneOf(second),
            Transform::default(),
            InheritedVisibility::VISIBLE,
        ));
        app.update();

        assert_eq!(scene_path_counts(&mut app), vec![1, 2]);
    }

    #[test]
    fn a_scene_clears_when_its_members_go_away() {
        let mut app = test_app();
        let asset = add_asset(&mut app);
        let entity = spawn_animator(&mut app, asset, Transform::default());
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
