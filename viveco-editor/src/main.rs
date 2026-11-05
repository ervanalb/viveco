//! Starts with a blank document and displays it with Vello.
//!
//! `model` owns the document format, `render` turns it into a Vello scene, and
//! `editor` adds selection and control-point editing. This binary only wires
//! those up to Bevy.

use bevy::camera::primitives::Aabb;
use bevy::camera::visibility::NoFrustumCulling;
use bevy::prelude::*;
use bevy::window::PrimaryWindow;
use bevy_vello::VelloPlugin;
use bevy_vello::prelude::*;

use viveco::Asset;
use viveco_editor::animate_panel::AnimatePanelPlugin;
use viveco_editor::animate_sidebar::AnimateSidebarPlugin;
use viveco_editor::bounds::bounds_for;
use viveco_editor::clipboard::ClipboardPlugin;
use viveco_editor::dialogs::DialogPlugin;
use viveco_editor::editor::{
    DocumentBounds, DocumentScene, EditorPlugin, EditorState, fit_transform,
};
use viveco_editor::file::{DocumentFile, FilePlugin};
use viveco_editor::history::{History, HistoryPlugin};
use viveco_editor::menu_bar::MenuBarPlugin;
use viveco_editor::mode_tabs::ModeTabsPlugin;
use viveco_editor::rig::RigPlugin;
use viveco_editor::rig_panel::RigPanelPlugin;
use viveco_editor::rig_sidebar::RigSidebarPlugin;
use viveco_editor::sidebar::SidebarPlugin;
use viveco_editor::style_panel::StylePanelPlugin;
use viveco_editor::theme;
use viveco_editor::timeline::TimelinePlugin;
use viveco_editor::toolbar::ToolbarPlugin;

/// Initial window size, in logical pixels.
const WINDOW_SIZE: u32 = 900;

fn main() {
    App::new()
        .insert_resource(ClearColor(theme::CANVAS_BG))
        .add_plugins(DefaultPlugins.set(WindowPlugin {
            primary_window: Some(Window {
                title: "Viveco Editor".into(),
                resolution: (WINDOW_SIZE, WINDOW_SIZE).into(),
                ..default()
            }),
            ..default()
        }))
        .add_plugins(VelloPlugin::default())
        .insert_resource(DocumentFile::untitled())
        .add_plugins(EditorPlugin)
        .add_plugins(SidebarPlugin)
        .add_plugins(StylePanelPlugin)
        .add_plugins(ToolbarPlugin)
        .add_plugins(ModeTabsPlugin)
        .add_plugins(RigPlugin)
        .add_plugins(RigSidebarPlugin)
        .add_plugins(RigPanelPlugin)
        .add_plugins(AnimateSidebarPlugin)
        .add_plugins(AnimatePanelPlugin)
        .add_plugins(TimelinePlugin)
        .add_plugins(MenuBarPlugin)
        .add_plugins(FilePlugin)
        .add_plugins(DialogPlugin)
        .add_plugins(HistoryPlugin)
        .add_plugins(ClipboardPlugin)
        .add_systems(Startup, setup)
        .run();
}

fn setup(mut commands: Commands, window: Query<&Window, With<PrimaryWindow>>) {
    let document = Asset::new();
    let bounds = bounds_for(&document);

    let (viewport_width, viewport_height) = window
        .single()
        .map(|window| (window.width(), window.height()))
        .unwrap_or((WINDOW_SIZE as f32, WINDOW_SIZE as f32));
    let transform = fit_transform(bounds, viewport_width, viewport_height);
    let scene = document.into_scene(1.0 / transform.scale.x);

    // `VelloScene2d` has no intrinsic bounds, so provide them for picking.
    let aabb = Aabb::from_min_max(
        Vec3::new(bounds.min[0], bounds.min[1], -1.0),
        Vec3::new(bounds.max[0], bounds.max[1], 1.0),
    );

    commands.insert_resource(DocumentBounds(bounds));
    commands.insert_resource(History::new(document.clone()));
    commands.insert_resource(EditorState::new(document));
    commands.spawn((Camera2d, VelloView, Transform::from_xyz(0.0, 0.0, 1000.0)));
    commands.spawn((
        VelloScene2d::from(scene),
        transform,
        aabb,
        // Vello renders the scene with its own Y-flip, so the entity's `Aabb`
        // does not describe where the document actually appears. Frustum
        // culling against it can hide the whole document once panning or
        // zooming moves the mismatch into view, and there is nothing else on
        // screen to cull against anyway.
        NoFrustumCulling,
        DocumentScene,
    ));
}
