//! File shortcuts: new, open, save, and save as.
//!
//! The editor works against at most one file at a time, tracked in
//! [`DocumentFile`]. Open and save use native dialogs (see [`crate::dialogs`]),
//! while save without a dialog overwrites the tracked path. A document created
//! with New has no path yet, so the first save asks where to put it.

use std::path::{Path, PathBuf};

use bevy::prelude::*;
use bevy::window::PrimaryWindow;

use crate::bounds::bounds_for;
use crate::dialogs::FileDialogs;
use crate::editor::{DocumentBounds, EditorState, TreeAddress, Viewport};
use crate::history::History;
use viveco::Asset;

/// Name used for a fresh document that has not been saved anywhere yet.
///
/// The extension selects the binary format, which is the default.
const UNTITLED_FILE: &str = "untitled.viveco";

/// Title shown when the document has no file.
const UNTITLED_TITLE: &str = "Untitled";

pub struct FilePlugin;

impl Plugin for FilePlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, (handle_file_shortcuts, sync_window_title).chain());
    }
}

/// The file the editor is currently associated with.
///
/// A [`None`] path means the document has never been saved and a save must ask
/// for a location.
#[derive(Resource, Clone, Debug)]
pub struct DocumentFile {
    pub path: Option<PathBuf>,
}

impl DocumentFile {
    /// A document that is not associated with any file yet.
    pub fn untitled() -> Self {
        Self { path: None }
    }

    /// The bare file name shown in the window title.
    pub fn display_name(&self) -> String {
        self.path
            .as_deref()
            .and_then(Path::file_name)
            .and_then(|name| name.to_str())
            .unwrap_or(UNTITLED_TITLE)
            .to_owned()
    }
}

/// The directory a dialog for `file` should start in.
pub fn start_directory(file: &DocumentFile) -> Option<PathBuf> {
    file.path
        .as_deref()
        .and_then(|path| path.parent())
        .map(Path::to_path_buf)
}

/// The default file name a save dialog should suggest for `file`.
pub fn suggested_file_name(file: &DocumentFile) -> String {
    file.path
        .as_deref()
        .and_then(Path::file_name)
        .and_then(|name| name.to_str())
        .unwrap_or(UNTITLED_FILE)
        .to_owned()
}

/// Replaces the editor's document and every piece of state derived from it.
pub(crate) fn install_document(
    editor: &mut EditorState,
    bounds: &mut DocumentBounds,
    viewport: &mut Viewport,
    history: &mut History,
    document: Asset,
) {
    *bounds = DocumentBounds(bounds_for(&document));
    *viewport = Viewport::default();
    history.reset(document.clone());
    editor.replace_document(document);
}

/// Replaces the document with an empty one that has no file yet.
pub fn new_document(
    editor: &mut EditorState,
    bounds: &mut DocumentBounds,
    viewport: &mut Viewport,
    history: &mut History,
    file: &mut DocumentFile,
) {
    file.path = None;
    install_document(editor, bounds, viewport, history, Asset::new());
}

/// Loads `path` and adopts it as the current file.
///
/// Returns `false` without touching the editor when the file cannot be read.
pub fn open_document(
    editor: &mut EditorState,
    bounds: &mut DocumentBounds,
    viewport: &mut Viewport,
    history: &mut History,
    file: &mut DocumentFile,
    path: &Path,
) -> bool {
    match Asset::load_from_file(path) {
        Ok(document) => {
            install_document(editor, bounds, viewport, history, document);
            file.path = Some(path.to_path_buf());
            true
        }
        Err(err) => {
            eprintln!("failed to load {path:?}: {err}");
            false
        }
    }
}

