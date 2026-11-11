//! Copy and paste for the selected shape or path.
//!
//! The clipboard is internal to the editor: it holds a deep copy of the
//! selected [`Shape`] (or, when a path is selected, a new shape wrapping just
//! that path). Keeping the data owned by the editor avoids depending on the
//! platform clipboard and its serialization rules.

use bevy::prelude::*;

use crate::editor::{EditorMode, EditorState, TreeAddress, shape_at, translate_shape};
use viveco::Shape;

/// Distance, in document units, that each successive paste is offset by.
const PASTE_OFFSET: f32 = 10.0;

pub struct ClipboardPlugin;

impl Plugin for ClipboardPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Clipboard>()
            .add_systems(Update, handle_clipboard_shortcuts);
    }
}

/// The copied shape and how many times it has been pasted since.
#[derive(Resource, Default)]
pub struct Clipboard {
    shape: Option<Shape>,
    pastes: u32,
}

/// Copies the selection into `clipboard`.
///
/// A shape selection copies the whole subtree. A path selection copies just
/// that contour, wrapped in a new shape that borrows its owner's style. Returns
/// `false` when nothing usable is selected.
pub fn copy_selection(editor: &EditorState, clipboard: &mut Clipboard) -> bool {
    let Some(address) = editor.selected_node.as_ref() else {
        return false;
    };
    let Some(owner) = shape_at(&editor.document, address) else {
        return false;
    };
    let shape = match address {
        TreeAddress::Shape { .. } => owner.clone(),
        TreeAddress::Path { path, .. } => {
            let Some(path) = owner.paths.get(*path) else {
                return false;
            };
            Shape {
                name: path.name.clone(),
                z: 0.0,
                style: owner.style.clone(),
                paths: vec![path.clone()],
                children: Vec::new(),
            }
        }
    };
    clipboard.shape = Some(shape);
    clipboard.pastes = 0;
    true
}

/// Appends an offset copy of the clipboard shape on top of the document and
/// selects it.
///
/// Returns `false` when the clipboard is empty.
pub fn paste_clipboard(editor: &mut EditorState, clipboard: &mut Clipboard) -> bool {
    let Some(mut shape) = clipboard.shape.clone() else {
        return false;
    };
    clipboard.pastes = clipboard.pastes.saturating_add(1);

    let offset = PASTE_OFFSET * clipboard.pastes as f32;
    translate_shape(&mut shape, [offset, offset]);

    // Shapes default to Z zero. Appending to the vector puts the paste on top
    // of the sibling shapes that share that Z.
    shape.z = 0.0;

    let index = editor.document.shapes.len();
    editor.document.shapes.push(shape);
    editor.select_node(Some(TreeAddress::Shape {
        shapes: vec![index],
    }));
    editor.structure_revision += 1;
    true
}

/// Ctrl+C / Ctrl+V.
fn handle_clipboard_shortcuts(
    mode: Res<EditorMode>,
    keys: Res<ButtonInput<KeyCode>>,
    mut editor: ResMut<EditorState>,
    mut clipboard: ResMut<Clipboard>,
) {
    // Only modes that manage document nodes use the clipboard; rig mode edits
    // the skeleton instead.
    if !mode.uses_clipboard() {
        return;
    }
    // While a tree row is being renamed, these shortcuts belong to the text
    // field.
    if editor.is_renaming() || !ctrl_held(&keys) {
        return;
    }
    if keys.just_pressed(KeyCode::KeyC) {
        copy_selection(&editor, &mut clipboard);
    } else if keys.just_pressed(KeyCode::KeyV) {
        paste_clipboard(&mut editor, &mut clipboard);
    }
}

fn ctrl_held(keys: &ButtonInput<KeyCode>) -> bool {
    keys.pressed(KeyCode::ControlLeft) || keys.pressed(KeyCode::ControlRight)
}

#[cfg(test)]
mod tests {
    use super::*;
    use viveco::{Asset, Path, Rgba8, Style};

    fn filled(color: Rgba8) -> Style {
        Style {
            fill: Some(color),
            stroke: None,
        }
    }

