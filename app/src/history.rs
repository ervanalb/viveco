//! Undo and redo.
//!
//! History is snapshot based: a changed [`Document`] is copied onto a stack so
//! every edit can be reverted without per-edit undo code. Snapshots are only
//! taken between gestures, so dragging a point or a slider produces one undo
//! step rather than one per frame.

use bevy::prelude::*;

use crate::editor::EditorState;
use crate::model::Document;

pub struct HistoryPlugin;

impl Plugin for HistoryPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, (handle_history_shortcuts, record_history).chain());
    }
}

/// Undo/redo stacks plus the last document state that was recorded.
///
/// The initial document must be passed to [`History::new`] so the first edit
/// has something to return to.
#[derive(Resource)]
pub struct History {
    undo: Vec<Document>,
    redo: Vec<Document>,
    last: Document,
}

impl History {
    pub fn new(document: Document) -> Self {
        Self {
            undo: Vec::new(),
            redo: Vec::new(),
            last: document,
        }
    }

    /// Forgets all history and treats `document` as the new baseline.
    ///
    /// Used when the document is replaced wholesale (new or load).
    pub fn reset(&mut self, document: Document) {
        self.undo.clear();
        self.redo.clear();
        self.last = document;
    }

    /// Records the document if it differs from the last recorded state.
    ///
    /// `continuous` suppresses recording mid-gesture; the next call with
    /// `continuous == false` then records the state from before the gesture,
    /// coalescing the whole drag into one undo step.
    pub fn observe(&mut self, document: &Document, continuous: bool) {
        if continuous || *document == self.last {
            return;
        }
        self.undo
            .push(std::mem::replace(&mut self.last, document.clone()));
        self.redo.clear();
    }

    /// Restores the previous document. Returns `false` when there is nothing to
    /// undo.
    pub fn undo(&mut self, editor: &mut EditorState) -> bool {
        let Some(previous) = self.undo.pop() else {
            return false;
        };
        let current = std::mem::replace(&mut self.last, previous.clone());
        self.redo.push(current);
        editor.replace_document(previous);
        true
    }

    /// Reapplies an undone document. Returns `false` when there is nothing to
    /// redo.
    pub fn redo(&mut self, editor: &mut EditorState) -> bool {
        let Some(next) = self.redo.pop() else {
            return false;
        };
        let current = std::mem::replace(&mut self.last, next.clone());
        self.undo.push(current);
        editor.replace_document(next);
        true
    }
}

/// Ctrl+Z / Ctrl+Shift+Z / Ctrl+Y.
fn handle_history_shortcuts(
    keys: Res<ButtonInput<KeyCode>>,
    mut editor: ResMut<EditorState>,
    mut history: ResMut<History>,
) {
    // While a tree row is being renamed, these shortcuts belong to the text
    // field.
    if editor.renaming.is_some() || !ctrl_held(&keys) {
        return;
    }
    if keys.just_pressed(KeyCode::KeyZ) {
        if shift_held(&keys) {
            history.redo(&mut editor);
        } else {
            history.undo(&mut editor);
        }
    } else if keys.just_pressed(KeyCode::KeyY) {
        history.redo(&mut editor);
    }
}

/// Snapshots the document whenever an edit settles.
///
/// While the left button is held a gesture (handle drag, object drag, slider
/// drag) may still be changing the document, so recording waits for release.
fn record_history(
    mouse: Res<ButtonInput<MouseButton>>,
    editor: Res<EditorState>,
    mut history: ResMut<History>,
) {
    history.observe(&editor.document, mouse.pressed(MouseButton::Left));
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
    use crate::model::{FORMAT_VERSION, Path, Shape, Style};

    fn document_with_path(name: &str) -> Document {
        Document {
            version: FORMAT_VERSION,
            shapes: vec![Shape {
                name: name.into(),
                z: 0.0,
                style: Style::default(),
                paths: vec![Path::new([0.0, 0.0]).line_to([10.0, 10.0])],
                children: Vec::new(),
            }],
        }
    }

    #[test]
    fn undoing_restores_the_previous_document() {
        let original = document_with_path("original");
        let mut editor = EditorState::new(original.clone());
        let mut history = History::new(original.clone());

        let edited = document_with_path("edited");
        editor.document = edited.clone();
        history.observe(&editor.document, false);

        assert!(history.undo(&mut editor));
        assert_eq!(editor.document, original);
        assert!(history.redo(&mut editor));
        assert_eq!(editor.document, edited);
    }

    #[test]
    fn changes_during_a_gesture_are_one_undo_step() {
        let original = document_with_path("original");
        let mut editor = EditorState::new(original.clone());
        let mut history = History::new(original.clone());

        // Several intermediate states while the mouse is held down.
        for name in ["drag-1", "drag-2", "drag-3"] {
            editor.document = document_with_path(name);
            history.observe(&editor.document, true);
        }
        editor.document = document_with_path("final");
        history.observe(&editor.document, false);

        assert!(history.undo(&mut editor));
        assert_eq!(editor.document, original);
        assert!(!history.undo(&mut editor));
    }

    #[test]
    fn a_new_edit_clears_the_redo_stack() {
        let original = document_with_path("original");
        let mut editor = EditorState::new(original.clone());
        let mut history = History::new(original);

        editor.document = document_with_path("first");
        history.observe(&editor.document, false);
        history.undo(&mut editor);

        editor.document = document_with_path("second");
        history.observe(&editor.document, false);

        assert!(!history.redo(&mut editor));
        assert_eq!(editor.document, document_with_path("second"));
    }

    #[test]
    fn resetting_history_keeps_the_current_document_as_the_baseline() {
        let original = document_with_path("original");
        let mut editor = EditorState::new(original.clone());
        let mut history = History::new(original);

        editor.document = document_with_path("edited");
        history.observe(&editor.document, false);

        let loaded = document_with_path("loaded");
        history.reset(loaded.clone());
        editor.replace_document(loaded.clone());

        assert!(!history.undo(&mut editor));
        assert_eq!(editor.document, loaded);
    }
}
