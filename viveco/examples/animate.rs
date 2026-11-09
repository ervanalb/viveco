//! Loads and animates the rigged `stick.viveco` example.
//!
//! The character always plays "Walk" at weight 1, with "Arm Swing" also at
//! weight 1 layered on top. An egui panel shows the four blend weights, controls
//! the cadence of Walk and Arm Swing together, and triggers the one-shot "Wave"
//! and "Shrug" animations.
//!
//! Each one-shot's weight follows a target with bounded speed and acceleration,
//! which gives a cubic-like ease in both directions. A click sets that
//! animation's target to 1 for one second; clicking again while it is playing
//! simply restarts the hold, so the weight reverses smoothly instead of
//! popping.
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
use bevy_egui::{EguiContexts, EguiPlugin, EguiPrimaryContextPass, egui};
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

/// The animations shown in the UI, in display order.
const ANIMATION_NAMES: [(usize, &str); 4] = [
    (WALK, "Walk"),
    (ARM_SWING, "Arm Swing"),
    (SHRUG, "Shrug"),
    (WAVE, "Wave"),
];

/// How long a button holds a one-shot's target at 1.
const ACTION_SECONDS: f32 = 1.0;
/// Weight-controller speed limit, in weight units per second.
const MAX_WEIGHT_SPEED: f32 = 3.;
/// Weight-controller acceleration limit, in weight units per second squared.
/// With the speed limit this reaches full weight in roughly one second.
const MAX_WEIGHT_ACCEL: f32 = 10.0;

// The example frames the character with fixed design-space constants (asset
// space is Y-down) rather than measuring geometry at runtime. The projections
// are large enough to contain every animation clip with a margin.
/// Center of the framed area, in asset space.
const CHARACTER_CENTER: Vec2 = Vec2::new(0.0, -600.0);
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
        .add_plugins(EguiPlugin::default())
        .insert_resource(ClearColor(Color::srgb(0.10, 0.10, 0.12)))
        .init_resource::<Demo>()
        .add_systems(Startup, setup)
        .add_systems(Update, animate)
        .add_systems(EguiPrimaryContextPass, animation_ui)
        .run();
}

/// A one-shot animation whose weight chases its target.
struct OneShot {
    /// Animation index in the asset.
    animation: usize,
    /// Current mixer weight.
    weight: f32,
    /// Current rate of weight change.
    velocity: f32,
    /// Where the weight is heading, `0` or `1`.
    target: f32,
    /// Seconds left with the target held at 1.
    hold: f32,
}

impl OneShot {
    fn new(animation: usize) -> Self {
        Self {
            animation,
            weight: 0.0,
            velocity: 0.0,
            target: 0.0,
            hold: 0.0,
        }
    }

    /// Moves `weight` toward `target` with bounded speed and acceleration.
    ///
    /// The desired speed follows the braking curve `sqrt(2 * a * |error|)`,
    /// which accelerates from rest, cruises, and eases into the target, roughly
    /// matching the cubic ease the example used before.
    fn step(&mut self, delta: f32) {
        self.hold = (self.hold - delta).max(0.0);
        if self.hold <= 0.0 {
            self.target = 0.0;
        }

        let error = self.target - self.weight;
        if error.abs() < 1e-4 && self.velocity.abs() < 1e-4 {
            self.weight = self.target;
            self.velocity = 0.0;
            return;
        }

        let desired = error.signum()
            * (2.0 * MAX_WEIGHT_ACCEL * error.abs())
                .sqrt()
                .min(MAX_WEIGHT_SPEED);
        let max_change = MAX_WEIGHT_ACCEL * delta;
        self.velocity += (desired - self.velocity).clamp(-max_change, max_change);

        let next = self.weight + self.velocity * delta;
        if (next - self.target).signum() != (self.weight - self.target).signum() {
            // Reaching or passing the target: stop exactly on it.
            self.weight = self.target;
            self.velocity = 0.0;
        } else {
            self.weight = next.clamp(0.0, 1.0);
        }
    }
}

/// State used by the example to layer the secondary animations over the walk.
#[derive(Resource)]
struct Demo {
    /// Playback speed for "Walk" and "Arm Swing". One-shots ignore it.
    cadence: f32,
    /// Per-animation playheads, indexed by animation index.
    times: Vec<f32>,
    wave: OneShot,
    shrug: OneShot,
}

impl Default for Demo {
    fn default() -> Self {
        Self {
            cadence: 1.0,
            times: vec![0.0; ANIMATION_NAMES.len()],
            wave: OneShot::new(WAVE),
            shrug: OneShot::new(SHRUG),
        }
    }
}

impl Demo {
    fn shot(&self, index: usize) -> Option<&OneShot> {
        if index == self.wave.animation {
            Some(&self.wave)
        } else if index == self.shrug.animation {
            Some(&self.shrug)
        } else {
            None
        }
    }

