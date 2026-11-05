//! Loads and animates the rigged `stick.viveco` example.
//!
//! The example asset lives in the workspace's `examples` directory, so the
//! asset server root is pointed there. The demo cycles through every animation
//! with a short crossfade, which exercises the `(index, time, weight)` instances
//! on [`VivecoAnimator`].
//!
//! Run with `cargo run -p viveco --example animate --features bevy`.

use bevy::asset::AssetPlugin;
use bevy::prelude::*;
use bevy::window::PrimaryWindow;
use bevy_vello::VelloPlugin;
use bevy_vello::prelude::*;
use viveco::{Asset, Path, Point, Shape, VivecoAnimator, VivecoPlugin};

/// How long each animation is shown before moving to the next one.
const CYCLE_SECONDS: f32 = 2.5;
/// Length of the crossfade at the start of each cycle.
const FADE_SECONDS: f32 = 0.5;
/// "Walk" is the second animation in stick.viveco.
const START_ANIMATION: usize = 1;

fn main() {
    App::new()
        .add_plugins(
            DefaultPlugins
                .set(AssetPlugin {
                    // `file_path` is resolved relative to the package root.
                    file_path: "../examples".to_owned(),
                    ..default()
                })
                .set(WindowPlugin {
                    primary_window: Some(Window {
                        title: "Viveco + Bevy".into(),
                        resolution: (720, 1000).into(),
                        ..default()
                    }),
                    ..default()
                }),
        )
        .add_plugins(VelloPlugin::default())
        .add_plugins(VivecoPlugin)
        .insert_resource(ClearColor(Color::srgb(0.10, 0.10, 0.12)))
        .init_resource::<Demo>()
        .add_systems(Startup, setup)
        .add_systems(Update, (fit_when_loaded, animate))
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
    commands.spawn((Camera2d, VelloView, Transform::from_xyz(0.0, 0.0, 1000.0)));

    let handle = asset_server.load::<Asset>("stick.viveco");
    commands.spawn((
        VivecoAnimator::from_animation(handle, START_ANIMATION),
        Transform::default(),
    ));
}

/// Fits the document into the window once its asset finishes loading.
fn fit_when_loaded(
    mut fitted: Local<bool>,
    windows: Query<&Window, With<PrimaryWindow>>,
    assets: Res<Assets<Asset>>,
    mut animators: Query<(&VivecoAnimator, &mut Transform)>,
) {
    if *fitted {
        return;
    }
    let Ok(window) = windows.single() else {
        return;
    };

    for (animator, mut transform) in &mut animators {
        let Some(asset) = assets.get(&animator.asset) else {
            continue;
        };
        let Some((min, max)) = bounds(asset) else {
            continue;
        };

        let width = (max[0] - min[0]).max(f32::EPSILON);
        let height = (max[1] - min[1]).max(f32::EPSILON);
        let scale = 0.9 * (window.width() / width).min(window.height() / height);
        let center = [(min[0] + max[0]) * 0.5, (min[1] + max[1]) * 0.5];

        // Asset space has Y pointing down, so centering moves the entity up.
        *transform = Transform::from_xyz(-scale * center[0], scale * center[1], 0.0)
            .with_scale(Vec3::splat(scale));
        *fitted = true;
    }
}

/// Advances every animation and rotates through them with a short crossfade.
fn animate(
    time: Res<Time>,
    assets: Res<Assets<Asset>>,
    mut demo: ResMut<Demo>,
    mut animators: Query<&mut VivecoAnimator>,
) {
    let delta = time.delta_secs();
    for mut animator in &mut animators {
        let Some(asset) = assets.get(&animator.asset) else {
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

        animator.mixer.clear();
        if count > 1 && blend < 1.0 {
            animator
                .mixer
                .push((previous, demo.times[previous], 1.0 - blend));
        }
        animator.mixer.push((current, demo.times[current], blend));
    }
}

/// Conservative bounds of all geometry, including Bézier control points.
fn bounds(asset: &Asset) -> Option<(Point, Point)> {
    let mut min = [f32::INFINITY; 2];
    let mut max = [f32::NEG_INFINITY; 2];
    for shape in &asset.shapes {
        visit_shape(shape, &mut min, &mut max);
    }
    min[0].is_finite().then_some((min, max))
}

fn visit_shape(shape: &Shape, min: &mut Point, max: &mut Point) {
    for path in &shape.paths {
        visit_path(path, min, max);
    }
    for child in &shape.children {
        visit_shape(child, min, max);
    }
}

fn visit_path(path: &Path, min: &mut Point, max: &mut Point) {
    if let Some(start) = path.start() {
        include(start, min, max);
    }
    for segment in &path.segments {
        if let Some(ctrl) = segment.ctrl1 {
            include(ctrl, min, max);
        }
        if let Some(ctrl) = segment.ctrl2 {
            include(ctrl, min, max);
        }
        include(segment.to, min, max);
    }
}

fn include(point: Point, min: &mut Point, max: &mut Point) {
    min[0] = min[0].min(point[0]);
    min[1] = min[1].min(point[1]);
    max[0] = max[0].max(point[0]);
    max[1] = max[1].max(point[1]);
}
