use super::*;

// --- Unit U4: open-folder target resolution (R4/R5) ---

#[test]
fn test_resolve_open_folder_target_returns_deepest_repo_root_for_cwd_in_known_repo() {
    // R4: when the active tab's cwd is inside a known repo, the target is the
    // deepest-ancestor repo root. The real lookup (`get_root_for_path`) already
    // returns the deepest ancestor; here the injected closure mimics that by
    // walking ancestors over a set of registered roots.
    let repo_roots = [
        PathBuf::from("/work/outer"),
        PathBuf::from("/work/outer/nested"),
    ];
    let deepest_root_for = |path: &Path| -> Option<PathBuf> {
        path.ancestors()
            .find(|ancestor| repo_roots.iter().any(|root| root == ancestor))
            .map(Path::to_path_buf)
    };

    // cwd nested under BOTH repos -> the deepest (nested) root wins.
    assert_eq!(
        resolve_open_folder_target_from(
            Some(PathBuf::from("/work/outer/nested/src")),
            deepest_root_for,
        ),
        Some(PathBuf::from("/work/outer/nested")),
    );

    // cwd under only the outer repo -> the outer root.
    assert_eq!(
        resolve_open_folder_target_from(Some(PathBuf::from("/work/outer/docs")), deepest_root_for,),
        Some(PathBuf::from("/work/outer")),
    );
}

#[test]
fn test_resolve_open_folder_target_returns_cwd_when_no_known_repo() {
    // R4: a cwd owned by no known repo -> open the cwd itself.
    let cwd = PathBuf::from("/tmp/loose-dir");
    assert_eq!(
        resolve_open_folder_target_from(Some(cwd.clone()), |_| None),
        Some(cwd),
    );
}

#[test]
fn test_resolve_open_folder_target_returns_none_when_no_local_cwd() {
    // R5: remote/SSH sessions AND deleted/non-existent cwds both surface as
    // `None` from `canonical_session_pwd_if_local`, so the target is `None` and
    // the repo lookup is never consulted.
    let mut lookup_called = false;
    let target = resolve_open_folder_target_from(None, |_| {
        lookup_called = true;
        Some(PathBuf::from("/should/not/be/used"))
    });
    assert_eq!(target, None);
    assert!(
        !lookup_called,
        "repo lookup must not run when there is no local cwd"
    );
}

// --- Unit U5: open-folder action decision + telemetry payload (R2/R3/R8) ---
//
// The full handlers (`Workspace::open_current_folder` and the `handle_action`
// arms) are ctx-only: they need a real `Workspace` + `AppContext` to resolve
// the target folder, launch the editor / reveal in Finder, and dispatch
// telemetry. What is unit-testable without that harness is the *decision* the
// handlers hand off to those effects — which editor vs. reveal, and the exact
// telemetry `target` / `from_dropdown` payload. Those are factored into the
// pure `default_open_folder_action` + `OpenFolderAction::telemetry_target`
// helpers, tested here. The resolve->early-return-on-`None` ordering (remote /
// missing cwd => no launch, no telemetry) lives entirely in the ctx-bound
// `open_current_folder` and is covered by the U4 resolver tests above plus
// manual smoke.

#[test]
fn test_default_open_action_launches_default_ide_when_set() {
    // R2: default-open with an IDE default -> launch that IDE, telemetry target
    // is the IDE's display name. `from_dropdown` is stamped `false` by the
    // primary-click handler (ctx-only).
    let action = default_open_folder_action(Some(Editor::VSCode));
    assert_eq!(action, OpenFolderAction::LaunchEditor(Editor::VSCode));
    assert_eq!(action.telemetry_target(), format!("{}", Editor::VSCode));
}

#[test]
fn test_default_open_action_reveals_when_no_default_ide() {
    // Edge: default setting unset + no IDE installed -> the default action
    // reveals in Finder (target "finder"), never launches an editor.
    let action = default_open_folder_action(None);
    assert_eq!(action, OpenFolderAction::Reveal);
    assert_eq!(action.telemetry_target(), "finder");
}

#[test]
fn test_launch_editor_telemetry_target_is_editor_display_name() {
    // R3: opening one-off in a specific IDE records that IDE's display name.
    for editor in [Editor::VSCode, Editor::Cursor, Editor::Zed] {
        let target = OpenFolderAction::LaunchEditor(editor).telemetry_target();
        assert_eq!(target, format!("{editor}"));
    }
}

#[test]
fn test_reveal_telemetry_target_is_finder() {
    // R3: reveal records the fixed "finder" target regardless of platform.
    assert_eq!(OpenFolderAction::Reveal.telemetry_target(), "finder");
}

