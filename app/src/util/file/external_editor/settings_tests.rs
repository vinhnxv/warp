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

/// Builds an app with settings registered, runs `body` against it, and returns
/// what `body` produced. `resolve_default_folder_editor_with_installed` reads
/// (and, on a first run, writes) the settings model, so its arms need a real
/// context rather than the pure-setting construction the tests above use.
fn with_settings<T: 'static>(body: impl FnOnce(&mut warpui::AppContext) -> T + 'static) -> T {
    use std::cell::RefCell;
    use std::rc::Rc;

    let out = Rc::new(RefCell::new(None));
    let captured = Rc::clone(&out);
    warpui::App::test((), move |mut app| async move {
        crate::test_util::settings::initialize_settings_for_tests(&mut app);
        let value = app.update(body);
        *captured.borrow_mut() = Some(value);
    });
    let value = out.borrow_mut().take();
    value.expect("App::test runs its body to completion")
}

/// Writes `default_folder_editor` the way the Settings dropdown does.
fn choose_folder_editor(choice: EditorChoice, ctx: &mut warpui::AppContext) {
    use warpui::SingletonEntity as _;

    EditorSettings::handle(ctx).update(ctx, |settings, ctx| {
        settings
            .default_folder_editor
            .set_value(choice, ctx)
            .expect("the folder editor setting accepts every EditorChoice");
    });
}

/// Reads back what is persisted, without going through the resolver.
fn persisted_folder_editor(ctx: &warpui::AppContext) -> (bool, EditorChoice) {
    use warpui::SingletonEntity as _;

    let settings = EditorSettings::as_ref(ctx);
    (
        settings.default_folder_editor.is_value_explicitly_set(),
        *settings.default_folder_editor,
    )
}

/// NA9: an explicitly chosen editor resolves to itself while it is installed.
#[test]
fn a_chosen_editor_that_is_installed_resolves_to_itself() {
    let resolved = with_settings(|ctx| {
        choose_folder_editor(EditorChoice::ExternalEditor(Editor::VSCode), ctx);
        resolve_default_folder_editor_with_installed(ctx, &[Editor::VSCode, Editor::Sublime3])
    });

    assert_eq!(resolved, Some(Editor::VSCode));
}

/// NA9: the user chose an IDE and then uninstalled it. The folder is revealed
/// in the file manager — it is *not* opened in whichever other IDE happens to
/// sort first.
///
/// This arm used to fall through to the first-run seed, so uninstalling your
/// editor silently promoted an unrelated one: the button showed its logo, the
/// tooltip named it, and clicking opened a folder in an IDE the user never
/// chose.
#[test]
fn a_chosen_editor_that_is_gone_reveals_rather_than_substituting_another() {
    let resolved = with_settings(|ctx| {
        choose_folder_editor(EditorChoice::ExternalEditor(Editor::VSCode), ctx);
        // VS Code is no longer among the installed editors.
        resolve_default_folder_editor_with_installed(ctx, &[Editor::Sublime3])
    });

    assert_eq!(resolved, None);
}

/// NA9: the "None — reveal in the file manager" row. Every non-IDE choice
/// resolves to no editor, including with IDEs installed and available.
#[test]
fn opting_out_of_an_ide_resolves_to_no_editor() {
    for choice in [
        EditorChoice::SystemDefault,
        EditorChoice::Warp,
        EditorChoice::EnvEditor,
    ] {
        let resolved = with_settings(move |ctx| {
            choose_folder_editor(choice, ctx);
            resolve_default_folder_editor_with_installed(ctx, &[Editor::VSCode, Editor::Sublime3])
        });

        assert_eq!(resolved, None, "{choice:?} should resolve to no editor");
    }
}

/// NA9: with nothing chosen, the first resolve suggests an editor *and* writes
/// it down.
#[test]
fn the_first_resolve_persists_its_suggestion() {
    let (resolved, persisted) = with_settings(|ctx| {
        assert!(!persisted_folder_editor(ctx).0, "the setting starts unset");
        let resolved =
            resolve_default_folder_editor_with_installed(ctx, &[Editor::Sublime3, Editor::VSCode]);
        (resolved, persisted_folder_editor(ctx))
    });

    assert_eq!(resolved, Some(Editor::Sublime3));
    assert_eq!(
        persisted,
        (true, EditorChoice::ExternalEditor(Editor::Sublime3))
    );
}

/// NA9: installing an editor that sorts ahead of the seeded one does not change
/// an already-decided default.
///
/// The seed reads `installed_editors.first()`, and it used to be recomputed on
/// every resolve rather than written down — so installing an application
/// silently changed which IDE the button opened.
#[test]
fn installing_a_higher_priority_editor_does_not_change_the_default() {
    let (before, after) = with_settings(|ctx| {
        let before = resolve_default_folder_editor_with_installed(ctx, &[Editor::VSCode]);
        // Sublime sorts ahead of VS Code in `SUPPORTED_EDITORS`, so an
        // unpersisted seed would switch to it here.
        let after =
            resolve_default_folder_editor_with_installed(ctx, &[Editor::Sublime3, Editor::VSCode]);
        (before, after)
    });

    assert_eq!(before, Some(Editor::VSCode));
    assert_eq!(after, Some(Editor::VSCode));
}

/// With no editor installed there is no suggestion to make: the folder is
/// revealed, and nothing is written down, so the first editor the user installs
/// can still seed the default.
#[test]
fn nothing_is_persisted_when_no_editor_is_installed() {
    let (resolved, persisted) = with_settings(|ctx| {
        let resolved = resolve_default_folder_editor_with_installed(ctx, &[]);
        (resolved, persisted_folder_editor(ctx))
    });

    assert_eq!(resolved, None);
    assert_eq!(persisted, (false, EditorChoice::SystemDefault));
}
