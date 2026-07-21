use settings::Setting as _;

use super::*;
use crate::util::file::external_editor::Editor;

/// Unset + the user's file editor is already an installed IDE: the folder
/// default seeds to that same IDE.
#[test]
fn seed_uses_file_editor_when_it_is_an_installed_external_editor() {
    let seed = seed_default_folder_editor(
        EditorChoice::ExternalEditor(Editor::VSCode),
        &[Editor::Zed, Editor::VSCode],
    );
    assert_eq!(seed, Some(Editor::VSCode));
}

/// Unset + the file editor is an IDE that is NOT installed: the seed must not
/// hand back an unlaunchable editor (the button would show its logo/tooltip and
/// telemetry would record it while the launch silently falls back). Falls back
/// to the first installed IDE instead.
#[test]
fn seed_skips_uninstalled_file_editor() {
    let seed =
        seed_default_folder_editor(EditorChoice::ExternalEditor(Editor::VSCode), &[Editor::Zed]);
    assert_eq!(seed, Some(Editor::Zed));
}

/// Unset + the file editor is an uninstalled IDE and nothing else is installed:
/// stays unset (None) so the primary reveals in the file manager.
#[test]
fn seed_is_none_when_file_editor_uninstalled_and_nothing_installed() {
    let seed = seed_default_folder_editor(EditorChoice::ExternalEditor(Editor::VSCode), &[]);
    assert_eq!(seed, None);
}

/// Unset + the file editor is a non-IDE choice (Warp / System / $EDITOR): the
/// folder default seeds to the first installed IDE.
#[test]
fn seed_falls_back_to_first_installed_editor_for_non_ide_file_editor() {
    for choice in [
        EditorChoice::Warp,
        EditorChoice::SystemDefault,
        EditorChoice::EnvEditor,
    ] {
        let seed = seed_default_folder_editor(choice, &[Editor::Zed, Editor::VSCode]);
        assert_eq!(
            seed,
            Some(Editor::Zed),
            "{choice:?} should seed to the first installed IDE"
        );
    }
}

/// Edge: unset + no IDE installed => stays unset (None). The primary toolbar
/// button falls back to revealing the folder in the file manager (U5).
#[test]
fn seed_is_none_when_no_editor_installed() {
    let seed = seed_default_folder_editor(EditorChoice::Warp, &[]);
    assert_eq!(seed, None);
}

/// Happy path: the setting round-trips an IDE (write it, read it back).
#[test]
fn default_folder_editor_setting_round_trips_an_ide() {
    let setting = DefaultFolderEditor::new(Some(EditorChoice::ExternalEditor(Editor::VSCode)));
    assert_eq!(
        *setting.value(),
        EditorChoice::ExternalEditor(Editor::VSCode)
    );
    assert!(setting.is_value_explicitly_set());
}

/// The setting is local-only, mirroring `open_file_editor`.
#[test]
fn default_folder_editor_is_local_only_like_open_file_editor() {
    assert_eq!(DefaultFolderEditor::sync_to_cloud(), SyncToCloud::Never);
    assert_eq!(OpenFileEditor::sync_to_cloud(), SyncToCloud::Never);
    assert_eq!(
        DefaultFolderEditor::default_value(),
        EditorChoice::SystemDefault
    );
    assert_eq!(
        DefaultFolderEditor::toml_path(),
        Some("code.editor.default_folder_editor")
    );
}

/// A freshly-constructed (unset) setting is not explicitly set, which is what the
/// seed logic keys off of.
#[test]
fn unset_default_folder_editor_is_not_explicitly_set() {
    let setting = DefaultFolderEditor::new(None);
    assert!(!setting.is_value_explicitly_set());
}
