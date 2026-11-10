//! Loads and animates the rigged `stick.viveco` example.
//!
//! The character always plays "Walk" at weight 1, with "Arm Swing" layered on
//! top. An egui panel shows the four blend weights, controls the cadence of
//! Walk and Arm Swing together, and triggers the one-shot "Wave" and "Shrug"
//! animations.
//!
//! [`StickAnimation`] sits beside the [`viveco::VivecoInstance`] and owns the
//! example's animation state: a playhead shared by Walk and Arm Swing plus one
//! controller per one-shot. Every frame [`animate`] advances that state and
//! refills the instance's `mixer` from it, reusing the existing allocation.
//!
//! A click holds a one-shot's target at 1 for one second; clicking again while
//! it is playing simply restarts the hold, so the weight reverses smoothly
//! instead of popping. Each one-shot owns an [`AccelEasing`], which limits
//! speed and acceleration to give a cubic-like ease in both directions.
//! Every clip in the asset is cyclic, so playheads wrap at the clip duration;
//! the one-shots advance only while they have weight, keeping them playing
//! through the hold and fade-out rather than freezing on a final pose.
//!
//! The camera is framed with fixed design-space constants and
//! [`ScalingMode::AutoMin`], so it zooms with the window and keeps the
//! character in view as the window resizes, without measuring geometry.
//!
//! The example asset lives in the workspace's `examples` directory, so the
//! asset server root is pointed there. `build.rs` generates animation indices
//! and durations plus shape and bone constants from that file, so the demo
//! refers to `stick::animations::WALK` and `stick::animations::WALK_DURATION`
//! instead of hard-coded values. The demo opts into a default scene, so its
//! instance does not need to name one.
//!
//! Run with `cargo run -p viveco --example compound --features bevy`.

use bevy::camera::ScalingMode;
use bevy::prelude::*;
use bevy_egui::{EguiContexts, EguiPlugin, EguiPrimaryContextPass, egui};
use bevy_vello::VelloPlugin;
use bevy_vello::prelude::*;
use viveco::{AccelEasing, AnimationInstance};

/// Animation, shape, and bone constants generated from `stick.viveco` by `build.rs`.
mod stick {
    include!(concat!(env!("OUT_DIR"), "/stick.rs"));
}

/// How long a button holds a one-shot's target at 1.
const ACTION_SECONDS: f32 = 1.0;

// The example frames the character with fixed design-space constants (asset
// space is Y-down) rather than measuring geometry at runtime. The projections
// are large enough to contain every animation clip with a margin.
/// Center of the framed area, in asset space.
const CHARACTER_CENTER: Vec2 = Vec2::new(0.0, 600.0);
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
        .add_systems(Startup, setup)
        // Build this frame's mixer before the plugin poses the instance.
        .add_systems(Update, animate.before(viveco::VivecoSceneSystems::Build))
        .add_systems(EguiPrimaryContextPass, animation_ui)
        .run();
}

/// A one-shot animation: its playhead, hold timer, and weight easing.
struct OneShot {
    /// Playhead position in seconds.
    time: f32,
    /// Seconds left with the target held at 1.
    hold: f32,
    /// Eases the weight toward 1 while held and back to 0 afterwards.
    easing: AccelEasing,
}

impl Default for OneShot {
    fn default() -> Self {
        Self {
            time: 0.0,
            hold: 0.0,
            // These limits bring the weight to full in roughly one second.
            easing: AccelEasing::new(3.0, 10.0),
        }
    }
}

impl OneShot {
    /// Advances the hold, the weight, and the playhead by one frame.
    fn step(&mut self, delta: f32, duration: f32) {
        self.hold = (self.hold - delta).max(0.0);
        let target = if self.hold > 0.0 { 1.0 } else { 0.0 };
        self.easing.step(target, delta);

        // Keep cycling while the animation has weight, so it plays through the
        // hold and fade-out and then freezes.
        if self.easing.value > 0.0 {
            self.time = (self.time + delta).rem_euclid(duration);
        }
    }

    /// Starts or extends a hold.
    ///
    /// The playhead restarts only from rest, so clicking during a fade cannot
    /// snap the pose.
    fn trigger(&mut self) {
        if self.hold <= 0.0 && self.easing.value <= 1e-4 {
            self.time = 0.0;
        }
        self.hold = ACTION_SECONDS;
    }

    /// Ends the hold, letting the animation fade out.
    fn cancel(&mut self) {
        self.hold = 0.0;
    }
}