    fn shot_mut(&mut self, index: usize) -> Option<&mut OneShot> {
        if index == self.wave.animation {
            Some(&mut self.wave)
        } else if index == self.shrug.animation {
            Some(&mut self.shrug)
        } else {
            None
        }
    }

    /// Requests a one-shot, or extends one that is already playing.
    ///
    /// Triggering one one-shot clears the other's target, so only one plays at
    /// a time. The target is held at 1 for [`ACTION_SECONDS`]. The playhead
    /// only restarts once the animation has fully faded out, so clicking during
    /// a fade cannot snap the pose.
    fn trigger(&mut self, index: usize) {
        let other = match index {
            WAVE => SHRUG,
            SHRUG => WAVE,
            _ => {
                return;
            }
        };
        if let Some(shot) = self.shot_mut(other) {
            shot.target = 0.0;
            shot.hold = 0.0;
        }

        let inactive = self
            .shot(index)
            .is_some_and(|shot| shot.weight <= 1e-4 && shot.target <= 0.0);
        if inactive && let Some(time) = self.times.get_mut(index) {
            *time = 0.0;
        }
        if let Some(shot) = self.shot_mut(index) {
            shot.target = 1.0;
            shot.hold = ACTION_SECONDS;
        }
    }

    /// The playhead for one animation.
    fn time(&self, index: usize) -> f32 {
        self.times.get(index).copied().unwrap_or(0.0)
    }

    /// Arm Swing yields to whichever one-shots are active.
    fn arm_swing_weight(&self) -> f32 {
        (1.0 - self.wave.weight - self.shrug.weight).max(0.0)
    }

    /// How much weight the animation at `index` contributes to the mixer.
    fn weight(&self, index: usize) -> f32 {
        if index == WALK {
            1.0
        } else if index == ARM_SWING {
            self.arm_swing_weight()
        } else if let Some(shot) = self.shot(index) {
            shot.weight
        } else {
            0.0
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

/// Drives the walk layer and the weight-driven secondary layers.
fn animate(
    time: Res<Time>,
    assets: Res<Assets<viveco::Asset>>,
    mut demo: ResMut<Demo>,
    mut instances: Query<&mut viveco::VivecoInstance>,
) {
    let delta = time.delta_secs();

    demo.wave.step(delta);
    demo.shrug.step(delta);

    // The example has one instance; take the clip durations from it.
    let durations: Vec<f32> = instances
        .iter()
        .find_map(|instance| assets.get(&instance.asset))
        .map(|asset| {
            asset
                .animations
                .iter()
                .map(|animation| animation.duration)
                .collect()
        })
        .unwrap_or_default();
    let playhead_count = durations.len().max(demo.times.len());
    demo.times.resize(playhead_count, 0.0);

    for (index, duration) in durations.iter().enumerate() {
        if *duration <= 0.0 {
            demo.times[index] = 0.0;
            continue;
        }
        let delta = if index == WALK || index == ARM_SWING {
            delta * demo.cadence
        } else {
            // One-shots play at normal speed.
            delta
        };
        let advanced = demo.times[index] + delta;
        demo.times[index] = if index == WALK || index == ARM_SWING {
            advanced % duration
        } else {
            // One-shots play once and hold their final pose.
            advanced.min(*duration)
        };
    }

    let arm_swing = demo.arm_swing_weight();
    for mut instance in &mut instances {
        instance.mixer.clear();

        // The walk always plays at full weight.
        instance.mixer.push((WALK, demo.time(WALK), 1.0));
        if arm_swing > 0.0 {
            instance
                .mixer
                .push((ARM_SWING, demo.time(ARM_SWING), arm_swing));
        }
        if demo.wave.weight > 0.0 {
            instance
                .mixer
                .push((WAVE, demo.time(WAVE), demo.wave.weight));
        }
        if demo.shrug.weight > 0.0 {
            instance
                .mixer
                .push((SHRUG, demo.time(SHRUG), demo.shrug.weight));
        }
    }
}

/// Read-only weights plus the cadence slider and one-shot buttons.
fn animation_ui(mut contexts: EguiContexts, mut demo: ResMut<Demo>) {
    let Ok(context) = contexts.ctx_mut() else {
        return;
    };

    egui::Window::new("Animations")
        .resizable(false)
        .show(context, |ui| {
            ui.label("Blend weights");
            for (index, name) in ANIMATION_NAMES {
                let weight = demo.weight(index);
                ui.add(egui::ProgressBar::new(weight).text(format!("{name}: {weight:.2}")));
            }

            ui.separator();
            ui.add(egui::Slider::new(&mut demo.cadence, 0.0..=3.0).text("Walk cadence"));

            ui.separator();
            ui.horizontal(|ui| {
                if ui.button("Wave").clicked() {
                    demo.trigger(WAVE);
                }
                if ui.button("Shrug").clicked() {
                    demo.trigger(SHRUG);
                }
            });
        });
}
