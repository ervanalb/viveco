//! Demonstrates parallax with several Viveco scenes at different depths.
//!
//! Every layer renders under one orthographic `Camera2d`. The library's
//! zero-sized [`ParallaxLayer`] marker opts a vello scene into
//! [`apply_parallax`], which derives its scale and offset from the layer's Z
//! and the camera's position, emulating the way a perspective camera would
//! treat planes at different depths.
//!
//! The layers are the landscape artwork converted from `landscape.svg` by
//! `build.rs` into `landscape_back.viveco`, `landscape_mid.viveco`, and
//! `landscape_front.viveco`. [`VivecoStrokeWidthInPixels`] keeps their strokes
//! two logical pixels wide regardless of parallax scale.
//!
//! Run with `cargo run -p viveco --example parallax --features bevy`.

use bevy::camera::ScalingMode;
use bevy::prelude::*;
use bevy_vello::VelloPlugin;
use bevy_vello::prelude::*;
use viveco::{
    ParallaxLayer, VivecoInstance, VivecoPlugin, VivecoScene, VivecoSceneOf,
    VivecoStrokeWidthInPixels, apply_parallax,
};

/// Where `build.rs` writes the converted landscape layers.
const ASSET_ROOT: &str = concat!(env!("OUT_DIR"), "/assets");

/// World-space center and maximum extent of the camera's view.
const CAMERA_Z: f32 = 10.;
const CAMERA: Vec3 = Vec3::new(400.0, 100.0, CAMERA_Z);
const MAX_VIEW_WIDTH: f32 = 500.0;
const MAX_VIEW_HEIGHT: f32 = 500.0;

/// Automatic pan amplitudes, in world units.
const PAN_X: f32 = 100.0;
const PAN_Y: f32 = 20.0;

/// Layer depths. Negative is farther away.
const FAR_Z: f32 = -3.0;
const MID_Z: f32 = 0.0;
const NEAR_Z: f32 = 3.0;

fn main() {
    App::new()
        .add_plugins(DefaultPlugins.set(AssetPlugin {
            file_path: ASSET_ROOT.to_owned(),
            ..default()
        }))
        .add_plugins(VelloPlugin::default())
        .add_plugins(VivecoPlugin)
        .insert_resource(ClearColor(Color::srgb(0.8, 1., 1.)))
        .add_systems(Startup, setup)
        // The camera must move before the layers react to it.
        .add_systems(Update, (pan_camera, apply_parallax).chain())
        .run();
}

fn setup(mut commands: Commands, asset_server: Res<AssetServer>) {
    commands.spawn((
        Camera2d,
        VelloView,
        Projection::Orthographic(OrthographicProjection {
            scaling_mode: ScalingMode::AutoMax {
                max_width: MAX_VIEW_WIDTH,
                max_height: MAX_VIEW_HEIGHT,
            },
            ..OrthographicProjection::default_2d()
        }),
        Transform::default(),
    ));

    // Strokes stay two logical pixels wide despite the parallax scaling.
    let stroke = VivecoStrokeWidthInPixels::new(2.0);
    let far = commands
        .spawn((
            VivecoScene::default(),
            stroke,
            ParallaxLayer,
            Transform::from_xyz(0.0, 0.0, FAR_Z),
        ))
        .id();
    let mid = commands
        .spawn((
            VivecoScene::default(),
            stroke,
            ParallaxLayer,
            Transform::from_xyz(0.0, 0.0, MID_Z),
        ))
        .id();
    let near = commands
        .spawn((
            VivecoScene::default(),
            stroke,
            ParallaxLayer,
            Transform::from_xyz(0.0, 0.0, NEAR_Z),
        ))
        .id();

    // Static artwork: an empty mixer samples the rest pose.
    let back = asset_server.load::<viveco::Asset>("landscape_back.viveco");
    let middle = asset_server.load::<viveco::Asset>("landscape_mid.viveco");
    let front = asset_server.load::<viveco::Asset>("landscape_front.viveco");
    commands.spawn((VivecoInstance::new(back, Vec::new()), VivecoSceneOf(far)));
    commands.spawn((VivecoInstance::new(middle, Vec::new()), VivecoSceneOf(mid)));
    commands.spawn((VivecoInstance::new(front, Vec::new()), VivecoSceneOf(near)));
}

/// Pans the orthographic camera so the parallax layers have something to react to.
fn pan_camera(
    time: Res<Time>,
    mut cameras: Query<&mut Transform, (With<Camera2d>, With<VelloView>)>,
) {
    let t = time.elapsed_secs();
    let pan = Vec2::new((t * 0.3).sin() * PAN_X, (t * 0.2).sin() * PAN_Y);
    for mut camera in &mut cameras {
        camera.translation.x = CAMERA.x + pan.x;
        camera.translation.y = CAMERA.y + pan.y;
        camera.translation.z = CAMERA_Z;
    }
}
