use super::*;
use crate::util::file::external_editor::settings::EditorChoice;
use crate::util::file::external_editor::Editor;

/// The default-folder-editor dropdown lists exactly the installed editors, in
/// order, each mapped to an `ExternalEditor` choice (the setting never holds
/// Warp / System / $EDITOR, so those entries are absent).
#[test]
fn folder_editor_dropdown_lists_only_installed_editors() {
    let items = ExternalEditorView::folder_editor_dropdown_items(&[Editor::VSCode, Editor::Zed]);
    assert_eq!(
        items,
        vec![
            (
                format!("{}", Editor::VSCode),
                EditorChoice::ExternalEditor(Editor::VSCode)
            ),
            (
                format!("{}", Editor::Zed),
                EditorChoice::ExternalEditor(Editor::Zed)
            ),
        ]
    );
}

/// With nothing installed, the dropdown offers no editors.
#[test]
fn folder_editor_dropdown_is_empty_when_no_editor_installed() {
    let items = ExternalEditorView::folder_editor_dropdown_items(&[]);
    assert!(items.is_empty());
}

/// Selecting an editor from the dropdown writes that IDE to the setting via the
/// `SetDefaultFolderEditor` action.
#[test]
fn selecting_a_folder_editor_maps_to_set_action() {
    let items = ExternalEditorView::folder_editor_dropdown_items(&[Editor::Cursor]);
    let (_, choice) = &items[0];
    assert_eq!(
        ExternalEditorAction::SetDefaultFolderEditor(*choice),
        ExternalEditorAction::SetDefaultFolderEditor(EditorChoice::ExternalEditor(Editor::Cursor))
    );
}
