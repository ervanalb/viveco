//! Loads and animates the rigged `stick.viveco` example.
//!
//! The example asset lives in the workspace's `examples` directory, so the
//! asset server root is pointed there. `build.rs` generates animation, shape,
//! and bone constants from that file, so the demo starts on
//! `stick::animations::WALK` instead of a hard-coded index. The demo opts into
//! a default scene, so its instance does not need to name one, then cycles
//! through every animation with a short crossfade, which exercises the
//! `(index, time, weight)` instances on [`VivecoInstance`].
//!
//! Run with `cargo run -p viveco --example animate --features bevy`.

use bevy::prelude::*;
use bevy_vello::VelloPlugin;
use bevy_vello::prelude::*;

/// Animation, shape, and bone constants generated from `stick.viveco` by `build.rs`.
mod stick {
    include!(concat!(env!("OUT_DIR"), "/stick.rs"));
}

/// How long each animation is shown before moving to the next one.
const CYCLE_SECONDS: f32 = 2.5;
/// Length of the crossfade at the start of each cycle.
const FADE_SECONDS: f32 = 0.5;
/// Start on the animation named "Walk".
const START_ANIMATION: usize = stick::animations::WALK;

fn main() {
    App::new()
        .add_plugins(DefaultPlugins.set(AssetPlugin {
            file_path: "examples/assets".to_owned(),
            ..default()
        }))
        .add_plugins(VelloPlugin::default())
        .add_plugins(viveco::VivecoPlugin)
        .insert_resource(ClearColor(Color::srgb(0.10, 0.10, 0.12)))
        .init_resource::<Demo>()
        .add_systems(Startup, setup)
        .add_systems(Update, animate)
        .run();
}

/// State used by the example to switch between the asset's animations.
#[derive(Resource)]
struct Demo {
    current: usize,
    elapsed: f32,
    times: Vec<f32>,
}

impl Default for Demo {
    fn default() -> Self {
        Self {
            current: START_ANIMATION,
            elapsed: 0.0,
            times: Vec::new(),
        }
    }
}

fn setup(mut commands: Commands, asset_server: Res<AssetServer>) {
    commands.spawn((
        Camera2d,
        VelloView,
        Transform::from_xyz(0.0, 0.0, 100.0).with_scale(Vec3::splat(1.)),
    ));

    // Mark one scene as the default; for VivecoInstances without explicit `VivecoSceneOf`.
    commands.spawn((viveco::VivecoScene::default(), viveco::VivecoDefaultScene));

    let handle = asset_server.load::<viveco::Asset>("stick.viveco");
    commands.spawn((
        viveco::VivecoInstance::from_animation(handle, START_ANIMATION),
        viveco::VivecoStrokeWidthInPixels::default(),
    ));
}

/// Advances every animation and rotates through them with a short crossfade.
fn animate(
    time: Res<Time>,
    assets: Res<Assets<viveco::Asset>>,
    mut demo: ResMut<Demo>,
    mut instances: Query<&mut viveco::VivecoInstance>,
) {
    let delta = time.delta_secs();
    for mut instance in &mut instances {
        let Some(asset) = assets.get(&instance.asset) else {
            continue;
        };
        let count = asset.animations.len();
        if count == 0 {
            continue;
        }

        demo.times.resize(count, 0.0);
        for (index, time) in demo.times.iter_mut().enumerate() {
            let duration = asset.animations[index].duration;
            *time = if duration > 0.0 {
                (*time + delta) % duration
            } else {
                0.0
            };
        }

        demo.elapsed += delta;
        if demo.elapsed >= CYCLE_SECONDS {
            demo.elapsed -= CYCLE_SECONDS;
            demo.current = (demo.current + 1) % count;
            // Restart the animation that just became current.
            let current = demo.current;
            demo.times[current] = 0.0;
        }
        demo.current %= count;

        let current = demo.current;
        let previous = (current + count - 1) % count;
        let blend = (demo.elapsed / FADE_SECONDS).clamp(0.0, 1.0);

        instance.mixer.clear();
        if count > 1 && blend < 1.0 {
            instance
                .mixer
                .push((previous, demo.times[previous], 1.0 - blend));
        }
        instance.mixer.push((current, demo.times[current], blend));
    }
}