    fn document_with_path() -> Asset {
        Asset {
            animations: Vec::new(),
            bones: Vec::new(),
            shapes: vec![Shape {
                style: filled(Rgba8::rgb(255, 0, 0)),
                paths: vec![Path::new([0.0, 0.0]).line_to([10.0, 0.0])],
                ..default()
            }],
        }
    }

    #[test]
    fn copying_then_pasting_adds_an_offset_copy_and_selects_it() {
        let mut editor = EditorState::new(document_with_path());
        editor.selected_node = Some(TreeAddress::Shape { shapes: vec![0] });
        let mut clipboard = Clipboard::default();

        assert!(copy_selection(&editor, &mut clipboard));
        assert!(paste_clipboard(&mut editor, &mut clipboard));

        let shapes = &editor.document.shapes;
        assert_eq!(shapes.len(), 2);
        assert_eq!(
            shapes[1].paths[0].start(),
            Some([PASTE_OFFSET, PASTE_OFFSET])
        );
        // The paste shares the original's default Z and comes later in the
        // vector, so it paints on top.
        assert_eq!(shapes[1].z, 0.0);
        assert_eq!(
            editor.selected_node,
            Some(TreeAddress::Shape { shapes: vec![1] })
        );

        // The next paste steps further away from the original.
        assert!(paste_clipboard(&mut editor, &mut clipboard));
        assert_eq!(
            editor.document.shapes[2].paths[0].start(),
            Some([2.0 * PASTE_OFFSET, 2.0 * PASTE_OFFSET])
        );
        assert_eq!(editor.document.shapes[2].z, 0.0);
    }

    #[test]
    fn copy_without_a_selection_does_nothing() {
        let editor = EditorState::new(document_with_path());
        let mut clipboard = Clipboard::default();

        assert!(!copy_selection(&editor, &mut clipboard));
        assert!(!paste_clipboard(
            &mut EditorState::new(Asset::new()),
            &mut clipboard
        ));
    }

    #[test]
    fn pasting_a_shape_with_children_keeps_the_subtree() {
        let source = Asset {
            animations: Vec::new(),
            bones: Vec::new(),
            shapes: vec![Shape {
                paths: vec![Path::new([0.0, 0.0]).line_to([10.0, 10.0])],
                children: vec![Shape {
                    paths: vec![Path::new([5.0, 5.0]).line_to([15.0, 5.0])],
                    ..default()
                }],
                ..default()
            }],
        };

        let mut editor = EditorState::new(source.clone());
        editor.selected_node = Some(TreeAddress::Shape { shapes: vec![0] });
        let mut clipboard = Clipboard::default();

        assert!(copy_selection(&editor, &mut clipboard));

        let mut target = EditorState::new(Asset::new());
        assert!(paste_clipboard(&mut target, &mut clipboard));
        assert_eq!(target.document.shapes.len(), 1);

        let pasted = &target.document.shapes[0];
        // Both the shape and its nested child move together.
        assert_eq!(pasted.paths[0].start(), Some([PASTE_OFFSET, PASTE_OFFSET]));
        assert_eq!(
            pasted.children[0].paths[0].start(),
            Some([5.0 + PASTE_OFFSET, 5.0 + PASTE_OFFSET])
        );
    }

    #[test]
    fn copying_a_path_pastes_it_as_its_own_shape() {
        let mut document = Asset {
            animations: Vec::new(),
            bones: Vec::new(),
            shapes: vec![Shape {
                paths: vec![
                    Path::new([0.0, 0.0]).line_to([10.0, 0.0]),
                    Path::new([20.0, 20.0]).line_to([30.0, 30.0]),
                ],
                ..default()
            }],
        };

        let mut editor = EditorState::new(std::mem::take(&mut document));
        editor.selected_node = Some(TreeAddress::Path {
            shapes: vec![0],
            path: 1,
        });
        let mut clipboard = Clipboard::default();

        assert!(copy_selection(&editor, &mut clipboard));
        assert!(paste_clipboard(&mut editor, &mut clipboard));

        let pasted = &editor.document.shapes[1];
        assert_eq!(pasted.paths.len(), 1);
        assert_eq!(
            pasted.paths[0].start(),
            Some([20.0 + PASTE_OFFSET, 20.0 + PASTE_OFFSET])
        );
    }
}
