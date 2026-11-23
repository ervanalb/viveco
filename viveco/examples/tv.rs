//! Draws the animated `planet.viveco` inside the screen of `tv.viveco`.
//!
//! The TV is a [`viveco::VivecoInstance`] linked to a scene. The planet
//! is a child instance: [`viveco::VivecoChildOf`] names the TV's `"screen"`
//! shape, so the plugin paints the planet inside that shape's clip layer and Z
//! order instead of drawing it as a separate instance.
//!
//! Run with `cargo run -p viveco --example tv --features bevy`.

use bevy::camera::ScalingMode;
use bevy::prelude::*;
use bevy_vello::VelloPlugin;
use bevy_vello::prelude::*;
use vello::kurbo::Affine;

/// Shape and animation constants generated from the example assets by
/// `build.rs`.
mod planet {
    include!(concat!(env!("OUT_DIR"), "/planet.rs"));
}
mod tv {
    include!(concat!(env!("OUT_DIR"), "/tv.rs"));
}

/// The TV `"screen"` shape's path bounds, from `viveco info tv.viveco`.
const SCREEN_MIN: Vec2 = Vec2::new(-115.921, -85.67);
const SCREEN_SIZE: Vec2 = Vec2::new(222.209, 170.365);

/// `planet.viveco`'s rest bounds, from `viveco info planet.viveco`.
const PLANET_MIN: Vec2 = Vec2::new(-147.188, -89.64);
const PLANET_SIZE: Vec2 = Vec2::new(248.299, 196.541);

/// Minimum world-space area the camera shows.
const MIN_VIEW_WIDTH: f32 = 380.0;
const MIN_VIEW_HEIGHT: f32 = 480.0;

fn main() {
    App::new()
        .add_plugins(DefaultPlugins.set(AssetPlugin {
            file_path: "examples/assets".to_owned(),
            ..default()
        }))
        .add_plugins(VelloPlugin::default())
        .add_plugins(viveco::VivecoPlugin)
        .insert_resource(ClearColor(Color::srgb(0.9, 0.8, 0.6)))
        .add_systems(Startup, setup)
        // Build this frame's mixer before the plugin poses the instance.
        .add_systems(Update, animate.before(viveco::VivecoSceneSystems::Build))
        .run();
}

/// The planet animation state (just one t value).
#[derive(Component, Default)]
struct Orbit {
    /// Playhead position in seconds, always in `0..ORBIT_DURATION`.
    time: f32,
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
    ));

    commands.spawn((viveco::VivecoScene::default(), viveco::VivecoDefaultScene));

    let tv_asset = asset_server.load::<viveco::Asset>("tv.viveco");
    let tv_entity = commands
        .spawn((
            viveco::VivecoInstance::from_asset(tv_asset),
            viveco::VivecoStrokeWidthInPixels::new(2.0),
        ))
        .id();

    // The planet is drawn inside the TV's "screen" shape
    let planet_asset = asset_server.load::<viveco::Asset>("planet.viveco");
    commands.spawn((
        viveco::VivecoInstance::from_asset(planet_asset).with_transform(Affine::scale(0.8)),
        viveco::VivecoChildOf::new(tv_entity, tv::shapes::SCREEN),
        viveco::VivecoStrokeWidthInPixels::new(2.0),
        Orbit::default(),
    ));
}

/// Advances each orbit's playhead and rebuilds its instance's mixer every frame.
fn animate(time: Res<Time>, mut planets: Query<(&mut viveco::VivecoInstance, &mut Orbit)>) {
    let delta = time.delta_secs();

    for (mut instance, mut orbit) in &mut planets {
        orbit.time = (orbit.time + delta).rem_euclid(planet::animations::ORBIT_DURATION);

        instance.mixer.clear();
        instance
            .mixer
            .push((planet::animations::ORBIT, orbit.time, 1.0));
    }
}
