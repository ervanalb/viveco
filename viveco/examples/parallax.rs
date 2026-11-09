//! Demonstrates parallax with several Viveco scenes at different depths.
//!
//! Every layer renders under one orthographic `Camera2d`. A zero-sized
//! [`ParallaxLayer`] marker opts a scene into a pre-render system that derives
//! its scale and offset from the layer's Z and the camera's position, emulating
//! the way a perspective camera would treat planes at different depths.
//!
//! The distant layer is procedurally drawn, the midground is a set of hills,
//! and the foreground holds a walking `stick.viveco` instance plus the ground.
//!
//! Run with `cargo run -p viveco --example parallax --features bevy`.

use bevy::camera::ScalingMode;
use bevy::prelude::*;
use bevy_vello::VelloPlugin;
use bevy_vello::prelude::*;
use vello::kurbo::{Affine, BezPath, Circle, Line, Rect, Stroke};
use vello::peniko::{Color as VelloColor, Fill};
use viveco::{
    SceneFragment, VivecoInstance, VivecoPlugin, VivecoScene, VivecoSceneOf, VivecoSceneSystems,
};

/// Animation, shape, and bone constants generated from `stick.viveco` by `build.rs`.
mod stick {
    include!(concat!(env!("OUT_DIR"), "/stick.rs"));
}

const WALK: usize = stick::animations::WALK;

/// Virtual perspective eye above the reference plane (z = 0). z increases
/// toward the viewer, so a plane at z has scale `EYE_Z / (EYE_Z - z)`.
const EYE_Z: f32 = 1000.0;
const CAMERA_Z: f32 = 0.0;

/// The design area the camera looks at. Asset/scene space is Y-down.
const CHARACTER_CENTER: Vec2 = Vec2::new(325.0, 355.0);
const MIN_VIEW_WIDTH: f32 = 1800.0;
const MIN_VIEW_HEIGHT: f32 = 1400.0;

/// Automatic pan amplitudes, in world units.
const PAN_X: f32 = 480.0;
const PAN_Y: f32 = 40.0;

/// Layer depths. Negative is farther away.
const FAR_Z: f32 = -700.0;
const MID_Z: f32 = -350.0;
const NEAR_Z: f32 = 0.0;

/// Procedural content extents, generous enough to cover the pan at any aspect.
const CONTENT_MIN_X: f64 = -8000.0;
const CONTENT_MAX_X: f64 = 8500.0;
const CONTENT_MIN_Y: f64 = -3000.0;
const CONTENT_MAX_Y: f64 = 4000.0;
const HORIZON: f64 = 945.0;

/// Opts a scene entity into depth-based parallax. The entity's Z is the depth.
#[derive(Component, Default)]
struct ParallaxLayer;

/// The three scene entities, so procedural content can be pushed into them.
#[derive(Resource)]
struct Layers {
    far: Entity,
    mid: Entity,
    near: Entity,
}

fn main() {
    App::new()
        .add_plugins(DefaultPlugins.set(AssetPlugin {
            file_path: "examples/assets".to_owned(),
            ..default()
        }))
        .add_plugins(VelloPlugin::default())
        .add_plugins(VivecoPlugin)
        .insert_resource(ClearColor(Color::srgb(0.03, 0.04, 0.06)))
        .add_systems(Startup, setup)
        // The camera must move before the layers react to it.
        .add_systems(Update, (pan_camera, apply_parallax).chain())
        .add_systems(Update, advance_walk)
        .add_systems(PostUpdate, draw_layers.in_set(VivecoSceneSystems::Build))
        .run();
}

fn setup(mut commands: Commands, asset_server: Res<AssetServer>) {
    commands.spawn((
        Camera2d,
        VelloView,
        Projection::Orthographic(OrthographicProjection {
            scaling_mode: ScalingMode::AutoMin {
                min_width: MIN_VIEW_WIDTH,
                min_height: MIN_VIEW_HEIGHT,
            },
            ..OrthographicProjection::default_2d()
        }),
        // Asset space is Y-down, so the camera's world-space center negates Y.
        Transform::from_xyz(CHARACTER_CENTER.x, -CHARACTER_CENTER.y, CAMERA_Z),
    ));

    let far = commands
        .spawn((
            VivecoScene::default(),
            ParallaxLayer,
            Transform::from_xyz(0.0, 0.0, FAR_Z),
        ))
        .id();
    let mid = commands
        .spawn((
            VivecoScene::default(),
            ParallaxLayer,
            Transform::from_xyz(0.0, 0.0, MID_Z),
        ))
        .id();
    let near = commands
        .spawn((
            VivecoScene::default(),
            ParallaxLayer,
            Transform::from_xyz(0.0, 0.0, NEAR_Z),
        ))
        .id();
    commands.insert_resource(Layers { far, mid, near });

    let handle = asset_server.load::<viveco::Asset>("stick.viveco");
    commands.spawn((
        VivecoInstance::from_animation(handle, WALK),
        VivecoSceneOf(near),
    ));
}

/// Pans the orthographic camera so the parallax layers have something to react to.
fn pan_camera(
    time: Res<Time>,
    mut cameras: Query<&mut Transform, (With<Camera2d>, With<VelloView>)>,
) {
    let t = time.elapsed_secs();
    let pan = Vec2::new((t * 0.35).sin() * PAN_X, (t * 0.23).sin() * PAN_Y);
    for mut camera in &mut cameras {
        camera.translation.x = CHARACTER_CENTER.x + pan.x;
        camera.translation.y = -CHARACTER_CENTER.y + pan.y;
        camera.translation.z = CAMERA_Z;
    }
}