#[test]
fn test_open_folder_telemetry_target_never_contains_a_path() {
    // R8: the telemetry payload must stay UGC-free -- `target` is only ever an
    // editor name or "finder", never a filesystem path.
    let targets = [
        OpenFolderAction::LaunchEditor(Editor::VSCode).telemetry_target(),
        OpenFolderAction::Reveal.telemetry_target(),
    ];
    for target in targets {
        assert!(
            !target.contains('/') && !target.contains('\\'),
            "telemetry target must not embed a path, got {target:?}",
        );
    }
}

// --- Unit U6: open-folder toolbar button pure helpers (R1/R3/R5, KTD5) ---
//
// The split-button construction, its flag-gated injection into the tab bar, the
// dropdown positioning, and the disabled/tooltip refresh are all ctx-bound
// (they need a real `Workspace` + `AppContext` + `ViewContext`), so they are
// covered by the build plus manual smoke. What is unit-testable without that
// harness is the pure decision layer the ctx-bound render path hands off to:
// the OS reveal label, the tooltip label given (default IDE, remote), and the
// menu-item list built from the installed-editor set.

#[test]
fn test_os_reveal_label_matches_platform() {
    let expected = if cfg!(target_os = "macos") {
        "Reveal in Finder"
    } else if cfg!(target_os = "windows") {
        "Reveal in Explorer"
    } else {
        "Reveal in file manager"
    };
    assert_eq!(os_reveal_label(), expected);
}

#[test]
fn test_tooltip_names_default_ide_when_set() {
    // Default IDE set + local tab -> tooltip names the IDE the primary opens.
    assert_eq!(
        open_folder_button_tooltip(Some(Editor::VSCode), false),
        format!("Open folder in {}", Editor::VSCode),
    );
}

#[test]
fn test_tooltip_is_os_reveal_label_when_no_default_ide() {
    // No default IDE -> primary reveals in Finder, tooltip reads the OS label.
    assert_eq!(
        open_folder_button_tooltip(None, false),
        os_reveal_label().to_string(),
    );
}

#[test]
fn test_tooltip_is_remote_message_when_disabled() {
    // Remote/disabled tab (resolver `None`) -> tooltip explains unavailability,
    // regardless of whether a default editor happens to be set.
    assert_eq!(
        open_folder_button_tooltip(None, true),
        "Not available for remote sessions",
    );
    assert_eq!(
        open_folder_button_tooltip(Some(Editor::VSCode), true),
        "Not available for remote sessions",
    );
}

#[test]
fn test_menu_items_with_zero_installed_ides_is_reveal_only() {
    // KTD5: with no installed IDEs the menu shows ONLY the Reveal item -- no IDE
    // rows, no separator, no hint row.
    let items = open_folder_menu_items(&[], None);
    assert_eq!(items.len(), 1, "expected exactly the Reveal item");
    match &items[0] {
        MenuItem::Item(fields) => {
            assert_eq!(fields.label(), os_reveal_label());
            assert!(matches!(
                fields.on_select_action(),
                Some(WorkspaceAction::RevealCurrentFolder)
            ));
        }
        other => panic!("expected the Reveal item, got {other:?}"),
    }
}

#[test]
fn test_menu_items_lists_installed_ides_then_divider_then_reveal() {
    // R3: one row per installed IDE (opening that specific editor), then a
    // visual separator, then the Reveal entry.
    let installed = [Editor::VSCode, Editor::Cursor];
    let items = open_folder_menu_items(&installed, Some(Editor::VSCode));

    // N IDE rows + 1 separator + 1 Reveal.
    assert_eq!(items.len(), installed.len() + 2);

    for (item, editor) in items.iter().zip(installed.iter()) {
        match item {
            MenuItem::Item(fields) => {
                assert_eq!(fields.label(), format!("{editor}").as_str());
                match fields.on_select_action() {
                    Some(WorkspaceAction::OpenCurrentFolderIn(action_editor)) => {
                        assert_eq!(action_editor, editor);
                    }
                    other => panic!("expected OpenCurrentFolderIn, got {other:?}"),
                }
                // Every IDE row carries that IDE's full-color logo.
                assert_eq!(fields.image_icon(), editor.logo_asset());
                // Only the current default IDE row is check-marked.
                let expected_check = (*editor == Editor::VSCode).then_some(icons::Icon::Check);
                assert_eq!(fields.right_side_icon(), expected_check);
            }
            other => panic!("expected an IDE item, got {other:?}"),
        }
    }

    // The separator divides the IDE rows from the Reveal entry.
    assert!(
        items[installed.len()].is_separator(),
        "installed IDEs must be separated from the Reveal entry by a divider",
    );

    // The trailing item reveals the folder in Finder.
    match items.last().expect("menu is non-empty") {
        MenuItem::Item(fields) => {
            assert_eq!(fields.label(), os_reveal_label());
            assert!(matches!(
                fields.on_select_action(),
                Some(WorkspaceAction::RevealCurrentFolder)
            ));
        }
        other => panic!("expected the Reveal item, got {other:?}"),
    }
}
