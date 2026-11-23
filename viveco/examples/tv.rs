//! Draws the animated `planet.viveco` inside the screen of `tv.viveco`.
//!
//! The TV is a [`viveco::VivecoInstance`] linked to a scene. The planet
//! is a child instance: [`viveco::VivecoChildOf`] names the TV's `"screen"`
//! shape, so the plugin paints the planet inside that shape's clip layer and Z
//! order instead of drawing it as a separate instance.
//!
//! The TV entity also carries a [`viveco::VivecoFragments`] buffer. Every frame
//! [`draw_scanlines`] queues a custom Vello scene targeting the TV's own
//! `"screen"` shape, so a scan-line overlay is clipped to the screen without a
//! separate entity.
//!
//! Run with `cargo run -p viveco --example tv --features bevy`.

use bevy::camera::ScalingMode;
use bevy::prelude::*;
use bevy_vello::VelloPlugin;
use bevy_vello::prelude::*;
use vello::kurbo::{Affine, BezPath, Stroke};

/// Shape and animation constants generated from the example assets by
/// `build.rs`.
mod planet {
    include!(concat!(env!("OUT_DIR"), "/planet.rs"));
}
mod tv {
    include!(concat!(env!("OUT_DIR"), "/tv.rs"));
}

/// Minimum world-space area the camera shows.
const MIN_VIEW_WIDTH: f32 = 380.0;
const MIN_VIEW_HEIGHT: f32 = 480.0;

/// Draws the overlay above the TV content, but below the tint & glare
const SCANLINE_Z: f32 = 9.0;
/// Half-width of the scan-line band, in the TV's local units. The screen clip
/// trims it, so it only has to be wide enough to cover the screen.
const SCANLINE_HALF_EXTENT: f32 = 180.0;
const SCANLINE_SPACING: f32 = 6.0;

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
        // Queue the custom scan-line scene before the plugin walks the tree.
        .add_systems(
            PostUpdate,
            draw_scanlines.in_set(viveco::VivecoSceneSystems::Build),
        )
        .run();
}

/// The planet animation state (just one t value).
#[derive(Component, Default)]
struct Orbit {
    /// Playhead position in seconds, always in `0..ORBIT_DURATION`.
    time: f32,
}

/// Marks a TV that draws a scan-line overlay on its screen.
#[derive(Component, Default)]
struct Scanlines {
    t: f32,
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
            // The scan-line overlay is drawn into the same entity's screen.
            viveco::VivecoFragments::default(),
            Scanlines::default(),
            viveco::VivecoStrokeWidthInPixels::new(2.0),
        ))
        .id();

    // The planet is drawn inside the TV's "screen" shape
    let planet_asset = asset_server.load::<viveco::Asset>("planet.viveco");
    commands.spawn((
        viveco::VivecoInstance::from_asset(planet_asset).with_transform(Affine::scale(0.8)),
        viveco::VivecoChildOf::shape(tv_entity, tv::shapes::SCREEN),
        viveco::VivecoStrokeWidthInPixels::new(2.0),
        Orbit::default(),
    ));
}

/// Queues a scan-line scene clipped to each TV's screen.
///
/// The scene is authored in the TV entity's local space. Targeting the
/// `"screen"` shape makes the plugin clip it to that shape and Z-order it with
/// the screen's children, so no separate scan-line entity is needed.
fn draw_scanlines(mut tvs: Query<(&mut Scanlines, &mut viveco::VivecoFragments)>) {
    for (mut scanlines, mut fragments) in &mut tvs {
        let mut path = BezPath::new();
        let mut y = -SCANLINE_HALF_EXTENT + scanlines.t;
        while y <= SCANLINE_HALF_EXTENT {
            let row = f64::from(y);
            let x = f64::from(SCANLINE_HALF_EXTENT);
            path.move_to((-x, row));
            path.line_to((x, row));
            y += SCANLINE_SPACING;
        }

        let mut scene = vello::Scene::new();
        scene.stroke(
            &Stroke::new(1.0),
            Affine::IDENTITY,
            vello::peniko::Color::from_rgba8(0, 0, 0, 48),
            None,
            &path,
        );
        fragments.push_at_shape(
            tv::shapes::SCREEN,
            viveco::SceneFragment::new(SCANLINE_Z, scene),
        );

        scanlines.t = (scanlines.t + 0.1).rem_euclid(SCANLINE_SPACING);
    }
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
