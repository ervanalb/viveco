//! Renders the stick figure with two animations blended additively.
//!
//! `Walk` drives the legs and body, while `Arm Swing` drives the arms. The two
//! clips animate disjoint bones, so layering them both at full weight produces
//! a walk cycle with swinging arms from a single source document.
//!
//! Run with:
//!
//! ```sh
//! cargo run -p viveco-bevy --example stick
//! ```

use bevy::prelude::*;
use bevy_vello::{VelloPlugin, prelude::*};
use viveco_bevy::*;

/// The compiled document, embedded at compile time. The path is resolved
/// relative to this crate's `Cargo.toml`.
static STICK: &[u8] = viveco!("../stick.json");

const WINDOW_SIZE: u32 = 720;

fn main() {
    App::new()
        .insert_resource(ClearColor(Color::srgb(0.97, 0.97, 0.95)))
        .add_plugins(DefaultPlugins.set(WindowPlugin {
            primary_window: Some(Window {
                title: "viveco — stick".into(),
                resolution: (WINDOW_SIZE, WINDOW_SIZE).into(),
                ..default()
            }),
            ..default()
        }))
        .add_plugins(VelloPlugin::default())
        .add_plugins(VivecoPlugin)
        .add_systems(Startup, setup)
        .run();
}

fn setup(mut commands: Commands, mut assets: ResMut<Assets<VivecoAsset>>, window: Query<&Window>) {
    // Read the bounds from the embedded bytes before moving them into the
    // asset store, so the scene can be framed automatically.
    let bounds = Viveco::parse(STICK)
        .expect("stick.json is valid")
        .bounds()
        .unwrap_or(Bounds {
            min: [0.0, 0.0],
            max: [1.0, 1.0],
        });

    let art = assets.add(VivecoAsset::from_static(STICK));

    let (width, height) = window
        .single()
        .map(|window| (window.width(), window.height()))
        .unwrap_or((WINDOW_SIZE as f32, WINDOW_SIZE as f32));

    commands.spawn((Camera2d, VelloView, Transform::from_xyz(0.0, 0.0, 1000.0)));

    commands.spawn((
        VivecoInstance::new(art)
            // Animation index 1 is `Walk`, index 2 is `Arm Swing`.
            .with_layer(1, 1.0, 0.0)
            .with_layer(2, 1.0, 0.0),
        fit_transform(bounds, width, height),
    ));
}

/// Fits the document's bounds into the viewport. Document Y points down, so
/// moving the centre to the screen centre negates the Y offset.
fn fit_transform(bounds: Bounds, viewport_width: f32, viewport_height: f32) -> Transform {
    let doc_width = bounds.width().max(f32::EPSILON);
    let doc_height = bounds.height().max(f32::EPSILON);
    let scale = 0.9 * (viewport_width / doc_width).min(viewport_height / doc_height);
    let center = bounds.center();

    Transform::from_xyz(-scale * center[0], scale * center[1], 0.0).with_scale(Vec3::splat(scale))
}
