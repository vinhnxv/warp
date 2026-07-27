use super::*;
use crate::util::file::external_editor::Editor;
use crate::util::file::external_editor::settings::EditorChoice;

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
