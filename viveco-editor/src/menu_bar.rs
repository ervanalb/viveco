//! Top menu bar exposing the file and edit actions.
//!
//! Bevy ships a menu widget (`bevy::ui_widgets::menu`) built on picking and
//! focus navigation. The editor's other panels use plain [`Button`] entities
//! with [`Interaction`], so this menu bar follows the same pattern instead of
//! pulling in the widget's popup/focus plumbing. Each menu label owns a
//! dropdown that is hidden until the label is pressed.

use bevy::prelude::*;
use bevy::window::PrimaryWindow;

use crate::clipboard::{Clipboard, copy_selection, paste_clipboard};
use crate::dialogs::FileDialogs;
use crate::editor::{DocumentBounds, EditorState, MENU_BAR_HEIGHT, Viewport};
use crate::file::{
    DocumentFile, new_document, report_save, save_to_current, start_directory, suggested_file_name,
};
use crate::history::History;
use crate::theme;

/// Height of one dropdown row, in screen pixels.
const MENU_ROW_HEIGHT: f32 = 26.0;

/// Minimum width of a dropdown, in screen pixels.
const MENU_PANEL_WIDTH: f32 = 190.0;

/// Font size of menu labels and rows.
const MENU_FONT_SIZE: f32 = 12.0;

/// Font size of the shortcut hints beside menu rows.
const SHORTCUT_FONT_SIZE: f32 = 11.0;

pub struct MenuBarPlugin;

impl Plugin for MenuBarPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<MenuState>()
            .add_systems(Startup, spawn_menu_bar)
            .add_systems(
                Update,
                (
                    handle_menu_labels,
                    handle_menu_items,
                    close_menu_on_outside_click,
                    update_menu_visuals,
                    sync_menu_panels,
                )
                    .chain()
                    // Canvas input runs first so it still sees the menu as open
                    // during the click that closes it.
                    .after(crate::editor::CanvasInput),
            );
    }
}

/// An editor command that a menu row can invoke.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EditorAction {
    New,
    Open,
    ImportSvg,
    Save,
    SaveAs,
    Undo,
    Redo,
    Copy,
    Paste,
}

/// The menus in the bar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MenuKind {
    File,
    Edit,
}

/// A clickable menu label.
#[derive(Component, Clone, Copy)]
struct MenuLabel(MenuKind);

/// A dropdown belonging to a menu label.
#[derive(Component, Clone, Copy)]
struct MenuPanel(MenuKind);

/// A row inside a dropdown.
#[derive(Component, Clone, Copy)]
struct MenuItem(EditorAction);

/// Which dropdown is currently open, if any.
#[derive(Resource, Default)]
pub struct MenuState {
    open: Option<MenuKind>,
}

impl MenuState {
    /// Whether a dropdown is open and therefore owns pointer input.
    pub fn is_open(&self) -> bool {
        self.open.is_some()
    }
}

fn spawn_menu_bar(mut commands: Commands) {
    commands
        .spawn((
            Node {
                position_type: PositionType::Absolute,
                left: Val::Px(0.0),
                top: Val::Px(0.0),
                width: Val::Percent(100.0),
                height: Val::Px(MENU_BAR_HEIGHT),
                flex_direction: FlexDirection::Row,
                align_items: AlignItems::Center,
                padding: UiRect::horizontal(Val::Px(8.0)),
                column_gap: Val::Px(2.0),
                ..default()
            },
            BackgroundColor(theme::PANEL_BG),
            GlobalZIndex(50),
        ))
        .with_children(|bar| {
            spawn_menu(
                bar,
                MenuKind::File,
                "File",
                &[
                    ("New", EditorAction::New, "Ctrl+N"),
                    ("Open...", EditorAction::Open, "Ctrl+O"),
                    ("Import SVG...", EditorAction::ImportSvg, "Ctrl+I"),
                    ("Save", EditorAction::Save, "Ctrl+S"),
                    ("Save As...", EditorAction::SaveAs, "Ctrl+Shift+S"),
                ],
            );
            spawn_menu(
                bar,
                MenuKind::Edit,
                "Edit",
                &[
                    ("Undo", EditorAction::Undo, "Ctrl+Z"),
                    ("Redo", EditorAction::Redo, "Ctrl+Shift+Z"),
                    ("Copy", EditorAction::Copy, "Ctrl+C"),
                    ("Paste", EditorAction::Paste, "Ctrl+V"),
                ],
            );
        });
}

fn spawn_menu(
    bar: &mut ChildSpawnerCommands,
    menu: MenuKind,
    label: &str,
    items: &[(&str, EditorAction, &str)],
) {
    bar.spawn((
        Button,
        MenuLabel(menu),
        Node {
            height: Val::Px(MENU_BAR_HEIGHT),
            padding: UiRect::horizontal(Val::Px(10.0)),
            align_items: AlignItems::Center,
            justify_content: JustifyContent::Center,
            ..default()
        },
        BackgroundColor(Color::NONE),
    ))
    .with_children(|button| {
        button.spawn((
            Text::new(label),
            TextFont::from_font_size(MENU_FONT_SIZE),
            TextColor(theme::TEXT),
        ));

        // The dropdown hangs below the label and is hidden until opened.
        button
            .spawn((
                MenuPanel(menu),
                Node {
                    position_type: PositionType::Absolute,
                    top: Val::Percent(100.0),
                    left: Val::Px(0.0),
                    display: Display::None,
                    flex_direction: FlexDirection::Column,
                    min_width: Val::Px(MENU_PANEL_WIDTH),
                    padding: UiRect::vertical(Val::Px(4.0)),
                    border: UiRect::all(Val::Px(1.0)),
                    ..default()
                },
                BackgroundColor(theme::PANEL_BG),
                BorderColor::all(theme::BORDER_FAINT),
                GlobalZIndex(60),
            ))
            .with_children(|panel| {
                for (label, action, shortcut) in items {
                    panel
                        .spawn((
                            Button,
                            MenuItem(*action),
                            Node {
                                height: Val::Px(MENU_ROW_HEIGHT),
                                padding: UiRect::horizontal(Val::Px(10.0)),
                                align_items: AlignItems::Center,
                                justify_content: JustifyContent::SpaceBetween,
                                column_gap: Val::Px(20.0),
                                ..default()
                            },
                            BackgroundColor(Color::NONE),
                        ))
                        .with_children(|row| {
                            row.spawn((
                                Text::new(*label),
                                TextFont::from_font_size(MENU_FONT_SIZE),
                                TextColor(theme::TEXT),
                            ));
                            row.spawn((
                                Text::new(*shortcut),
                                TextFont::from_font_size(SHORTCUT_FONT_SIZE),
                                TextColor(theme::MUTED),
                            ));
                        });
                }
            });
    });
}

