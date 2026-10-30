//! Loads the example document and displays it with Vello.
//!
//! `model` owns the document format, `render` turns it into a Vello scene, and
//! `editor` adds selection and control-point editing. This binary only wires
//! those up to Bevy.

use std::path::PathBuf;

use bevy::camera::primitives::Aabb;
use bevy::prelude::*;
use bevy::window::PrimaryWindow;
use bevy_vello::VelloPlugin;
use bevy_vello::prelude::*;

use vector_anim_app::editor::{
    DocumentBounds, DocumentScene, EditorPlugin, EditorState, fit_transform,
};
use vector_anim_app::model::Document;
use vector_anim_app::render::build_scene;
use vector_anim_app::sidebar::SidebarPlugin;
use vector_anim_app::style_panel::StylePanelPlugin;

const BACKGROUND: Color = Color::srgb_u8(0x1a, 0x1d, 0x24);

fn main() {
    App::new()
        .insert_resource(ClearColor(BACKGROUND))
        .add_plugins(DefaultPlugins.set(WindowPlugin {
            primary_window: Some(Window {
                title: "Vector Anim".into(),
                resolution: (900, 900).into(),
                ..default()
            }),
            ..default()
        }))
        .add_plugins(VelloPlugin::default())
        .add_plugins(EditorPlugin)
        .add_plugins(SidebarPlugin)
        .add_plugins(StylePanelPlugin)
        .add_systems(Startup, setup)
        .run();
}

fn setup(mut commands: Commands, window: Query<&Window, With<PrimaryWindow>>) {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/example.vecart.json");
    let document = Document::load_from_file(&path)
        .unwrap_or_else(|err| panic!("failed to load {path:?}: {err}"));

    let bounds = document
        .bounds()
        .expect("the example document contains no geometry");
    let scene = build_scene(&document);

    println!(
        "loaded {path:?}: {} layers, {}x{} units",
        document.layers.len(),
        bounds.width(),
        bounds.height()
    );

    let (viewport_width, viewport_height) = window
        .single()
        .map(|window| (window.width(), window.height()))
        .unwrap_or((900.0, 900.0));

    // `VelloScene2d` has no intrinsic bounds, so provide them for visibility
    // culling.
    let aabb = Aabb::from_min_max(
        Vec3::new(bounds.min[0], bounds.min[1], -1.0),
        Vec3::new(bounds.max[0], bounds.max[1], 1.0),
    );

    commands.insert_resource(DocumentBounds(bounds));
    commands.insert_resource(EditorState::new(document));
    commands.spawn((Camera2d, VelloView, Transform::from_xyz(0.0, 0.0, 1000.0)));
    commands.spawn((
        VelloScene2d::from(scene),
        fit_transform(bounds, viewport_width, viewport_height),
        aabb,
        DocumentScene,
    ));
}