/// Imports the shapes of the SVG at `path` into the current document.
///
/// The shapes are appended above every existing top-level shape, and the first
/// imported shape becomes the selection. The import is recorded as one undo
/// step. Returns `false` when the file cannot be read or has no shapes.
pub fn import_svg_document(
    editor: &mut EditorState,
    bounds: &mut DocumentBounds,
    history: &mut History,
    path: &Path,
) -> bool {
    let imported = match Asset::load_svg_from_file(path) {
        Ok(asset) => asset,
        Err(err) => {
            eprintln!("failed to import {}: {err}", path.display());
            return false;
        }
    };
    if imported.shapes.is_empty() {
        eprintln!("no importable shapes in {}", path.display());
        return false;
    }

    // New top-level shapes go above every existing one, preserving the SVG's
    // own painting order among themselves.
    let highest = editor
        .document
        .shapes
        .iter()
        .map(|shape| shape.z)
        .filter(|z| z.is_finite())
        .fold(f32::NEG_INFINITY, f32::max);
    let base_z = if highest.is_finite() {
        highest + 1.0
    } else {
        0.0
    };

    let first_imported = editor.document.shapes.len();
    for (index, mut shape) in imported.shapes.into_iter().enumerate() {
        shape.z = base_z + index as f32;
        editor.document.shapes.push(shape);
    }

    *bounds = DocumentBounds(bounds_for(&editor.document));
    editor.structure_revision += 1;
    editor.select_node(Some(TreeAddress::Shape {
        shapes: vec![first_imported],
    }));
    history.observe(&editor.document, false);
    true
}

/// Writes the document to `path`, creating parent directories as needed, and
/// adopts it as the current file.
pub fn save_document_to(
    editor: &EditorState,
    file: &mut DocumentFile,
    path: &Path,
) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    editor.document.save_to_file(path)?;
    file.path = Some(path.to_path_buf());
    Ok(())
}

/// Saves to the file the editor is associated with.
///
/// Returns `None` when there is no file yet, so the caller should ask for one.
pub fn save_to_current(
    editor: &EditorState,
    file: &mut DocumentFile,
) -> Option<std::io::Result<()>> {
    let path = file.path.clone()?;
    Some(save_document_to(editor, file, &path))
}

/// Reports the outcome of a save in the terminal.
pub(crate) fn report_save(result: std::io::Result<()>, file: &DocumentFile) {
    match result {
        Ok(()) => println!("saved {:?}", file.path),
        Err(err) => eprintln!("failed to save {:?}: {err}", file.path),
    }
}

/// Ctrl+N / Ctrl+O / Ctrl+S / Ctrl+Shift+S.
fn handle_file_shortcuts(
    keys: Res<ButtonInput<KeyCode>>,
    mut editor: ResMut<EditorState>,
    mut bounds: ResMut<DocumentBounds>,
    mut viewport: ResMut<Viewport>,
    mut history: ResMut<History>,
    mut file: ResMut<DocumentFile>,
    mut dialogs: ResMut<FileDialogs>,
) {
    // While a tree row is being renamed, these shortcuts belong to the text
    // field.
    if editor.is_renaming() || !ctrl_held(&keys) {
        return;
    }

    if keys.just_pressed(KeyCode::KeyN) {
        // A dialog opened for the old document must not act on the new one.
        dialogs.cancel();
        new_document(
            &mut editor,
            &mut bounds,
            &mut viewport,
            &mut history,
            &mut file,
        );
    } else if keys.just_pressed(KeyCode::KeyO) {
        dialogs.open(start_directory(&file));
    } else if keys.just_pressed(KeyCode::KeyI) {
        dialogs.import_svg(start_directory(&file));
    } else if keys.just_pressed(KeyCode::KeyS) {
        if shift_held(&keys) {
            dialogs.save_as(start_directory(&file), suggested_file_name(&file));
        } else if let Some(result) = save_to_current(&editor, &mut file) {
            report_save(result, &file);
        } else {
            // The document has never been saved, so ask where to put it.
            dialogs.save_as(start_directory(&file), suggested_file_name(&file));
        }
    }
}