/// Opens, switches, or closes a dropdown when its label is pressed.
fn handle_menu_labels(
    labels: Query<(&Interaction, &MenuLabel), Changed<Interaction>>,
    mut state: ResMut<MenuState>,
) {
    for (interaction, label) in &labels {
        if *interaction == Interaction::Pressed {
            state.open = if state.open == Some(label.0) {
                None
            } else {
                Some(label.0)
            };
        }
    }
}

/// Runs the action of a pressed row and closes the dropdown.
fn handle_menu_items(
    items: Query<(&Interaction, &MenuItem), Changed<Interaction>>,
    mut state: ResMut<MenuState>,
    mut editor: ResMut<EditorState>,
    mut bounds: ResMut<DocumentBounds>,
    mut viewport: ResMut<Viewport>,
    mut history: ResMut<History>,
    mut file: ResMut<DocumentFile>,
    mut clipboard: ResMut<Clipboard>,
    mut dialogs: ResMut<FileDialogs>,
) {
    for (interaction, item) in &items {
        if *interaction != Interaction::Pressed {
            continue;
        }
        state.open = None;
        run_action(
            item.0,
            &mut editor,
            &mut bounds,
            &mut viewport,
            &mut history,
            &mut file,
            &mut clipboard,
            &mut dialogs,
        );
    }
}

fn run_action(
    action: EditorAction,
    editor: &mut EditorState,
    bounds: &mut DocumentBounds,
    viewport: &mut Viewport,
    history: &mut History,
    file: &mut DocumentFile,
    clipboard: &mut Clipboard,
    dialogs: &mut FileDialogs,
) {
    match action {
        EditorAction::New => {
            dialogs.cancel();
            new_document(editor, bounds, viewport, history, file);
        }
        EditorAction::Open => {
            dialogs.open(start_directory(file));
        }
        EditorAction::ImportSvg => {
            dialogs.import_svg(start_directory(file));
        }
        EditorAction::Save => match save_to_current(editor, file) {
            Some(result) => report_save(result, file),
            None => {
                dialogs.save_as(start_directory(file), suggested_file_name(file));
            }
        },
        EditorAction::SaveAs => {
            dialogs.save_as(start_directory(file), suggested_file_name(file));
        }
        EditorAction::Undo => {
            history.undo(editor);
        }
        EditorAction::Redo => {
            history.redo(editor);
        }
        EditorAction::Copy => {
            copy_selection(editor, clipboard);
        }
        EditorAction::Paste => {
            paste_clipboard(editor, clipboard);
        }
    }
}

/// Closes the open dropdown when the canvas or a panel below the bar is
/// clicked.
fn close_menu_on_outside_click(
    mouse: Res<ButtonInput<MouseButton>>,
    windows: Query<&Window, With<PrimaryWindow>>,
    mut state: ResMut<MenuState>,
) {
    if state.open.is_none() || !mouse.just_pressed(MouseButton::Left) {
        return;
    }
    let Ok(window) = windows.single() else {
        return;
    };
    let Some(cursor) = window.cursor_position() else {
        return;
    };
    if cursor.y >= MENU_BAR_HEIGHT {
        state.open = None;
    }
}

/// Highlights open labels, hovered rows, and hovered labels.
fn update_menu_visuals(
    state: Res<MenuState>,
    mut labels: Query<(&Interaction, &MenuLabel, &mut BackgroundColor), Without<MenuItem>>,
    mut items: Query<(&Interaction, &mut BackgroundColor), With<MenuItem>>,
) {
    for (interaction, label, mut background) in &mut labels {
        let highlighted = state.open == Some(label.0) || *interaction != Interaction::None;
        let color = if highlighted {
            theme::ACCENT_SELECTED
        } else {
            Color::NONE
        };
        if background.0 != color {
            background.0 = color;
        }
    }

    for (interaction, mut background) in &mut items {
        let color = match interaction {
            Interaction::Pressed => theme::ACCENT_SELECTED,
            Interaction::Hovered => theme::ACCENT_FAINT,
            Interaction::None => Color::NONE,
        };
        if background.0 != color {
            background.0 = color;
        }
    }
}

/// Shows only the open menu's dropdown.
fn sync_menu_panels(state: Res<MenuState>, mut panels: Query<(&MenuPanel, &mut Node)>) {
    for (panel, mut node) in &mut panels {
        let display = if state.open == Some(panel.0) {
            Display::Flex
        } else {
            Display::None
        };
        if node.display != display {
            node.display = display;
        }
    }
}
