//! Native open and save dialogs, backed by `rfd`.
//!
//! The dialogs block the calling thread until the user picks a path or cancels,
//! so each one runs on its own worker thread and the result travels back over a
//! channel. A system drains that channel every frame and applies the result,
//! which keeps the render loop responsive while a dialog is open.

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::{Mutex, MutexGuard};

use bevy::prelude::*;

use crate::editor::{DocumentBounds, EditorState, Viewport};
use crate::file::{DocumentFile, import_svg_document, open_document, save_document_to};
use crate::history::History;

/// Shown in the dialog's binary file-type filter.
const BINARY_FILTER_NAME: &str = "Viveco";

/// Extensions accepted by the binary file-type filter. Listed first so the
/// save dialog defaults to the binary format.
const BINARY_FILTER_EXTENSIONS: &[&str] = &["viveco"];

/// Shown in the dialog's JSON file-type filter.
const JSON_FILTER_NAME: &str = "Viveco JSON";

/// Extensions accepted by the JSON file-type filter.
const JSON_FILTER_EXTENSIONS: &[&str] = &["json"];

/// Shown in the dialog's SVG file-type filter.
const SVG_FILTER_NAME: &str = "SVG";

/// Extensions accepted by the SVG file-type filter.
const SVG_FILTER_EXTENSIONS: &[&str] = &["svg", "svgz"];

/// Title of the open dialog.
const OPEN_TITLE: &str = "Open Viveco";

/// Title of the save dialog.
const SAVE_TITLE: &str = "Save Viveco";

/// Title of the SVG import dialog.
const IMPORT_TITLE: &str = "Import SVG";

pub struct DialogPlugin;

impl Plugin for DialogPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<FileDialogs>()
            .add_systems(Update, poll_file_dialogs);
    }
}

/// Which dialog was shown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DialogRequest {
    Open,
    Save,
    ImportSvg,
}

/// A dialog that finished on its worker thread.
#[derive(Debug)]
struct DialogResult {
    request: DialogRequest,
    path: Option<PathBuf>,
}

/// Tracks the dialog currently being shown, if any.
///
/// Only one dialog may be open at a time so that two requests cannot race over
/// the document.
#[derive(Resource, Default)]
pub struct FileDialogs {
    // A `Receiver` is `Send` but not `Sync`, so it is locked to make the whole
    // resource shareable; the lock is only held for the few instructions that
    // touch it.
    pending: Mutex<Option<Receiver<DialogResult>>>,
}

impl FileDialogs {
    /// Whether a dialog is already open.
    pub fn is_open(&self) -> bool {
        self.pending().is_some()
    }

    fn pending(&self) -> MutexGuard<'_, Option<Receiver<DialogResult>>> {
        self.pending
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Shows a native open dialog. Returns `false` when one is already open.
    pub fn open(&mut self, directory: Option<PathBuf>) -> bool {
        self.start(DialogRequest::Open, directory, None)
    }

    /// Shows a native save dialog. Returns `false` when one is already open.
    pub fn save_as(&mut self, directory: Option<PathBuf>, file_name: String) -> bool {
        self.start(DialogRequest::Save, directory, Some(file_name))
    }

    /// Shows a native dialog for choosing an SVG to import. Returns `false`
    /// when a dialog is already open.
    pub fn import_svg(&mut self, directory: Option<PathBuf>) -> bool {
        self.start(DialogRequest::ImportSvg, directory, None)
    }

    /// Abandons the dialog in progress, if any.
    ///
    /// The worker thread cannot be stopped, but its result is discarded, so a
    /// dialog that was open before the document was replaced cannot overwrite
    /// the new document.
    pub fn cancel(&mut self) {
        *self.pending() = None;
    }

    fn start(
        &mut self,
        request: DialogRequest,
        directory: Option<PathBuf>,
        file_name: Option<String>,
    ) -> bool {
        if self.pending().is_some() {
            return false;
        }

        let (sender, receiver) = mpsc::channel();
        std::thread::spawn(move || {
            let mut dialog = rfd::FileDialog::new().set_title(match request {
                DialogRequest::Open => OPEN_TITLE,
                DialogRequest::Save => SAVE_TITLE,
                DialogRequest::ImportSvg => IMPORT_TITLE,
            });
            dialog = match request {
                DialogRequest::ImportSvg => {
                    dialog.add_filter(SVG_FILTER_NAME, SVG_FILTER_EXTENSIONS)
                }
                DialogRequest::Open | DialogRequest::Save => dialog
                    .add_filter(BINARY_FILTER_NAME, BINARY_FILTER_EXTENSIONS)
                    .add_filter(JSON_FILTER_NAME, JSON_FILTER_EXTENSIONS),
            };
            if let Some(directory) = directory {
                dialog = dialog.set_directory(directory);
            }
            if let Some(file_name) = file_name {
                dialog = dialog.set_file_name(file_name);
            }

            let path = match request {
                DialogRequest::Open | DialogRequest::ImportSvg => dialog.pick_file(),
                DialogRequest::Save => dialog.save_file(),
            };
            // The receiver is dropped when the dialog is cancelled; sending
            // then fails, which is fine.
            let _ = sender.send(DialogResult { request, path });
        });

        *self.pending() = Some(receiver);
        true
    }
}

/// Applies the result of a dialog once its worker thread has finished.
fn poll_file_dialogs(
    dialogs: ResMut<FileDialogs>,
    mut editor: ResMut<EditorState>,
    mut bounds: ResMut<DocumentBounds>,
    mut viewport: ResMut<Viewport>,
    mut history: ResMut<History>,
    mut file: ResMut<DocumentFile>,
) {
    // `Some(Some(result))` is a finished dialog, `Some(None)` means the worker
    // thread went away without a result, and `None` means it is still open.
    let finished = {
        let pending = dialogs.pending();
        match pending.as_ref() {
            None => return,
            Some(receiver) => match receiver.try_recv() {
                Ok(result) => Some(Some(result)),
                Err(TryRecvError::Empty) => return,
                // The worker thread panicked or otherwise dropped the sender.
                Err(TryRecvError::Disconnected) => Some(None),
            },
        }
    };
    *dialogs.pending() = None;

    let Some(Some(result)) = finished else {
        return;
    };
    let Some(path) = result.path else {
        // The user cancelled.
        return;
    };

    match result.request {
        DialogRequest::Open => {
            open_document(
                &mut editor,
                &mut bounds,
                &mut viewport,
                &mut history,
                &mut file,
                &path,
            );
        }
        DialogRequest::Save => {
            if let Err(err) = save_document_to(&editor, &mut file, &path) {
                eprintln!("failed to save {path:?}: {err}");
            } else {
                println!("saved {path:?}");
            }
        }
        DialogRequest::ImportSvg => {
            import_svg_document(&mut editor, &mut bounds, &mut history, &path);
        }
    }
}
