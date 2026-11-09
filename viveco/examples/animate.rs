//! Loads and animates the rigged `stick.viveco` example.
//!
//! The character always plays "Walk" at weight 1, with "Arm Swing" also at
//! weight 1 layered on top. Every few seconds the secondary layer cross-fades
//! from "Arm Swing" to "Shrug" or "Wave", alternating between the two. The
//! one-shot specials fade in over one second, then immediately fade out over
//! one second to return to "Arm Swing", using cubic easing in both directions.
//!
//! The camera is framed with fixed design-space constants and
//! [`ScalingMode::AutoMin`], so it zooms with the window and keeps the
//! character in view as the window resizes, without measuring geometry.
//!
//! The example asset lives in the workspace's `examples` directory, so the
//! asset server root is pointed there. `build.rs` generates animation, shape,
//! and bone constants from that file, so the demo refers to
//! `stick::animations::WALK` instead of a hard-coded index. The demo opts into
//! a default scene, so its instance does not need to name one.
//!
//! Run with `cargo run -p viveco --example animate --features bevy`.

use bevy::camera::ScalingMode;
use bevy::prelude::*;
use bevy_vello::VelloPlugin;
use bevy_vello::prelude::*;

/// Animation, shape, and bone constants generated from `stick.viveco` by `build.rs`.
mod stick {
    include!(concat!(env!("OUT_DIR"), "/stick.rs"));
}

const WALK: usize = stick::animations::WALK;
const ARM_SWING: usize = stick::animations::ARM_SWING;
const SHRUG: usize = stick::animations::SHRUG;
const WAVE: usize = stick::animations::WAVE;

/// How long the secondary layer stays on "Arm Swing" before trying a special.
const ARM_SWING_SECONDS: f32 = 4.0;
/// Length of each half of a one-shot's entrance and exit cross-fade.
const FADE_SECONDS: f32 = 1.0;
/// How long a special holds at full weight between fading in and out.
const SPECIAL_HOLD_SECONDS: f32 = 0.0;

// The example frames the character with fixed design-space constants (asset
// space is Y-down) rather than measuring geometry at runtime. The projections
// are large enough to contain every animation clip with a margin.
/// Center of the framed area, in asset space.
const CHARACTER_CENTER: Vec2 = Vec2::new(325.0, 355.0);
/// Minimum world-space area the camera shows.
const MIN_VIEW_WIDTH: f32 = 760.0;
const MIN_VIEW_HEIGHT: f32 = 1280.0;

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

/// State used by the example to layer a secondary animation over the walk.
#[derive(Resource)]
struct Demo {
    /// Shared playhead. All clips are sampled from it, keeping them in sync.
    clock: f32,
    /// The secondary clip currently playing.
    secondary: usize,
    /// When `secondary` started, for clips that play once.
    secondary_start: f32,
    /// The clip being cross-faded away from while `blend < 1.0`.
    previous: usize,
    /// When `previous` started, for clips that play once.
    previous_start: f32,
    /// Cross-fade progress from `previous` to `secondary`, in `0..=1`.
    blend: f32,
    /// Seconds until the secondary clip switches.
    until_switch: f32,
    /// The special clip to play after the next "Arm Swing" stretch.
    next_special: usize,
}

impl Default for Demo {
    fn default() -> Self {
        Self {
            clock: 0.0,
            secondary: ARM_SWING,
            secondary_start: 0.0,
            previous: ARM_SWING,
            previous_start: 0.0,
            blend: 1.0,
            until_switch: ARM_SWING_SECONDS,
            next_special: SHRUG,
        }
    }
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
        Transform::from_xyz(CHARACTER_CENTER.x, -CHARACTER_CENTER.y, 100.0),
    ));

    // Mark one scene as the default; for VivecoInstances without explicit `VivecoSceneOf`.
    commands.spawn((viveco::VivecoScene::default(), viveco::VivecoDefaultScene));

    let handle = asset_server.load::<viveco::Asset>("stick.viveco");
    commands.spawn((
        viveco::VivecoInstance::from_animation(handle, WALK),
        viveco::VivecoStrokeWidthInPixels::default(),
    ));
}

/// Drives the walk layer and the cross-fading secondary layer.
fn animate(
    time: Res<Time>,
    assets: Res<Assets<viveco::Asset>>,
    mut demo: ResMut<Demo>,
    mut instances: Query<&mut viveco::VivecoInstance>,
) {
    let delta = time.delta_secs();
    demo.clock += delta;

    // Switch the secondary clip between "Arm Swing" and a special, alternating
    // which special is used.
    demo.until_switch -= delta;
    if demo.until_switch <= 0.0 {
        demo.previous = demo.secondary;
        demo.previous_start = demo.secondary_start;
        demo.secondary = if demo.secondary == ARM_SWING {
            let special = demo.next_special;
            demo.next_special = if special == SHRUG { WAVE } else { SHRUG };
            special
        } else {
            ARM_SWING
        };
        demo.secondary_start = demo.clock;
        demo.until_switch = if demo.secondary == ARM_SWING {
            ARM_SWING_SECONDS
        } else {
            // Fade in, hold, then start fading out immediately.
            FADE_SECONDS + SPECIAL_HOLD_SECONDS
        };
        demo.blend = 0.0;
    }
    if demo.blend < 1.0 {
        demo.blend = (demo.blend + delta / FADE_SECONDS).min(1.0);
    }
    let blend = ease_in_out_cubic(demo.blend);

    for mut instance in &mut instances {
        let Some(asset) = assets.get(&instance.asset) else {
            continue;
        };

        instance.mixer.clear();

        // The walk always plays at full weight, phase-locked to the clock.
        instance
            .mixer
            .push((WALK, clip_time(asset, WALK, demo.clock, 0.0), 1.0));

        // The secondary layer also has full weight, split between the outgoing
        // and incoming clips during a cross-fade. A special therefore reaches
        // weight 1 at its peak, while the arm swing is displaced for the same
        // amount. The split is eased so the fade starts and ends softly.
        if blend < 1.0 {
            instance.mixer.push((
                demo.previous,
                clip_time(asset, demo.previous, demo.clock, demo.previous_start),
                1.0 - blend,
            ));
        }
        instance.mixer.push((
            demo.secondary,
            clip_time(asset, demo.secondary, demo.clock, demo.secondary_start),
            blend,
        ));
    }
}

/// Cubic ease-in-out: slow at both ends, fast in the middle.
fn ease_in_out_cubic(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    if t < 0.5 {
        4.0 * t * t * t
    } else {
        1.0 - (-2.0 * t + 2.0).powi(3) / 2.0
    }
}

/// Samples a clip from the shared clock.
///
/// "Walk" and "Arm Swing" loop on the clock, so they stay synchronized.
/// One-shot clips (the specials) play from their own start time and then hold
/// their final pose, because the sampler clamps times past the duration.
fn clip_time(asset: &viveco::Asset, index: usize, clock: f32, start: f32) -> f32 {
    asset.animations.get(index).map_or(0.0, |animation| {
        if animation.duration <= 0.0 {
            0.0
        } else if index == WALK || index == ARM_SWING {
            clock % animation.duration
        } else {
            clock - start
        }
    })
}
