//! Loads and loops the animated `planet.viveco` example.
//!
//! The asset's only animation, "orbit", plays at weight 1 forever. A playhead
//! advances every frame and wraps at the clip duration; [`animate`] writes that
//! playhead into the instance's `mixer`, which is the input the plugin poses
//! the artwork from.
//!
//! [`Orbit`] sits beside the [`viveco::VivecoInstance`] and owns the playhead.
//! Every frame [`animate`] advances it and refills the instance's `mixer` from
//! it, reusing the existing allocation.
//!
//! The camera is framed with fixed design-space constants and
//! [`ScalingMode::AutoMin`], so it zooms with the window and keeps the planet
//! and its moon in view as the window resizes, without measuring geometry.
//!
//! The example asset lives in the workspace's `examples` directory, so the
//! asset server root is pointed there. `build.rs` generates animation indices
//! and durations plus shape and bone constants from that file, so the demo
//! refers to `planet::animations::ORBIT` and
//! `planet::animations::ORBIT_DURATION` instead of hard-coded values. The demo
//! opts into a default scene, so its instance does not need to name one.
//!
//! Run with `cargo run -p viveco --example planet --features bevy`.

use bevy::camera::ScalingMode;
use bevy::prelude::*;
use bevy_vello::VelloPlugin;
use bevy_vello::prelude::*;

/// Animation, shape, and bone constants generated from `planet.viveco` by
/// `build.rs`.
mod planet {
    include!(concat!(env!("OUT_DIR"), "/planet.rs"));
}

// The example frames the artwork with fixed constants (asset space is Y-down)
// rather than measuring geometry at runtime. The projections are large enough
// to contain the planet and the moon's whole orbit with a margin.
/// The camera's world-space center.
///
/// The artwork is centered near (6.5, 8.6) in asset space, where Y points
/// down; the camera's world is Y-up, so the Y coordinate is negated.
const CAMERA_CENTER: Vec2 = Vec2::new(6.5, -8.6);
/// Minimum world-space area the camera shows.
const MIN_VIEW_WIDTH: f32 = 420.0;
const MIN_VIEW_HEIGHT: f32 = 300.0;

fn main() {
    App::new()
        .add_plugins(DefaultPlugins.set(AssetPlugin {
            file_path: "examples/assets".to_owned(),
            ..default()
        }))
        .add_plugins(VelloPlugin::default())
        .add_plugins(viveco::VivecoPlugin)
        .insert_resource(ClearColor(Color::srgb(0.02, 0.02, 0.05)))
        .add_systems(Startup, setup)
        // Build this frame's mixer before the plugin poses the instance.
        .add_systems(Update, animate.before(viveco::VivecoSceneSystems::Build))
        .run();
}

/// The planet's orbit playhead.
///
/// This is attached beside the [`viveco::VivecoInstance`]. Each frame
/// [`animate`] advances the playhead, then rebuilds the instance's mixer from
/// it, so the mixer is always a fresh description of this frame's blend.
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
        Transform::from_xyz(CAMERA_CENTER.x, CAMERA_CENTER.y, 100.0),
    ));

    // Mark one scene as the default; for VivecoInstances without explicit `VivecoSceneOf`.
    commands.spawn((viveco::VivecoScene::default(), viveco::VivecoDefaultScene));

    // The instance starts at rest; `animate` supplies the mixer from the
    // `Orbit` state every frame.
    let handle = asset_server.load::<viveco::Asset>("planet.viveco");
    commands.spawn((viveco::VivecoInstance::from_asset(handle), Orbit::default()));
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