/// Shows the current file in the window title so actions have visible
/// feedback.
fn sync_window_title(
    file: Res<DocumentFile>,
    mut windows: Query<&mut Window, With<PrimaryWindow>>,
    mut last: Local<Option<String>>,
) {
    let name = file.display_name();
    if last.as_ref() == Some(&name) {
        return;
    }
    *last = Some(name.clone());

    for mut window in &mut windows {
        window.title = format!("Viveco Editor - {name}");
    }
}

fn ctrl_held(keys: &ButtonInput<KeyCode>) -> bool {
    keys.pressed(KeyCode::ControlLeft) || keys.pressed(KeyCode::ControlRight)
}

fn shift_held(keys: &ButtonInput<KeyCode>) -> bool {
    keys.pressed(KeyCode::ShiftLeft) || keys.pressed(KeyCode::ShiftRight)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bounds::EMPTY_BOUNDS;
    use viveco::Shape;

    #[test]
    fn empty_documents_fall_back_to_the_default_bounds() {
        assert_eq!(bounds_for(&Asset::new()), EMPTY_BOUNDS);
    }

    #[test]
    fn installed_documents_reset_view_and_history() {
        let mut editor = EditorState::new(Asset::new());
        let mut bounds = DocumentBounds(EMPTY_BOUNDS);
        let mut viewport = Viewport {
            zoom: 4.0,
            pan: Vec2::new(10.0, 20.0),
        };
        let mut history = History::new(Asset::new());

        let mut document = Asset::new();
        document.shapes.push(Shape {
            name: String::new(),
            z: 0.0,
            ..default()
        });

        install_document(
            &mut editor,
            &mut bounds,
            &mut viewport,
            &mut history,
            document.clone(),
        );

        assert_eq!(editor.document, document);
        assert_eq!(viewport, Viewport::default());
        assert!(!history.undo(&mut editor));
    }

    #[test]
    fn importing_an_svg_appends_shapes_and_is_undoable() {
        let path = std::env::temp_dir().join(format!(
            "viveco_editor_import_test_{}.svg",
            std::process::id()
        ));
        std::fs::write(
            &path,
            r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 10 10">
                  <rect x="1" y="2" width="3" height="4" fill="#ff0000"/>
                </svg>"##,
        )
        .unwrap();

        let mut document = Asset::new();
        document.shapes.push(Shape {
            z: 5.0,
            ..default()
        });
        let mut editor = EditorState::new(document.clone());
        let mut bounds = DocumentBounds(EMPTY_BOUNDS);
        let mut history = History::new(document);

        assert!(import_svg_document(
            &mut editor,
            &mut bounds,
            &mut history,
            &path
        ));

        assert_eq!(editor.document.shapes.len(), 2);
        // Imported shapes sit above the existing top-level shape.
        assert_eq!(editor.document.shapes[1].z, 6.0);
        assert_eq!(
            editor.selected_node,
            Some(TreeAddress::Shape { shapes: vec![1] })
        );

        assert!(history.undo(&mut editor));
        assert_eq!(editor.document.shapes.len(), 1);

        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn saving_and_opening_round_trips() {
        let path =
            std::env::temp_dir().join(format!("viveco_editor_test_{}.json", std::process::id()));
        let mut file = DocumentFile::untitled();

        let mut document = Asset::new();
        document.shapes.push(Shape {
            name: "Saved".into(),
            z: 0.0,
            ..default()
        });
        let editor = EditorState::new(document.clone());

        save_document_to(&editor, &mut file, &path).unwrap();
        assert_eq!(file.path.as_deref(), Some(path.as_path()));
        let loaded = Asset::load_from_file(&path).unwrap();
        assert_eq!(loaded, document);

        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn untitled_documents_have_no_save_target() {
        let mut file = DocumentFile::untitled();
        let editor = EditorState::new(Asset::new());

        assert_eq!(file.display_name(), UNTITLED_TITLE);
        assert!(save_to_current(&editor, &mut file).is_none());
        assert_eq!(suggested_file_name(&file), UNTITLED_FILE);
    }
}
