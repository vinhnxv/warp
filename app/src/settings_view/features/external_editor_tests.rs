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

/// The open-file dropdowns build from a supplied installed-editor list, which
/// is what lets the page detect once and share the answer.
///
/// Every dropdown used to call `is_installed` per supported editor while
/// building its own items, so opening Settings ran the same sweep four times —
/// on macOS an uncached LaunchServices lookup each, ~104 in total. Nothing here
/// can probe: pass the list and the entries follow from it.
#[test]
fn the_editor_dropdown_builds_from_the_supplied_list() {
    let items = ExternalEditorView::editor_dropdown_items(&[Editor::VSCode, Editor::Zed]);

    // The fixed entries lead, then the installed editors in the given order.
    assert_eq!(
        items.first(),
        Some(&(
            ExternalEditorView::DEFAULT_OPTION_TEXT.to_string(),
            EditorChoice::SystemDefault
        ))
    );
    assert!(items.contains(&("Warp".to_string(), EditorChoice::Warp)));
    assert_eq!(
        items[items.len() - 2..],
        [
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

/// An empty list yields only the fixed entries — no editor is offered that the
/// shared scan did not find.
#[test]
fn the_editor_dropdown_offers_no_editor_when_none_is_installed() {
    let items = ExternalEditorView::editor_dropdown_items(&[]);

    assert!(
        !items
            .iter()
            .any(|(_, choice)| matches!(choice, EditorChoice::ExternalEditor(_))),
        "an editor appeared that the shared scan never reported: {items:?}"
    );
}
