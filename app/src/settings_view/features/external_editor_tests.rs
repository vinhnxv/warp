use super::*;
use crate::util::file::external_editor::Editor;
use crate::util::file::external_editor::settings::EditorChoice;

/// The default-folder-editor dropdown lists the opt-out row and then the
/// installed editors, in order. There are no Warp / System / $EDITOR rows:
/// opening a *folder* in Warp or `$EDITOR` is not a thing, so the only two
/// outcomes are an IDE or the file manager.
#[test]
fn folder_editor_dropdown_lists_the_opt_out_row_then_installed_editors() {
    let items = ExternalEditorView::folder_editor_dropdown_items(&[Editor::VSCode, Editor::Zed]);
    assert_eq!(
        items,
        vec![
            (
                ExternalEditorView::no_folder_editor_label(),
                EditorChoice::SystemDefault
            ),
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

/// N15: with nothing installed, the opt-out row is still there — the state the
/// toolbar button already falls back to has to be visible and selectable, not
/// an empty dropdown that reads as a broken setting.
#[test]
fn folder_editor_dropdown_still_offers_the_opt_out_row_when_no_editor_installed() {
    let items = ExternalEditorView::folder_editor_dropdown_items(&[]);
    assert_eq!(
        items,
        vec![(
            ExternalEditorView::no_folder_editor_label(),
            EditorChoice::SystemDefault
        )]
    );
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