/// The stick character's animation state.
///
/// This is attached beside the [`viveco::VivecoInstance`]. Each frame
/// [`animate`] advances the state, then rebuilds the instance's mixer from it,
/// so the mixer is always a fresh description of this frame's blend.
#[derive(Component)]
struct StickAnimation {
    /// Playback speed for Walk and Arm Swing. One-shots ignore it.
    cadence: f32,
    /// Playhead shared by Walk and Arm Swing; their clips have equal length.
    loop_time: f32,
    wave: OneShot,
    shrug: OneShot,
}

impl Default for StickAnimation {
    fn default() -> Self {
        Self {
            cadence: 1.0,
            loop_time: 0.0,
            wave: OneShot::default(),
            shrug: OneShot::default(),
        }
    }
}

impl StickAnimation {
    /// Advances the playheads by a frame of length `delta`.
    fn step(&mut self, delta: f32) {
        self.loop_time =
            (self.loop_time + delta * self.cadence).rem_euclid(stick::animations::WALK_DURATION);
        self.wave.step(delta, stick::animations::WAVE_DURATION);
        self.shrug.step(delta, stick::animations::SHRUG_DURATION);
    }

    /// Arm Swing yields the weight the one-shots do not use.
    fn arm_swing_weight(&self) -> f32 {
        (1.0 - self.wave.easing.value - self.shrug.easing.value).max(0.0)
    }

    /// Triggers Wave, fading Shrug out.
    fn trigger_wave(&mut self) {
        self.shrug.cancel();
        self.wave.trigger();
    }

    /// Triggers Shrug, fading Wave out.
    fn trigger_shrug(&mut self) {
        self.wave.cancel();
        self.shrug.trigger();
    }

    /// Rebuilds `mixer` from the tracked state, reusing its allocation.
    fn write_mixer(&self, mixer: &mut Vec<AnimationInstance>) {
        mixer.clear();

        let arm_swing = self.arm_swing_weight();
        // Walk always plays at full weight as the base layer.
        mixer.push((stick::animations::WALK, self.loop_time, 1.0));
        if arm_swing > 0.0 {
            mixer.push((stick::animations::ARM_SWING, self.loop_time, arm_swing));
        }
        if self.wave.easing.value > 0.0 {
            mixer.push((
                stick::animations::WAVE,
                self.wave.time,
                self.wave.easing.value,
            ));
        }
        if self.shrug.easing.value > 0.0 {
            mixer.push((
                stick::animations::SHRUG,
                self.shrug.time,
                self.shrug.easing.value,
            ));
        }
    }

    /// The blend weights in the order the UI displays them.
    fn weights(&self) -> [(&'static str, f32); 4] {
        [
            ("Walk", 1.0),
            ("Arm Swing", self.arm_swing_weight()),
            ("Shrug", self.shrug.easing.value),
            ("Wave", self.wave.easing.value),
        ]
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
        Transform::from_xyz(CHARACTER_CENTER.x, CHARACTER_CENTER.y, 100.0),
    ));

    // Mark one scene as the default; for VivecoInstances without explicit `VivecoSceneOf`.
    commands.spawn((viveco::VivecoScene::default(), viveco::VivecoDefaultScene));

    // The instance starts at rest; `animate` supplies the mixer from the
    // `StickAnimation` state every frame.
    let handle = asset_server.load::<viveco::Asset>("stick.viveco");
    commands.spawn((
        viveco::VivecoInstance::from_asset(handle),
        StickAnimation::default(),
        viveco::VivecoStrokeWidthInPixels::default(),
    ));
}

/// Advances the animation state and rebuilds each instance's mixer every frame.
fn animate(
    time: Res<Time>,
    mut characters: Query<(&mut viveco::VivecoInstance, &mut StickAnimation)>,
) {
    let delta = time.delta_secs();

    for (mut instance, mut animation) in &mut characters {
        animation.step(delta);
        animation.write_mixer(&mut instance.mixer);
    }
}

/// Read-only weights plus the cadence slider and one-shot buttons.
fn animation_ui(mut contexts: EguiContexts, mut characters: Query<&mut StickAnimation>) {
    let Ok(context) = contexts.ctx_mut() else {
        return;
    };
    let Ok(mut animation) = characters.single_mut() else {
        return;
    };

    egui::Window::new("Animations")
        .resizable(false)
        .show(context, |ui| {
            ui.label("Blend weights");
            for (name, weight) in animation.weights() {
                ui.add(egui::ProgressBar::new(weight).text(format!("{name}: {weight:.2}")));
            }

            ui.separator();
            ui.add(egui::Slider::new(&mut animation.cadence, 0.0..=3.0).text("Walk cadence"));

            ui.separator();
            ui.horizontal(|ui| {
                if ui.button("Wave").clicked() {
                    animation.trigger_wave();
                }
                if ui.button("Shrug").clicked() {
                    animation.trigger_shrug();
                }
            });
        });
}