/// Scales and offsets each layer as a perspective camera at [`EYE_Z`] would.
fn apply_parallax(
    camera: Query<&Transform, (With<Camera2d>, With<VelloView>)>,
    mut layers: Query<&mut Transform, (With<ParallaxLayer>, Without<Camera2d>)>,
) {
    let Ok(camera) = camera.single() else {
        return;
    };

    for mut layer in &mut layers {
        let depth = EYE_Z - layer.translation.z;
        if depth <= f32::EPSILON {
            continue;
        }
        let scale = EYE_Z / depth;

        // Content is authored around the layer origin, so this both scales it
        // about the camera's view center and makes far layers follow the camera.
        layer.translation.x = camera.translation.x * (1.0 - scale);
        layer.translation.y = camera.translation.y * (1.0 - scale);
        layer.scale = Vec3::splat(scale);
    }
}

/// Keeps the character walking.
fn advance_walk(
    time: Res<Time>,
    assets: Res<Assets<viveco::Asset>>,
    mut instances: Query<&mut VivecoInstance>,
) {
    let clock = time.elapsed_secs();
    for mut instance in &mut instances {
        let Some(asset) = assets.get(&instance.asset) else {
            continue;
        };
        let Some(animation) = asset.animations.get(WALK) else {
            continue;
        };
        let sample = if animation.duration > 0.0 {
            clock % animation.duration
        } else {
            0.0
        };
        instance.mixer.clear();
        instance.mixer.push((WALK, sample, 1.0));
    }
}

/// Pushes the procedural background, hills, and ground into their scenes.
fn draw_layers(layers: Res<Layers>, mut scenes: Query<&mut VivecoScene>) {
    if let Ok(mut scene) = scenes.get_mut(layers.far) {
        scene.push(SceneFragment {
            z: 0.0,
            scene: star_field(),
        });
    }
    if let Ok(mut scene) = scenes.get_mut(layers.mid) {
        scene.push(SceneFragment {
            z: 0.0,
            scene: hills(),
        });
    }
    if let Ok(mut scene) = scenes.get_mut(layers.near) {
        // Behind the character's own shapes.
        scene.push(SceneFragment {
            z: -10.0,
            scene: ground(),
        });
    }
}

fn star_field() -> vello::Scene {
    let mut scene = vello::Scene::new();
    let mut state = 0x1234_5678u32;
    for _ in 0..260 {
        let x = lerp(CONTENT_MIN_X, CONTENT_MAX_X, unit_random(&mut state));
        let y = lerp(CONTENT_MIN_Y, CONTENT_MAX_Y, unit_random(&mut state));
        let radius = 2.0 + 4.0 * unit_random(&mut state);
        let shade = (120.0 + 110.0 * unit_random(&mut state)) as u8;
        scene.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            VelloColor::from_rgba8(shade, shade, shade, 255),
            None,
            &Circle::new((x, y), radius),
        );
    }
    scene
}

fn hills() -> vello::Scene {
    let mut scene = vello::Scene::new();
    let ridges = [
        (800.0, 90.0, 0.0011, VelloColor::from_rgba8(26, 34, 52, 255)),
        (900.0, 70.0, 0.0017, VelloColor::from_rgba8(20, 27, 42, 255)),
        (
            1000.0,
            45.0,
            0.0029,
            VelloColor::from_rgba8(15, 20, 32, 255),
        ),
    ];

    for (index, (base, amplitude, frequency, color)) in ridges.into_iter().enumerate() {
        let mut path = BezPath::new();
        path.move_to((CONTENT_MIN_X, CONTENT_MAX_Y));
        let mut x = CONTENT_MIN_X;
        while x <= CONTENT_MAX_X {
            let y = base + amplitude * ((x * frequency) + index as f64 * 1.7).sin();
            path.line_to((x, y));
            x += 60.0;
        }
        path.line_to((CONTENT_MAX_X, CONTENT_MAX_Y));
        path.close_path();
        scene.fill(Fill::NonZero, Affine::IDENTITY, color, None, &path);
    }

    scene
}

fn ground() -> vello::Scene {
    let mut scene = vello::Scene::new();
    scene.fill(
        Fill::NonZero,
        Affine::IDENTITY,
        VelloColor::from_rgba8(18, 22, 30, 255),
        None,
        &Rect::new(CONTENT_MIN_X, HORIZON, CONTENT_MAX_X, CONTENT_MAX_Y),
    );
    scene.stroke(
        &Stroke::new(3.0),
        Affine::IDENTITY,
        VelloColor::from_rgba8(80, 110, 130, 255),
        None,
        &Line::new((CONTENT_MIN_X, HORIZON), (CONTENT_MAX_X, HORIZON)),
    );
    scene
}

fn lerp(a: f64, b: f64, t: f64) -> f64 {
    a + (b - a) * t
}

/// Cheap deterministic random in `0..1`, so the star field is stable per frame.
fn unit_random(state: &mut u32) -> f64 {
    *state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
    f64::from(*state >> 8) / f64::from(1u32 << 24)
}
