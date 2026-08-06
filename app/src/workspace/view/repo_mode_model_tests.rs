use chrono::{Duration, Utc};
use repo_mode::{RemoteProbeOutcome, RemoteProbeState, format_remote_key};
use warpui::{App, EntityId, TypedActionView as _};

use super::*;
use crate::persistence::model::Project;
use crate::workspace::view::non_contiguous_groups;
use crate::workspace::view::tests::{initialize_app, mock_workspace};

fn register_projects_model(app: &mut App, projects: Vec<Project>) {
    app.add_singleton_model(|ctx| ProjectManagementModel::new(projects, None, ctx));
}

/// A probe session that has already landed, for rendering tests that care about
/// the state a row shows and not about which probe wrote it.
fn settled_session(state: RemoteProbeState) -> RemoteProbeSession {
    RemoteProbeSession {
        generation: 1,
        in_flight: false,
        state,
    }
}

fn registered_paths(ctx: &AppContext) -> Vec<String> {
    let mut paths: Vec<String> = ProjectManagementModel::handle(ctx).read(ctx, |projects, _| {
        projects.all_projects().map(|p| p.path.clone()).collect()
    });
    paths.sort();
    paths
}

/// Covers AE5 / R10: no selection means the stock tab set (no filtering).
#[test]
fn test_no_selection_shows_stock_tab_set() {
    let _repo_mode_guard = FeatureFlag::RepoMode.override_enabled(true);
    let _grouped_tabs_guard = FeatureFlag::GroupedTabs.override_enabled(true);
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        register_projects_model(&mut app, Vec::new());
        let workspace = mock_workspace(&mut app);
        workspace.update(&mut app, |workspace, ctx| {
            assert_eq!(workspace.selected_repo_root, None);
            assert_eq!(workspace.repo_mode_visible_tab_indices(ctx), None);
        });
    });
}

/// Covers AE2 / R6: selecting an entry filters the visible set to its tabs
/// (group-root fallback while cwds are unknown); the other repo's tabs stay
/// alive and untouched.
#[test]
fn test_selection_filters_visible_tabs_to_group() {
    let _repo_mode_guard = FeatureFlag::RepoMode.override_enabled(true);
    let _grouped_tabs_guard = FeatureFlag::GroupedTabs.override_enabled(true);
    let now = Utc::now().naive_utc();
    let projects = vec![
        Project {
            path: "/repo/a".to_string(),
            added_ts: now,
            last_opened_ts: Some(now),
            manual_position: None,
        },
        Project {
            path: "/repo/b".to_string(),
            added_ts: now,
            last_opened_ts: Some(now),
            manual_position: None,
        },
    ];
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        register_projects_model(&mut app, projects);
        let workspace = mock_workspace(&mut app);
        workspace.update(&mut app, |workspace, ctx| {
            // Tabs: [default, a1, a2, b1]
            workspace.add_terminal_tab(false, ctx);
            workspace.add_terminal_tab(false, ctx);
            workspace.add_terminal_tab(false, ctx);

            let mut group_a = TabGroup::new();
            group_a.repo_root = Some("/repo/a".to_string());
            let group_a_id = group_a.id;
            workspace.tab_groups.insert(group_a_id, group_a);
            let mut group_b = TabGroup::new();
            group_b.repo_root = Some("/repo/b".to_string());
            let group_b_id = group_b.id;
            workspace.tab_groups.insert(group_b_id, group_b);

            workspace.tabs[1].group_id = Some(group_a_id);
            workspace.tabs[2].group_id = Some(group_a_id);
            workspace.tabs[3].group_id = Some(group_b_id);

            workspace.selected_repo_root = Some("/repo/a".to_string());
            assert_eq!(
                workspace.repo_mode_visible_tab_indices(ctx),
                Some(vec![1, 2])
            );

            workspace.selected_repo_root = Some("/repo/b".to_string());
            assert_eq!(workspace.repo_mode_visible_tab_indices(ctx), Some(vec![3]));
            // The other repo's tabs are still present (background, not closed).
            assert_eq!(workspace.tab_count(), 4);

            // A *registered* entry with no matching tabs yields an empty set,
            // never unrelated tabs (R10). Drop b1 from group_b so /repo/b owns
            // no tabs, then select it.
            workspace.tabs[3].group_id = None;
            workspace.selected_repo_root = Some("/repo/b".to_string());
            assert_eq!(
                workspace.repo_mode_visible_tab_indices(ctx),
                Some(Vec::new())
            );

            // A selection no longer in the registry (e.g. the entry was removed
            // in another window) recovers to "All" (None sentinel = no filter)
            // rather than stranding this window with an empty strip.
            workspace.selected_repo_root = Some("/repo/missing".to_string());
            assert_eq!(workspace.repo_mode_visible_tab_indices(ctx), None);
        });
    });
}

/// Covers R6/KTD8: selecting a registered entry creates its group with a tab,
/// and later new tabs join the bound group.
#[test]
fn test_select_entry_creates_group_and_new_tabs_join_it() {
    let _repo_mode_guard = FeatureFlag::RepoMode.override_enabled(true);
    let _grouped_tabs_guard = FeatureFlag::GroupedTabs.override_enabled(true);
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dunce::canonicalize(dir.path()).expect("canonicalize");
    let root_str = root.to_string_lossy().into_owned();
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        register_projects_model(&mut app, Vec::new());
        let workspace = mock_workspace(&mut app);
        workspace.update(&mut app, |workspace, ctx| {
            let initial_tabs = workspace.tab_count();
            workspace.select_repo_mode_entry(&root, ctx);

            assert_eq!(
                workspace.selected_repo_root.as_deref(),
                Some(root_str.as_str())
            );
            let group_id = workspace
                .selected_repo_mode_group_id()
                .expect("bound group should exist after select");
            assert_eq!(workspace.tab_count(), initial_tabs + 1);
            let member_count = |workspace: &Workspace| {
                workspace
                    .tabs
                    .iter()
                    .filter(|t| t.group_id == Some(group_id))
                    .count()
            };
            assert_eq!(member_count(workspace), 1);

            // A new tab opened while the entry is selected joins its group.
            workspace.add_terminal_tab(false, ctx);
            assert_eq!(member_count(workspace), 2);
        });
    });
}

/// NA2, no-redundant-terminal half: reselecting an entry that already has a
/// bound tab activates it and spawns nothing.
///
/// This is the user-visible cost of cwd-based attribution: a tab whose shell
/// had `cd`'d into another registered repo was filed under *that* repo, so
/// selecting its own repo found zero members and took the create-a-tab branch,
/// leaving the user with two terminals for one repository. Attribution is now
/// by binding, and the guard in `select_repo_mode_entry` catches a regression
/// even if the partition ever diverges again.
#[test]
fn test_reselecting_an_entry_with_a_bound_tab_opens_no_new_terminal() {
    let _repo_mode_guard = FeatureFlag::RepoMode.override_enabled(true);
    let _grouped_tabs_guard = FeatureFlag::GroupedTabs.override_enabled(true);
    let repo = tempfile::tempdir().expect("tempdir");
    let other = tempfile::tempdir().expect("tempdir");
    let repo_root = dunce::canonicalize(repo.path()).expect("canonicalize");
    let other_root = dunce::canonicalize(other.path()).expect("canonicalize");
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        register_projects_model(&mut app, Vec::new());
        let workspace = mock_workspace(&mut app);
        workspace.update(&mut app, |workspace, ctx| {
            // Two registered entries, so a cwd-following partition would have
            // somewhere else to file this repo's tab.
            workspace.select_repo_mode_entry(&other_root, ctx);
            workspace.select_repo_mode_entry(&repo_root, ctx);
            let group_id = workspace
                .selected_repo_mode_group_id()
                .expect("bound group should exist after select");
            let bound_tabs = workspace.repo_mode_bound_group_tab_indices(&repo_root);
            assert_eq!(bound_tabs.len(), 1, "the entry has exactly one bound tab");

            let tabs_before = workspace.tab_count();

            // Select somewhere else and back again.
            workspace.select_repo_mode_entry(&other_root, ctx);
            workspace.select_repo_mode_entry(&repo_root, ctx);

            assert_eq!(
                workspace.tab_count(),
                tabs_before,
                "reselecting a repo that already has a tab must not open another"
            );
            assert_eq!(
                workspace.tabs[workspace.active_tab_index].group_id,
                Some(group_id),
                "the existing bound tab should be the one activated"
            );
        });
    });
}

/// NA2, display half: a tab stays under the repo it is bound to, and the
/// partition reads binding only — no terminal state, so it is safe on a render
/// path.
#[test]
fn test_bound_tab_is_filed_under_its_binding_not_another_entry() {
    let _repo_mode_guard = FeatureFlag::RepoMode.override_enabled(true);
    let _grouped_tabs_guard = FeatureFlag::GroupedTabs.override_enabled(true);
    let repo = tempfile::tempdir().expect("tempdir");
    let other = tempfile::tempdir().expect("tempdir");
    let repo_root = dunce::canonicalize(repo.path()).expect("canonicalize");
    let other_root = dunce::canonicalize(other.path()).expect("canonicalize");
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        register_projects_model(&mut app, Vec::new());
        let workspace = mock_workspace(&mut app);
        workspace.update(&mut app, |workspace, ctx| {
            workspace.select_repo_mode_entry(&repo_root, ctx);
            let bound_index = workspace.active_tab_index;
            workspace.select_repo_mode_entry(&other_root, ctx);

            let entry_paths = vec![repo_root.clone(), other_root.clone()];
            let (by_entry, loose) = workspace.repo_mode_tab_partition(&entry_paths);

            assert!(
                by_entry
                    .get(&repo_root)
                    .is_some_and(|members| members.contains(&bound_index)),
                "the tab belongs to the entry it is bound to"
            );
            assert!(
                by_entry
                    .get(&other_root)
                    .is_none_or(|members| !members.contains(&bound_index)),
                "and to no other entry"
            );
            assert!(!loose.contains(&bound_index));

            // Its bound root leaving the registry drops it loose rather than
            // reassigning it to whatever else is registered.
            let (by_entry, loose) =
                workspace.repo_mode_tab_partition(std::slice::from_ref(&other_root));
            assert!(loose.contains(&bound_index));
            assert!(
                by_entry
                    .get(&other_root)
                    .is_none_or(|members| !members.contains(&bound_index)),
            );
        });
    });
}

/// NA3: activating a tab outside the selected repo collapses the selection to
/// "All", whichever route the activation came in on.
///
/// The sync used to hang off the `FocusPane` action alone, so a keyboard tab
/// switch left the filtered strip showing a repo whose tabs did not include the
/// active one. It now lives in `activate_tab_internal`, which every activation
/// route funnels through.
#[test]
fn test_keyboard_activation_outside_the_selection_falls_back_to_all() {
    let _repo_mode_guard = FeatureFlag::RepoMode.override_enabled(true);
    let _grouped_tabs_guard = FeatureFlag::GroupedTabs.override_enabled(true);
    let first = tempfile::tempdir().expect("tempdir");
    let second = tempfile::tempdir().expect("tempdir");
    let first_root = dunce::canonicalize(first.path()).expect("canonicalize");
    let second_root = dunce::canonicalize(second.path()).expect("canonicalize");
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        register_projects_model(&mut app, Vec::new());
        let workspace = mock_workspace(&mut app);
        workspace.update(&mut app, |workspace, ctx| {
            workspace.select_repo_mode_entry(&first_root, ctx);
            let first_tab = workspace.active_tab_index;
            workspace.select_repo_mode_entry(&second_root, ctx);
            assert_eq!(
                workspace.selected_repo_root.as_deref(),
                Some(second_root.to_string_lossy().as_ref()),
                "selecting the second entry should stick"
            );
            assert_ne!(workspace.active_tab_index, first_tab);

            // Activate-by-number onto the first repo's tab: a different repo,
            // so the selection cannot survive.
            workspace.handle_action(&WorkspaceAction::ActivateTabByNumber(first_tab + 1), ctx);
            assert_eq!(workspace.active_tab_index, first_tab);
            assert_eq!(
                workspace.selected_repo_root, None,
                "activating a tab outside the selection must fall back to All"
            );

            // Same for next-tab cycling, which walks the *unfiltered* tab list
            // and so always lands outside a single-tab selection.
            workspace.select_repo_mode_entry(&second_root, ctx);
            assert!(workspace.selected_repo_root.is_some());
            let before = workspace.active_tab_index;
            workspace.handle_action(&WorkspaceAction::ActivateNextTab, ctx);
            assert_ne!(
                workspace.active_tab_index, before,
                "next-tab ignores repo filtering, so it must have moved"
            );
            assert_eq!(
                workspace.selected_repo_root, None,
                "cycling off the selected repo's tab must fall back to All"
            );
        });
    });
}

/// NA3, close-tab half: the post-close activation fallback goes through the
/// same chokepoint, so closing a repo's last visible tab does not leave the
/// strip selected on a repo with nothing in it.
#[test]
fn test_closing_the_selected_repos_tab_falls_back_to_all() {
    let _repo_mode_guard = FeatureFlag::RepoMode.override_enabled(true);
    let _grouped_tabs_guard = FeatureFlag::GroupedTabs.override_enabled(true);
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dunce::canonicalize(dir.path()).expect("canonicalize");
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        register_projects_model(&mut app, Vec::new());
        let workspace = mock_workspace(&mut app);
        workspace.update(&mut app, |workspace, ctx| {
            workspace.select_repo_mode_entry(&root, ctx);
            let repo_tab = workspace.active_tab_index;
            assert!(workspace.tab_count() > 1, "a loose tab remains to fall to");

            workspace.close_tab(repo_tab, true, false, ctx);

            assert_eq!(
                workspace.selected_repo_root, None,
                "closing the selected repo's only tab must fall back to All"
            );
        });
    });
}

/// Covers R6: "+ New" in the "Other tabs" section opens a tab bound to no
/// repo group, even when a repo tab is active — the stock new-tab path would
/// otherwise inherit the active tab's group and file it under that repo.
#[test]
fn test_loose_tab_never_joins_the_active_repo_group() {
    let _repo_mode_guard = FeatureFlag::RepoMode.override_enabled(true);
    let _grouped_tabs_guard = FeatureFlag::GroupedTabs.override_enabled(true);
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dunce::canonicalize(dir.path()).expect("canonicalize");
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        register_projects_model(&mut app, Vec::new());
        let workspace = mock_workspace(&mut app);
        workspace.update(&mut app, |workspace, ctx| {
            // Active tab is the repo's tab, so the inherit path is live.
            workspace.select_repo_mode_entry(&root, ctx);
            let group_id = workspace.selected_repo_mode_group_id().expect("group");
            assert_eq!(
                workspace.tabs[workspace.active_tab_index].group_id,
                Some(group_id)
            );

            workspace.new_repo_mode_loose_tab(ctx);

            assert_eq!(workspace.selected_repo_root, None);
            let new_index = workspace.active_tab_index;
            assert_eq!(workspace.tabs[new_index].group_id, None);
            // ... and it renders under "Other tabs", not under the repo row.
            let entry_paths = vec![root.clone()];
            let (by_entry, loose) = workspace.repo_mode_tab_partition(&entry_paths);
            assert!(loose.contains(&new_index));
            assert!(
                !by_entry
                    .get(&root)
                    .is_some_and(|members| members.contains(&new_index))
            );
        });
    });
}

/// Covers R11 / F5: closing the last tab of the selected entry falls back to
/// "All" without auto-opening a fresh tab.
#[test]
fn test_last_tab_close_falls_back_to_all() {
    let _repo_mode_guard = FeatureFlag::RepoMode.override_enabled(true);
    let _grouped_tabs_guard = FeatureFlag::GroupedTabs.override_enabled(true);
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dunce::canonicalize(dir.path()).expect("canonicalize");
    let root_str = root.to_string_lossy().into_owned();
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        register_projects_model(&mut app, Vec::new());
        let workspace = mock_workspace(&mut app);
        workspace.update(&mut app, |workspace, ctx| {
            workspace.select_repo_mode_entry(&root, ctx);
            let group_id = workspace.selected_repo_mode_group_id().expect("group");
            let member_index = workspace
                .tabs
                .iter()
                .position(|t| t.group_id == Some(group_id))
                .expect("member tab");

            let tab_count_before = workspace.tabs.len();
            workspace.remove_tab(member_index, false, false, ctx);

            // Selection falls back to "All"; no group or tab is recreated (R11).
            assert_eq!(workspace.selected_repo_root, None);
            assert!(
                !workspace
                    .tab_groups
                    .values()
                    .any(|g| g.repo_root.as_deref() == Some(root_str.as_str())),
                "bound group should stay pruned after last tab close"
            );
            assert_eq!(workspace.tabs.len(), tab_count_before - 1);
        });
    });
}

/// Covers R11 (window level): closing the window's very last tab with repo
/// mode on opens a fresh loose home terminal instead of closing the window.
#[test]
fn test_window_last_tab_close_opens_loose_home_tab() {
    let _repo_mode_guard = FeatureFlag::RepoMode.override_enabled(true);
    let _grouped_tabs_guard = FeatureFlag::GroupedTabs.override_enabled(true);
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        register_projects_model(&mut app, Vec::new());
        let workspace = mock_workspace(&mut app);
        workspace.update(&mut app, |workspace, ctx| {
            assert_eq!(workspace.tabs.len(), 1);
            let old_pane_group_id = workspace.tabs[0].pane_group.id();

            workspace.remove_tab(0, false, false, ctx);

            // The old tab is gone, replaced by a single ungrouped tab; the
            // window survives with no repo selected.
            assert_eq!(workspace.tabs.len(), 1);
            assert_ne!(workspace.tabs[0].pane_group.id(), old_pane_group_id);
            assert_eq!(workspace.tabs[0].group_id, None);
            assert_eq!(workspace.selected_repo_root, None);
            assert_eq!(workspace.active_tab_index, 0);
        });
    });
}

/// Covers R4: removing an entry ungroups its tabs without closing them and
/// clears the selection.
#[test]
fn test_remove_entry_ungroups_tabs_and_keeps_them_open() {
    let _repo_mode_guard = FeatureFlag::RepoMode.override_enabled(true);
    let _grouped_tabs_guard = FeatureFlag::GroupedTabs.override_enabled(true);
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dunce::canonicalize(dir.path()).expect("canonicalize");
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        register_projects_model(&mut app, Vec::new());
        let workspace = mock_workspace(&mut app);
        workspace.update(&mut app, |workspace, ctx| {
            workspace.select_repo_mode_entry(&root, ctx);
            workspace.add_terminal_tab(false, ctx);
            let tabs_before = workspace.tab_count();
            let group_id = workspace.selected_repo_mode_group_id().expect("group");
            assert!(workspace.tabs.iter().any(|t| t.group_id == Some(group_id)));

            workspace.remove_repo_mode_entry(&root, ctx);

            assert_eq!(workspace.tab_count(), tabs_before);
            assert!(workspace.tabs.iter().all(|t| t.group_id != Some(group_id)));
            assert_eq!(workspace.selected_repo_root, None);
        });
    });
}

/// Display rule: a bound tab belongs to the entry it is bound to, full stop.
///
/// This is the inverse of the original rule, which followed the terminal's
/// live cwd and let a `cd` move a tab out of its own repo's row. The helper
/// only ever sees repo-bound tabs — loose tabs stay under "Other tabs"
/// unconditionally, which is what keeps a newly registered repository from
/// absorbing a pre-existing loose tab whose cwd happens to live inside it.
#[test]
fn test_bound_tab_owner_rules() {
    let entries = vec![
        PathBuf::from("/repo/a"),
        PathBuf::from("/repo/a/nested"),
        PathBuf::from("/repo/b"),
    ];
    // The binding wins, whatever any terminal's cwd is doing.
    assert_eq!(
        repo_mode_bound_tab_owner(Path::new("/repo/b"), &entries),
        Some(PathBuf::from("/repo/b"))
    );
    // A nested entry is only an owner for tabs actually bound to it — being an
    // ancestor of, or nested inside, another entry changes nothing.
    assert_eq!(
        repo_mode_bound_tab_owner(Path::new("/repo/a/nested"), &entries),
        Some(PathBuf::from("/repo/a/nested"))
    );
    assert_eq!(
        repo_mode_bound_tab_owner(Path::new("/repo/a"), &entries),
        Some(PathBuf::from("/repo/a"))
    );
    // Bound root no longer registered: the tab drops loose.
    assert_eq!(
        repo_mode_bound_tab_owner(Path::new("/repo/gone"), &entries),
        None
    );
    // A path under an entry is not the entry. Only an exact bound root owns.
    assert_eq!(
        repo_mode_bound_tab_owner(Path::new("/repo/a/nested/src"), &entries),
        None
    );
}

/// Covers R9/R11: the row for a remote key is built from the ephemeral probe
/// cache alone. With nothing cached the row is pending, and building it touches
/// neither the filesystem nor the network.
#[test]
fn remote_row_is_pending_until_a_probe_resolves() {
    let key = format_remote_key("10.0.0.7", 2222, "vinh", "/k", "/srv/app");
    let now = Utc::now().naive_utc();
    let mut probes = HashMap::new();

    let pending = remote_list_entry(key.clone(), PathBuf::from(&key), Some(now), now, &probes);
    let remote = pending.remote.as_ref().expect("remote detail");
    assert_eq!(remote.probe, RemoteProbeState::Pending);
    assert_eq!(remote.target.user_host(), "vinh@10.0.0.7");
    assert_eq!(pending.display_name, "app");
    // R11: an unreachable remote entry is never the local "dead path" state.
    assert!(!pending.is_dead);

    probes.insert(
        key.clone(),
        settled_session(RemoteProbeState::Resolved {
            kind: RepoEntryKind::Repo,
            branch: Some("main".to_string()),
        }),
    );
    let resolved = remote_list_entry(key.clone(), PathBuf::from(&key), Some(now), now, &probes);
    assert_eq!(resolved.kind, RepoEntryKind::Repo);
    assert_eq!(
        resolved.remote.expect("remote detail").probe,
        RemoteProbeState::Resolved {
            kind: RepoEntryKind::Repo,
            branch: Some("main".to_string()),
        }
    );

    probes.insert(
        key.clone(),
        settled_session(RemoteProbeState::Failed {
            reason: repo_mode::RemoteProbeFailure::Unreachable,
        }),
    );
    let failed = remote_list_entry(key.clone(), PathBuf::from(&key), Some(now), now, &probes);
    assert!(!failed.is_dead);
    assert_eq!(failed.kind, RepoEntryKind::Folder);
}

/// A registry row that carries the remote scheme but does not parse (a
/// hand-edited or corrupted row) is surfaced as dead so the row offers
/// "Remove", instead of rendering as a live local folder that can never open.
#[test]
fn unparseable_remote_row_is_dead() {
    let now = Utc::now().naive_utc();
    let key = "ssh://not-a-valid-key".to_string();
    let entry = remote_list_entry(
        key.clone(),
        PathBuf::from(&key),
        Some(now),
        now,
        &HashMap::new(),
    );
    assert!(entry.remote.is_none());
    assert!(entry.is_dead);
}

/// Covers KTD3 at the model seam: remove/select derive their registry key
/// without canonicalizing a remote key against the local filesystem.
#[test]
fn registry_key_path_leaves_remote_keys_untouched() {
    let key = format_remote_key("h", 22, "u", "/k", "/srv/app");
    assert_eq!(
        registry_key_path(Path::new(&key), "select"),
        PathBuf::from(&key)
    );

    let dir = tempfile::tempdir().expect("tempdir");
    let canonical = dunce::canonicalize(dir.path()).expect("canonicalize");
    let with_slash = PathBuf::from(format!("{}/", dir.path().display()));
    assert_eq!(registry_key_path(&with_slash, "select"), canonical);
}

/// Covers R1: the single "+ Add" control offers exactly the local and remote
/// destinations.
#[test]
fn add_menu_offers_local_and_remote() {
    let entries = repo_mode_add_menu_entries();
    assert_eq!(entries[0].0, "Local Repository or Folder…");
    assert!(matches!(
        entries[0].1,
        WorkspaceAction::AddLocalRepositoryOrFolder
    ));
    assert_eq!(entries[1].0, "Remote Repository or Folder…");
    assert!(matches!(
        entries[1].1,
        WorkspaceAction::AddRemoteRepositoryOrFolder
    ));
}

/// The remote action classifies like its local sibling: registering an entry is
/// workspace state worth persisting.
#[test]
fn remote_add_action_is_classified_like_the_local_one() {
    assert_eq!(
        WorkspaceAction::AddRemoteRepositoryOrFolder.should_save_app_state_on_action(),
        WorkspaceAction::AddLocalRepositoryOrFolder.should_save_app_state_on_action()
    );
}

/// Covers R3/R8/R9: the row is registered pending under the typed path and
/// resolves onto the path the *host* expanded, leaving no stale key behind.
#[test]
fn test_probe_success_resolves_the_row_onto_the_expanded_path() {
    let _repo_mode_guard = FeatureFlag::RepoMode.override_enabled(true);
    let _grouped_tabs_guard = FeatureFlag::GroupedTabs.override_enabled(true);
    let target = RemoteTarget {
        server: "10.0.0.7".to_string(),
        port: 22,
        user: "vinh".to_string(),
        identity: "/k".to_string(),
        remote_path: "~/app".to_string(),
    };
    let pending_key = target.key();
    let now = Utc::now().naive_utc();
    let projects = vec![Project {
        path: pending_key.clone(),
        added_ts: now,
        last_opened_ts: Some(now),
        manual_position: None,
    }];
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        register_projects_model(&mut app, projects);
        let workspace = mock_workspace(&mut app);
        workspace.update(&mut app, |workspace, ctx| {
            let generation = workspace.restart_remote_probe(&pending_key);

            workspace.apply_remote_probe_result(
                &target,
                &pending_key,
                generation,
                None,
                Ok(RemoteProbeOutcome::Found {
                    remote_path: "/home/vinh/app".to_string(),
                    kind: RepoEntryKind::Repo,
                    branch: Some("main".to_string()),
                }),
                ctx,
            );

            let resolved_key = RemoteTarget {
                remote_path: "/home/vinh/app".to_string(),
                ..target.clone()
            }
            .key();
            assert_eq!(registered_paths(ctx), vec![resolved_key.clone()]);

            let entries = workspace.repo_mode_entries(ctx);
            assert_eq!(entries.len(), 1);
            assert_eq!(entries[0].kind, RepoEntryKind::Repo);
            assert_eq!(
                entries[0].remote.as_ref().expect("remote detail").probe,
                RemoteProbeState::Resolved {
                    kind: RepoEntryKind::Repo,
                    branch: Some("main".to_string()),
                }
            );
        });
    });
}

/// Covers R6/R7/AE3: a failed add leaves nothing behind — no row, no cache
/// entry — so the user is not left with a half-registered machine.
#[test]
fn test_probe_failure_at_add_time_registers_nothing() {
    let _repo_mode_guard = FeatureFlag::RepoMode.override_enabled(true);
    let _grouped_tabs_guard = FeatureFlag::GroupedTabs.override_enabled(true);
    let target = RemoteTarget {
        server: "10.0.0.7".to_string(),
        port: 22,
        user: "vinh".to_string(),
        identity: "/k".to_string(),
        remote_path: "/srv/app".to_string(),
    };
    let key = target.key();
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        register_projects_model(&mut app, Vec::new());
        let workspace = mock_workspace(&mut app);
        workspace.update(&mut app, |workspace, ctx| {
            let generation = workspace.restart_remote_probe(&key);

            // The pending row is visible while the probe runs, and it is
            // visible without being registered — the registry means "verified".
            assert_eq!(workspace.repo_mode_entries(ctx).len(), 1);
            assert!(registered_paths(ctx).is_empty());

            workspace.apply_remote_probe_result(
                &target,
                &key,
                generation,
                Some(1),
                Err(repo_mode::RemoteProbeFailure::Unreachable),
                ctx,
            );

            assert!(registered_paths(ctx).is_empty());
            assert!(workspace.repo_mode_entries(ctx).is_empty());
            assert!(
                !workspace
                    .repo_mode_remote_probes
                    .borrow()
                    .contains_key(&key)
            );
        });
    });
}

/// A *reprobe* failure is not an add failure: the entry the user already has
/// stays, marked unreachable, instead of disappearing over a network blip
/// (R11).
#[test]
fn test_reprobe_failure_keeps_the_entry_and_marks_it_unreachable() {
    let _repo_mode_guard = FeatureFlag::RepoMode.override_enabled(true);
    let _grouped_tabs_guard = FeatureFlag::GroupedTabs.override_enabled(true);
    let target = RemoteTarget {
        server: "10.0.0.7".to_string(),
        port: 22,
        user: "vinh".to_string(),
        identity: "/k".to_string(),
        remote_path: "/srv/app".to_string(),
    };
    let key = target.key();
    let now = Utc::now().naive_utc();
    let projects = vec![Project {
        path: key.clone(),
        added_ts: now,
        last_opened_ts: Some(now),
        manual_position: None,
    }];
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        register_projects_model(&mut app, projects);
        let workspace = mock_workspace(&mut app);
        workspace.update(&mut app, |workspace, ctx| {
            let generation = workspace
                .begin_remote_probe(&key)
                .expect("no probe is running for a freshly restored entry");
            workspace.apply_remote_probe_result(
                &target,
                &key,
                generation,
                None,
                Err(repo_mode::RemoteProbeFailure::NeedsFirstHandConnect),
                ctx,
            );

            assert_eq!(registered_paths(ctx), vec![key.clone()]);
            let entries = workspace.repo_mode_entries(ctx);
            assert_eq!(
                entries[0].remote.as_ref().expect("remote detail").probe,
                RemoteProbeState::Failed {
                    reason: repo_mode::RemoteProbeFailure::NeedsFirstHandConnect
                }
            );
            assert!(!entries[0].is_dead);
        });
    });
}

/// Covers R5/KTD3: a remote key is stored exactly as formatted — canonicalizing
/// it would resolve a remote directory against the *local* filesystem — and a
/// second upsert bumps recency instead of duplicating the row.
#[test]
fn test_remote_entry_stores_key_verbatim_and_lists_pending() {
    let _repo_mode_guard = FeatureFlag::RepoMode.override_enabled(true);
    let _grouped_tabs_guard = FeatureFlag::GroupedTabs.override_enabled(true);
    let key = format_remote_key("10.0.0.7", 2222, "vinh", "/Users/v/.ssh/id", "/srv/app");
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        register_projects_model(&mut app, Vec::new());
        let workspace = mock_workspace(&mut app);
        workspace.update(&mut app, |workspace, ctx| {
            ProjectManagementModel::handle(ctx).update(ctx, |projects, ctx| {
                projects.upsert_project(PathBuf::from(&key), ctx);
                projects.upsert_project(PathBuf::from(&key), ctx);
            });
            assert_eq!(registered_paths(ctx), vec![key.clone()]);

            let entries = workspace.repo_mode_entries(ctx);
            assert_eq!(entries.len(), 1);
            let entry = &entries[0];
            let remote = entry.remote.as_ref().expect("remote detail");
            assert_eq!(remote.target.server, "10.0.0.7");
            assert_eq!(remote.target.port, 2222);
            assert_eq!(remote.target.remote_path, "/srv/app");
            // R9/R11: nothing has probed yet, so the row is pending and no
            // filesystem or network call was made to build the list.
            assert_eq!(remote.probe, RemoteProbeState::Pending);
            assert_eq!(entry.display_name, "app");
            assert!(!entry.is_dead);
        });
    });
}

/// Covers AE1/AE2: the machine is part of the identity, so the same path on two
/// hosts — and a local entry with a matching path — are three distinct rows.
#[test]
fn test_remote_and_local_entries_with_same_path_coexist() {
    let _repo_mode_guard = FeatureFlag::RepoMode.override_enabled(true);
    let _grouped_tabs_guard = FeatureFlag::GroupedTabs.override_enabled(true);
    let now = Utc::now().naive_utc();
    let key_a = format_remote_key("10.0.0.1", 22, "vinh", "/k", "/srv/app");
    let key_b = format_remote_key("10.0.0.2", 22, "vinh", "/k", "/srv/app");
    let projects = vec![
        Project {
            path: "/srv/app".to_string(),
            added_ts: now,
            last_opened_ts: Some(now),
            manual_position: None,
        },
        Project {
            path: key_a.clone(),
            added_ts: now,
            last_opened_ts: Some(now),
            manual_position: None,
        },
        Project {
            path: key_b.clone(),
            added_ts: now,
            last_opened_ts: Some(now),
            manual_position: None,
        },
    ];
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        register_projects_model(&mut app, projects);
        let workspace = mock_workspace(&mut app);
        workspace.update(&mut app, |workspace, ctx| {
            let entries = workspace.repo_mode_entries(ctx);
            assert_eq!(entries.len(), 3);
            let remote_hosts: Vec<String> = entries
                .iter()
                .filter_map(|e| e.remote.as_ref())
                .map(|r| r.target.server.clone())
                .collect();
            assert_eq!(remote_hosts.len(), 2);
            assert!(remote_hosts.contains(&"10.0.0.1".to_string()));
            assert!(remote_hosts.contains(&"10.0.0.2".to_string()));
            assert_eq!(entries.iter().filter(|e| e.remote.is_none()).count(), 1);
        });
    });
}

/// Covers R5: removal keys on the raw path, so a remote entry leaves the
/// listing and the registry together.
#[test]
fn test_remove_remote_entry_drops_it_from_listing_and_registry() {
    let _repo_mode_guard = FeatureFlag::RepoMode.override_enabled(true);
    let _grouped_tabs_guard = FeatureFlag::GroupedTabs.override_enabled(true);
    let now = Utc::now().naive_utc();
    let key = format_remote_key("10.0.0.7", 22, "vinh", "/k", "/srv/app");
    let projects = vec![Project {
        path: key.clone(),
        added_ts: now,
        last_opened_ts: Some(now),
        manual_position: None,
    }];
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        register_projects_model(&mut app, projects);
        let workspace = mock_workspace(&mut app);
        workspace.update(&mut app, |workspace, ctx| {
            assert_eq!(workspace.repo_mode_entries(ctx).len(), 1);

            workspace.remove_repo_mode_entry(Path::new(&key), ctx);

            assert!(workspace.repo_mode_entries(ctx).is_empty());
            assert!(registered_paths(ctx).is_empty());
        });
    });
}

/// A registry row that looks remote but does not parse (hand-edited or
/// corrupted) is surfaced as dead so the user can remove it, rather than
/// rendering as a live local folder that can never open.
#[test]
fn test_unparseable_remote_key_is_marked_dead() {
    let _repo_mode_guard = FeatureFlag::RepoMode.override_enabled(true);
    let _grouped_tabs_guard = FeatureFlag::GroupedTabs.override_enabled(true);
    let now = Utc::now().naive_utc();
    let projects = vec![Project {
        path: "ssh://not-a-valid-key".to_string(),
        added_ts: now,
        last_opened_ts: Some(now),
        manual_position: None,
    }];
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        register_projects_model(&mut app, projects);
        let workspace = mock_workspace(&mut app);
        workspace.update(&mut app, |workspace, ctx| {
            let entries = workspace.repo_mode_entries(ctx);
            assert_eq!(entries.len(), 1);
            assert!(entries[0].remote.is_none());
            assert!(entries[0].is_dead);
        });
    });
}

/// Covers R3: section order reflects recency at launch and does not reshuffle
/// when a different entry is selected mid-session, while the recency bump is
/// still recorded for the next launch.
#[test]
fn test_recency_order_settles_at_launch() {
    let _repo_mode_guard = FeatureFlag::RepoMode.override_enabled(true);
    let _grouped_tabs_guard = FeatureFlag::GroupedTabs.override_enabled(true);
    let dir_a = tempfile::tempdir().expect("tempdir");
    let dir_b = tempfile::tempdir().expect("tempdir");
    let root_a = dunce::canonicalize(dir_a.path()).expect("canonicalize");
    let root_b = dunce::canonicalize(dir_b.path()).expect("canonicalize");
    let now = Utc::now().naive_utc();
    let projects = vec![
        Project {
            path: root_a.to_string_lossy().into_owned(),
            added_ts: now - Duration::days(2),
            last_opened_ts: Some(now - Duration::days(2)),
            manual_position: None,
        },
        Project {
            path: root_b.to_string_lossy().into_owned(),
            added_ts: now - Duration::days(1),
            last_opened_ts: Some(now - Duration::days(1)),
            manual_position: None,
        },
    ];
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        register_projects_model(&mut app, projects);
        let workspace = mock_workspace(&mut app);
        workspace.update(&mut app, |workspace, ctx| {
            // B was used more recently, so it leads at launch.
            let order: Vec<_> = workspace
                .repo_mode_entries(ctx)
                .into_iter()
                .map(|e| e.path)
                .collect();
            assert_eq!(order, vec![root_b.clone(), root_a.clone()]);

            // Selecting A bumps its recency for the NEXT launch...
            workspace.select_repo_mode_entry(&root_a, ctx);
            let bumped = ProjectManagementModel::handle(ctx).read(ctx, |projects, _| {
                projects
                    .all_projects()
                    .find(|p| p.path == root_a.to_string_lossy())
                    .and_then(|p| p.last_opened_ts)
            });
            assert!(bumped.expect("last_opened_ts") > now - Duration::days(2));

            // ...but the section order stays pinned this session.
            let order_after: Vec<_> = workspace
                .repo_mode_entries(ctx)
                .into_iter()
                .map(|e| e.path)
                .collect();
            assert_eq!(order_after, vec![root_b, root_a]);
        });
    });
}

/// Registry rows whose recency order is the argument order: the first path is
/// the most recently used, each later one a day older. Callers pass paths that
/// are *not* in alphabetical order, so an assertion on the rendered list tells
/// recency apart from the display-name tiebreaker.
fn projects_by_recency(paths: &[&str]) -> Vec<Project> {
    let now = Utc::now().naive_utc();
    paths
        .iter()
        .enumerate()
        .map(|(index, path)| {
            let used = now - Duration::days(index as i64);
            Project {
                path: (*path).to_string(),
                added_ts: used,
                last_opened_ts: Some(used),
                manual_position: None,
            }
        })
        .collect()
}

/// The order the Repositories section renders, as registry keys.
fn rendered_order(workspace: &Workspace, ctx: &AppContext) -> Vec<String> {
    workspace
        .repo_mode_entries(ctx)
        .into_iter()
        .map(|entry| entry.path.to_string_lossy().into_owned())
        .collect()
}

/// Hand the registry over to a manual order, as a completed drag does.
fn apply_manual_order(paths: &[&str], ctx: &mut ViewContext<Workspace>) {
    let ordered: Vec<PathBuf> = paths.iter().map(|path| PathBuf::from(*path)).collect();
    ProjectManagementModel::handle(ctx).update(ctx, |projects, ctx| {
        projects.set_manual_order(ordered, ctx);
    });
}

/// Covers AE2/R3: with no manual order on the registry the section orders by
/// recency exactly as it did before manual ordering existed.
#[test]
fn test_section_orders_by_recency_without_a_manual_order() {
    let _repo_mode_guard = FeatureFlag::RepoMode.override_enabled(true);
    let _grouped_tabs_guard = FeatureFlag::GroupedTabs.override_enabled(true);
    let projects = projects_by_recency(&["/repo/c", "/repo/a", "/repo/b"]);
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        register_projects_model(&mut app, projects);
        let workspace = mock_workspace(&mut app);
        workspace.update(&mut app, |workspace, ctx| {
            assert_eq!(
                rendered_order(workspace, ctx),
                ["/repo/c", "/repo/a", "/repo/b"]
            );
        });
    });
}

/// Covers AE1/R2/R6: once the registry carries a manual order it owns the list
/// — a window drawing for the first time renders that order, not recency.
#[test]
fn test_manual_order_replaces_recency_for_the_section() {
    let _repo_mode_guard = FeatureFlag::RepoMode.override_enabled(true);
    let _grouped_tabs_guard = FeatureFlag::GroupedTabs.override_enabled(true);
    let projects = projects_by_recency(&["/repo/c", "/repo/a", "/repo/b"]);
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        register_projects_model(&mut app, projects);
        let workspace = mock_workspace(&mut app);
        workspace.update(&mut app, |workspace, ctx| {
            // Another window's drag. This window has not drawn its list yet, so
            // it adopts the order rather than pinning recency.
            apply_manual_order(&["/repo/a", "/repo/b", "/repo/c"], ctx);

            assert_eq!(
                rendered_order(workspace, ctx),
                ["/repo/a", "/repo/b", "/repo/c"]
            );
        });
    });
}

/// Covers AE3/R4: a repository registered while a manual order is in effect
/// carries no position, so it appends at the end — even though its recency is
/// the newest in the registry.
#[test]
fn test_a_repository_added_under_a_manual_order_renders_last() {
    let _repo_mode_guard = FeatureFlag::RepoMode.override_enabled(true);
    let _grouped_tabs_guard = FeatureFlag::GroupedTabs.override_enabled(true);
    let projects = projects_by_recency(&["/repo/c", "/repo/a", "/repo/b"]);
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        register_projects_model(&mut app, projects);
        let workspace = mock_workspace(&mut app);
        workspace.update(&mut app, |workspace, ctx| {
            apply_manual_order(&["/repo/a", "/repo/b", "/repo/c"], ctx);
            ProjectManagementModel::handle(ctx).update(ctx, |projects, ctx| {
                projects.upsert_project(PathBuf::from("/repo/d"), ctx);
            });

            assert_eq!(
                rendered_order(workspace, ctx),
                ["/repo/a", "/repo/b", "/repo/c", "/repo/d"]
            );
        });
    });
}

/// Covers AE4/R7: a window that has already drawn its list keeps that order for
/// the rest of its session. A manual order another window writes afterwards is
/// adopted at the next launch, not under the user's cursor.
#[test]
fn test_a_window_that_already_drew_its_list_ignores_a_later_manual_order() {
    let _repo_mode_guard = FeatureFlag::RepoMode.override_enabled(true);
    let _grouped_tabs_guard = FeatureFlag::GroupedTabs.override_enabled(true);
    let projects = projects_by_recency(&["/repo/c", "/repo/a", "/repo/b"]);
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        register_projects_model(&mut app, projects);
        let workspace = mock_workspace(&mut app);
        workspace.update(&mut app, |workspace, ctx| {
            assert_eq!(
                rendered_order(workspace, ctx),
                ["/repo/c", "/repo/a", "/repo/b"],
                "the first draw pins recency"
            );

            apply_manual_order(&["/repo/a", "/repo/b", "/repo/c"], ctx);

            assert_eq!(
                rendered_order(workspace, ctx),
                ["/repo/c", "/repo/a", "/repo/b"],
                "the pinned order survives the handover"
            );
        });
    });
}

/// Covers AE5/R9: recency keeps being recorded under a manual order — it is
/// what Reset order restores to — but it moves no row.
#[test]
fn test_selecting_under_a_manual_order_bumps_recency_without_moving_rows() {
    let _repo_mode_guard = FeatureFlag::RepoMode.override_enabled(true);
    let _grouped_tabs_guard = FeatureFlag::GroupedTabs.override_enabled(true);
    let dir_a = tempfile::tempdir().expect("tempdir");
    let dir_b = tempfile::tempdir().expect("tempdir");
    let dir_c = tempfile::tempdir().expect("tempdir");
    let root_a = dunce::canonicalize(dir_a.path()).expect("canonicalize");
    let root_b = dunce::canonicalize(dir_b.path()).expect("canonicalize");
    let root_c = dunce::canonicalize(dir_c.path()).expect("canonicalize");
    let key_a = root_a.to_string_lossy().into_owned();
    let key_b = root_b.to_string_lossy().into_owned();
    let key_c = root_c.to_string_lossy().into_owned();
    // Recency runs a, b, c; the manual order is its reverse.
    let projects = projects_by_recency(&[&key_a, &key_b, &key_c]);
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        register_projects_model(&mut app, projects);
        let workspace = mock_workspace(&mut app);
        workspace.update(&mut app, |workspace, ctx| {
            apply_manual_order(&[&key_c, &key_b, &key_a], ctx);
            let before = Utc::now().naive_utc();

            workspace.select_repo_mode_entry(&root_a, ctx);
            workspace.select_repo_mode_entry(&root_b, ctx);

            let bumped = ProjectManagementModel::handle(ctx).read(ctx, |projects, _| {
                projects
                    .all_projects()
                    .filter(|project| project.last_opened_ts.is_some_and(|used| used >= before))
                    .map(|project| project.path.clone())
                    .collect::<HashSet<String>>()
            });
            assert_eq!(
                bumped,
                HashSet::from([key_a.clone(), key_b.clone()]),
                "both selections are recorded for the next launch"
            );
            assert_eq!(
                rendered_order(workspace, ctx),
                [key_c.as_str(), key_b.as_str(), key_a.as_str()],
                "and neither one moves a row"
            );
        });
    });
}

/// Covers AE7/R5: removing a repository takes its position with it, so adding
/// it back appends at the end rather than resurfacing mid-list.
#[test]
fn test_removing_and_re_adding_a_repository_renders_it_last() {
    let _repo_mode_guard = FeatureFlag::RepoMode.override_enabled(true);
    let _grouped_tabs_guard = FeatureFlag::GroupedTabs.override_enabled(true);
    let projects = projects_by_recency(&["/repo/c", "/repo/a", "/repo/b"]);
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        register_projects_model(&mut app, projects);
        let workspace = mock_workspace(&mut app);
        workspace.update(&mut app, |workspace, ctx| {
            apply_manual_order(&["/repo/a", "/repo/b", "/repo/c"], ctx);
            assert_eq!(
                rendered_order(workspace, ctx),
                ["/repo/a", "/repo/b", "/repo/c"]
            );

            workspace.remove_repo_mode_entry(Path::new("/repo/b"), ctx);
            assert_eq!(rendered_order(workspace, ctx), ["/repo/a", "/repo/c"]);

            ProjectManagementModel::handle(ctx).update(ctx, |projects, ctx| {
                projects.upsert_project(PathBuf::from("/repo/b"), ctx);
            });
            assert_eq!(
                rendered_order(workspace, ctx),
                ["/repo/a", "/repo/c", "/repo/b"]
            );
        });
    });
}

/// A remote row whose first probe has not landed is projected into the list and
/// is deliberately never in the registry, so it is never in the manual order
/// either. It stays at the top where its `now` timestamp puts it: a
/// "Connecting…" row at the bottom of a long list can sit below the fold with
/// nothing to scroll it into view.
#[test]
fn test_a_pending_remote_row_stays_above_the_manual_order() {
    let _repo_mode_guard = FeatureFlag::RepoMode.override_enabled(true);
    let _grouped_tabs_guard = FeatureFlag::GroupedTabs.override_enabled(true);
    let key = probe_target("/srv/app").key();
    let projects = projects_by_recency(&["/repo/c", "/repo/a", "/repo/b"]);
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        register_projects_model(&mut app, projects);
        let workspace = mock_workspace(&mut app);
        workspace.update(&mut app, |workspace, ctx| {
            apply_manual_order(&["/repo/a", "/repo/b", "/repo/c"], ctx);
            workspace.restart_remote_probe(&key);

            assert_eq!(
                rendered_order(workspace, ctx),
                [key.as_str(), "/repo/a", "/repo/b", "/repo/c"]
            );
        });
    });
}

/// A three-row list, in the order the section renders it.
fn ordered_rows(paths: &[&str]) -> Vec<PathBuf> {
    paths.iter().map(PathBuf::from).collect()
}

/// Covers R10. From the middle of the list each direction resolves to the
/// adjacent row, which is the pair the caller swaps once the midpoints cross.
#[test]
fn test_row_neighbor_moves_within_the_repository_list() {
    let rows = ordered_rows(&["/repo/a", "/repo/b", "/repo/c"]);

    assert_eq!(
        repo_mode_row_neighbor(&rows, Path::new("/repo/b"), false),
        Some(0)
    );
    assert_eq!(
        repo_mode_row_neighbor(&rows, Path::new("/repo/b"), true),
        Some(2)
    );
}

/// Covers R11. The ends of the Repositories list clamp: there is no index
/// above the first row and none below the last, so no drag can reach past the
/// list into "Other tabs" below it.
#[test]
fn test_row_neighbor_clamps_at_the_list_ends() {
    let rows = ordered_rows(&["/repo/a", "/repo/b", "/repo/c"]);

    assert_eq!(
        repo_mode_row_neighbor(&rows, Path::new("/repo/a"), false),
        None,
        "nothing above the first row"
    );
    assert_eq!(
        repo_mode_row_neighbor(&rows, Path::new("/repo/c"), true),
        None,
        "nothing below the last row — 'Other tabs' is not a swap target"
    );
    // And the inward directions from those same ends still resolve, so the
    // clamp is per-direction rather than "an end row cannot move".
    assert_eq!(
        repo_mode_row_neighbor(&rows, Path::new("/repo/a"), true),
        Some(1)
    );
    assert_eq!(
        repo_mode_row_neighbor(&rows, Path::new("/repo/c"), false),
        Some(1)
    );
}

/// A single registered repository is both the first and the last row, so it
/// has nowhere to go in either direction.
#[test]
fn test_row_neighbor_is_absent_for_a_single_row_list() {
    let rows = ordered_rows(&["/repo/a"]);

    assert_eq!(
        repo_mode_row_neighbor(&rows, Path::new("/repo/a"), false),
        None
    );
    assert_eq!(
        repo_mode_row_neighbor(&rows, Path::new("/repo/a"), true),
        None
    );
}

/// A drag can outlive its row: another window can remove the repository
/// mid-drag, and the empty list is the same case. Both clamp rather than
/// panic.
#[test]
fn test_row_neighbor_is_absent_for_an_unlisted_path() {
    let rows = ordered_rows(&["/repo/a", "/repo/b"]);

    assert_eq!(
        repo_mode_row_neighbor(&rows, Path::new("/repo/gone"), false),
        None
    );
    assert_eq!(
        repo_mode_row_neighbor(&rows, Path::new("/repo/gone"), true),
        None
    );
    assert_eq!(
        repo_mode_row_neighbor(&[], Path::new("/repo/a"), true),
        None
    );
}

/// Covers R14/KTD8: a remote entry gets the same group binding a local one
/// does — one group keyed by the entry's registry key (here the remote key),
/// with the opened tab as its member. That binding is what makes selecting the
/// row filter the tab UI, so it must not depend on the key being a real path.
#[test]
fn test_remote_entry_opens_a_tab_bound_to_its_key() {
    let _repo_mode_guard = FeatureFlag::RepoMode.override_enabled(true);
    let _grouped_tabs_guard = FeatureFlag::GroupedTabs.override_enabled(true);
    let key = format_remote_key("10.0.0.7", 2222, "vinh", "", "/srv/app");
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        register_projects_model(&mut app, Vec::new());
        let workspace = mock_workspace(&mut app);
        workspace.update(&mut app, |workspace, ctx| {
            ProjectManagementModel::handle(ctx).update(ctx, |projects, ctx| {
                projects.upsert_project(PathBuf::from(&key), ctx);
            });
            let initial_tabs = workspace.tab_count();

            workspace.select_repo_mode_entry(Path::new(&key), ctx);

            assert_eq!(workspace.tab_count(), initial_tabs + 1);
            let group_id = workspace
                .selected_repo_mode_group_id()
                .expect("remote entry should bind a group");
            let group = workspace.tab_groups.get(&group_id).expect("group");
            // The group is keyed by the key as typed — no canonicalization, or
            // it would no longer match the registry entry (KTD3).
            assert_eq!(group.repo_root.as_deref(), Some(key.as_str()));
            assert_eq!(group.name.as_deref(), Some("app"));
            assert_eq!(
                workspace.tabs[workspace.active_tab_index].group_id,
                Some(group_id)
            );

            // Selecting again reuses the group rather than minting a second one
            // bound to the same key.
            workspace.select_repo_mode_entry(Path::new(&key), ctx);
            assert_eq!(
                workspace
                    .tab_groups
                    .values()
                    .filter(|g| g.repo_root.as_deref() == Some(key.as_str()))
                    .count(),
                1
            );
        });
    });
}

/// Covers R15/AE6: only the entry-open path assigns a `group_id`, so an `ssh`
/// the user types by hand — in a tab that belongs to no entry — stays outside
/// every group and lists under "Other tabs".
#[test]
fn test_tab_opened_outside_the_entry_path_stays_ungrouped() {
    let _repo_mode_guard = FeatureFlag::RepoMode.override_enabled(true);
    let _grouped_tabs_guard = FeatureFlag::GroupedTabs.override_enabled(true);
    let key = format_remote_key("10.0.0.7", 22, "vinh", "", "/srv/app");
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        register_projects_model(&mut app, Vec::new());
        let workspace = mock_workspace(&mut app);
        workspace.update(&mut app, |workspace, ctx| {
            ProjectManagementModel::handle(ctx).update(ctx, |projects, ctx| {
                projects.upsert_project(PathBuf::from(&key), ctx);
            });
            // No entry selected: a plain new tab is nobody's member. Where that
            // tab later ssh's to is irrelevant — grouping is decided here, not
            // by the command the user runs.
            workspace.select_repo_mode_all(ctx);
            workspace.add_terminal_tab(false, ctx);

            let index = workspace.active_tab_index;
            assert_eq!(workspace.tabs[index].group_id, None);

            let entry_paths = vec![PathBuf::from(&key)];
            let (by_entry, loose) = workspace.repo_mode_tab_partition(&entry_paths);
            assert!(loose.contains(&index));
            assert!(
                by_entry.values().all(|members| !members.contains(&index)),
                "an ungrouped tab must not be filed under any entry"
            );
        });
    });
}

/// R7: the probe's `ssh` must be findable when Warp was launched outside a
/// login shell. The interactive `PATH` wins, and the one inherited value that
/// cannot resolve anything — an empty `PATH` — is replaced rather than passed
/// through, which is what turned a healthy host into "Unreachable".
#[cfg(not(target_family = "wasm"))]
#[test]
fn probe_path_env_prefers_the_shell_path_and_replaces_an_empty_one() {
    use std::ffi::OsString;

    assert_eq!(
        probe_path_env(
            Some("/opt/homebrew/bin".to_string()),
            Some(OsString::from(""))
        ),
        Some("/opt/homebrew/bin".to_string()),
        "the interactive PATH is what the user's ssh lives on"
    );
    // Nothing to improve on: inherit, so the child sees the process PATH.
    assert_eq!(probe_path_env(None, Some(OsString::from("/usr/bin"))), None);
    assert_eq!(probe_path_env(Some(String::new()), None), None);

    let empty_path = probe_path_env(None, Some(OsString::from("")));
    if cfg!(unix) {
        assert_eq!(
            empty_path.as_deref(),
            Some("/usr/bin:/bin:/usr/sbin:/sbin"),
            "an empty PATH resolves nothing, so it must not be inherited"
        );
    } else {
        assert_eq!(empty_path, None);
    }
}

fn probe_target(path: &str) -> RemoteTarget {
    RemoteTarget {
        server: "10.0.0.7".to_string(),
        port: 22,
        user: "vinh".to_string(),
        identity: "/k".to_string(),
        remote_path: path.to_string(),
    }
}

/// NA4 (N7/N9): the user submits a connection and closes the form while the
/// probe is still running. When the probe later succeeds it must register
/// nothing and close nothing — they walked away from that connection.
///
/// The old path wrote the row to the registry on *submit*, so this sequence
/// left a permanent entry for a host the user cancelled out of, and the late
/// success then closed whatever form happened to be open.
#[test]
fn test_a_probe_that_lands_after_the_form_closed_registers_nothing() {
    let _repo_mode_guard = FeatureFlag::RepoMode.override_enabled(true);
    let _grouped_tabs_guard = FeatureFlag::GroupedTabs.override_enabled(true);
    let target = probe_target("/srv/app");
    let key = target.key();
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        register_projects_model(&mut app, Vec::new());
        let workspace = mock_workspace(&mut app);
        workspace.update(&mut app, |workspace, ctx| {
            let generation = workspace.restart_remote_probe(&key);
            workspace.repo_mode_pending_remote_key = Some(key.clone());
            assert_eq!(workspace.repo_mode_entries(ctx).len(), 1);

            // Escape / the close button.
            workspace.close_remote_connection_modal(ctx);
            assert!(workspace.repo_mode_entries(ctx).is_empty());

            workspace.apply_remote_probe_result(
                &target,
                &key,
                generation,
                Some(1),
                Ok(RemoteProbeOutcome::Found {
                    remote_path: "/srv/app".to_string(),
                    kind: RepoEntryKind::Repo,
                    branch: Some("main".to_string()),
                }),
                ctx,
            );

            assert!(
                registered_paths(ctx).is_empty(),
                "a connection the user cancelled must not be persisted"
            );
            assert!(workspace.repo_mode_entries(ctx).is_empty());
            assert!(
                !workspace
                    .repo_mode_remote_probes
                    .borrow()
                    .contains_key(&key)
            );
        });
    });
}

/// NA5 (N7/N10): re-adding a connection the user already has, while the host is
/// down, must not damage the entry they already have. The failed add drops its
/// own pending row and leaves the existing entry, its group, and its tabs alone.
#[test]
fn test_a_failed_re_add_leaves_the_existing_entry_intact() {
    let _repo_mode_guard = FeatureFlag::RepoMode.override_enabled(true);
    let _grouped_tabs_guard = FeatureFlag::GroupedTabs.override_enabled(true);
    let target = probe_target("/srv/app");
    let key = target.key();
    let now = Utc::now().naive_utc();
    let projects = vec![Project {
        path: key.clone(),
        added_ts: now,
        last_opened_ts: Some(now),
        manual_position: None,
    }];
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        register_projects_model(&mut app, projects);
        let workspace = mock_workspace(&mut app);
        workspace.update(&mut app, |workspace, ctx| {
            workspace.select_repo_mode_entry(Path::new(&key), ctx);
            let tabs_before = workspace.tabs.len();
            let group_before = workspace
                .tab_groups
                .values()
                .find(|group| group.repo_root.as_deref() == Some(key.as_str()))
                .map(|group| group.id)
                .expect("selecting the entry binds a group to it");

            // The user re-adds the identical connection, and it fails.
            let generation = workspace.restart_remote_probe(&key);
            workspace.repo_mode_pending_remote_key = Some(key.clone());
            workspace.apply_remote_probe_result(
                &target,
                &key,
                generation,
                Some(1),
                Err(repo_mode::RemoteProbeFailure::Unreachable),
                ctx,
            );

            assert_eq!(
                registered_paths(ctx),
                vec![key.clone()],
                "the entry the user already had must survive a failed re-add"
            );
            assert_eq!(workspace.tabs.len(), tabs_before);
            assert!(
                workspace.tab_groups.contains_key(&group_before),
                "the bound group must survive too"
            );
        });
    });
}

/// NA6 (N8): selecting the same unreachable row repeatedly must not stack up
/// `ssh` subprocesses. A probe is only started when none is in flight for that
/// key, and the row ends up showing the newest result.
#[test]
fn test_repeated_selection_runs_one_probe_at_a_time() {
    let _repo_mode_guard = FeatureFlag::RepoMode.override_enabled(true);
    let _grouped_tabs_guard = FeatureFlag::GroupedTabs.override_enabled(true);
    let target = probe_target("/srv/app");
    let key = target.key();
    let now = Utc::now().naive_utc();
    let projects = vec![Project {
        path: key.clone(),
        added_ts: now,
        last_opened_ts: Some(now),
        manual_position: None,
    }];
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        register_projects_model(&mut app, projects);
        let workspace = mock_workspace(&mut app);
        workspace.update(&mut app, |workspace, ctx| {
            let first = workspace
                .begin_remote_probe(&key)
                .expect("the first click starts a probe");
            assert_eq!(
                workspace.begin_remote_probe(&key),
                None,
                "a second click while the first probe runs must not spawn another ssh"
            );
            assert_eq!(workspace.begin_remote_probe(&key), None);

            workspace.apply_remote_probe_result(
                &target,
                &key,
                first,
                None,
                Err(repo_mode::RemoteProbeFailure::Unreachable),
                ctx,
            );

            // With the probe landed, the next click is free to start a new one,
            // and its result replaces the previous one.
            let second = workspace
                .begin_remote_probe(&key)
                .expect("a landed probe releases the key");
            assert_ne!(first, second);
            workspace.apply_remote_probe_result(
                &target,
                &key,
                second,
                None,
                Ok(RemoteProbeOutcome::Found {
                    remote_path: "/srv/app".to_string(),
                    kind: RepoEntryKind::Repo,
                    branch: Some("main".to_string()),
                }),
                ctx,
            );

            let entries = workspace.repo_mode_entries(ctx);
            assert_eq!(
                entries[0].remote.as_ref().expect("remote detail").probe,
                RemoteProbeState::Resolved {
                    kind: RepoEntryKind::Repo,
                    branch: Some("main".to_string()),
                }
            );
        });
    });
}

/// Two probes for one key resolving out of order leave the newer result. The
/// older probe's callback carries a generation the key no longer recognises, so
/// it is dropped rather than overwriting the answer that superseded it.
#[test]
fn test_an_out_of_order_probe_result_does_not_overwrite_the_newer_one() {
    let _repo_mode_guard = FeatureFlag::RepoMode.override_enabled(true);
    let _grouped_tabs_guard = FeatureFlag::GroupedTabs.override_enabled(true);
    let target = probe_target("/srv/app");
    let key = target.key();
    let now = Utc::now().naive_utc();
    let projects = vec![Project {
        path: key.clone(),
        added_ts: now,
        last_opened_ts: Some(now),
        manual_position: None,
    }];
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        register_projects_model(&mut app, projects);
        let workspace = mock_workspace(&mut app);
        workspace.update(&mut app, |workspace, ctx| {
            let stale = workspace.begin_remote_probe(&key).expect("first probe");
            // The user resubmits, orphaning the first probe.
            let fresh = workspace.restart_remote_probe(&key);

            workspace.apply_remote_probe_result(
                &target,
                &key,
                fresh,
                None,
                Err(repo_mode::RemoteProbeFailure::PathNotFound),
                ctx,
            );
            workspace.apply_remote_probe_result(
                &target,
                &key,
                stale,
                None,
                Ok(RemoteProbeOutcome::Found {
                    remote_path: "/srv/app".to_string(),
                    kind: RepoEntryKind::Repo,
                    branch: None,
                }),
                ctx,
            );

            let entries = workspace.repo_mode_entries(ctx);
            assert_eq!(
                entries[0].remote.as_ref().expect("remote detail").probe,
                RemoteProbeState::Failed {
                    reason: repo_mode::RemoteProbeFailure::PathNotFound
                },
                "the late result of an orphaned probe must not replace the newer answer"
            );
        });
    });
}

/// A success for a key the user removed while the probe was running registers
/// nothing: the entry is gone, and re-registering it would resurrect a row they
/// deleted.
#[test]
fn test_a_probe_success_for_a_removed_key_registers_nothing() {
    let _repo_mode_guard = FeatureFlag::RepoMode.override_enabled(true);
    let _grouped_tabs_guard = FeatureFlag::GroupedTabs.override_enabled(true);
    let target = probe_target("/srv/app");
    let key = target.key();
    let now = Utc::now().naive_utc();
    let projects = vec![Project {
        path: key.clone(),
        added_ts: now,
        last_opened_ts: Some(now),
        manual_position: None,
    }];
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        register_projects_model(&mut app, projects);
        let workspace = mock_workspace(&mut app);
        workspace.update(&mut app, |workspace, ctx| {
            let generation = workspace.begin_remote_probe(&key).expect("probe starts");
            workspace.remove_repo_mode_entry(Path::new(&key), ctx);

            workspace.apply_remote_probe_result(
                &target,
                &key,
                generation,
                None,
                Ok(RemoteProbeOutcome::Found {
                    remote_path: "/srv/app".to_string(),
                    kind: RepoEntryKind::Repo,
                    branch: None,
                }),
                ctx,
            );

            assert!(registered_paths(ctx).is_empty());
            assert!(workspace.repo_mode_entries(ctx).is_empty());
        });
    });
}

/// NA13 (N7/N12): a remote key persisted by the pre-U4 path — written on submit
/// and never verified — has no probe session on this launch. It must not render
/// as a resolved entry, and it must not be treated as dead either: the user
/// typed those connection details, and a row vanishing on upgrade is worse than
/// one that says it has not connected.
#[test]
fn test_a_persisted_but_never_verified_row_renders_unresolved() {
    let _repo_mode_guard = FeatureFlag::RepoMode.override_enabled(true);
    let _grouped_tabs_guard = FeatureFlag::GroupedTabs.override_enabled(true);
    let key = probe_target("/srv/app").key();
    let now = Utc::now().naive_utc();
    let projects = vec![Project {
        path: key.clone(),
        added_ts: now,
        last_opened_ts: Some(now),
        manual_position: None,
    }];
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        register_projects_model(&mut app, projects);
        let workspace = mock_workspace(&mut app);
        workspace.update(&mut app, |workspace, ctx| {
            assert!(workspace.repo_mode_remote_probes.borrow().is_empty());

            let entries = workspace.repo_mode_entries(ctx);
            assert_eq!(entries.len(), 1);
            let remote = entries[0].remote.as_ref().expect("remote detail");
            assert_eq!(remote.probe, RemoteProbeState::Pending);
            assert_eq!(remote.probe.kind(), None, "it has resolved to nothing");
            assert!(!entries[0].is_dead, "and it is not the dead-path state");

            // Touching it is what refreshes it (R11), and that is a probe the
            // key did not previously have.
            assert!(workspace.begin_remote_probe(&key).is_some());
        });
    });
}

/// A success for the host the user *was* adding must not close the form they
/// have since reopened for a different host — nor register the abandoned one.
///
/// Submitting again swaps the pending row, which orphans the first probe, so
/// its late result has no session to land on.
#[test]
fn test_a_late_success_does_not_close_a_form_reopened_for_another_host() {
    let _repo_mode_guard = FeatureFlag::RepoMode.override_enabled(true);
    let _grouped_tabs_guard = FeatureFlag::GroupedTabs.override_enabled(true);
    let abandoned = probe_target("/srv/first");
    let abandoned_key = abandoned.key();
    let current = probe_target("/srv/second");
    let current_key = current.key();
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        register_projects_model(&mut app, Vec::new());
        let workspace = mock_workspace(&mut app);
        workspace.update(&mut app, |workspace, ctx| {
            let stale = workspace.restart_remote_probe(&abandoned_key);
            workspace.repo_mode_pending_remote_key = Some(abandoned_key.clone());

            // The user goes back and submits a different connection.
            workspace.add_remote_repo_mode_entry(
                2,
                current.server.clone(),
                current.port,
                current.user.clone(),
                current.identity.clone(),
                current.remote_path.clone(),
                ctx,
            );
            assert_eq!(
                workspace.repo_mode_pending_remote_key.as_deref(),
                Some(current_key.as_str())
            );

            workspace.apply_remote_probe_result(
                &abandoned,
                &abandoned_key,
                stale,
                Some(1),
                Ok(RemoteProbeOutcome::Found {
                    remote_path: "/srv/first".to_string(),
                    kind: RepoEntryKind::Repo,
                    branch: None,
                }),
                ctx,
            );

            assert!(
                registered_paths(ctx).is_empty(),
                "the abandoned host must not be registered"
            );
            assert_eq!(
                workspace.repo_mode_pending_remote_key.as_deref(),
                Some(current_key.as_str()),
                "the form the user is actually looking at must still be probing"
            );
            let keys: Vec<String> = workspace
                .repo_mode_remote_probes
                .borrow()
                .keys()
                .cloned()
                .collect();
            assert_eq!(keys, vec![current_key.clone()]);
        });
    });
}

/// Section identity per KTD2: a tab whose group is bound to a *registered*
/// repository reports that root; a tab in a user-created group carries no
/// `repo_root` and so is loose, exactly as `repo_mode_tab_partition` files it.
#[test]
fn test_section_accessor_reads_the_bound_repo_root() {
    let _repo_mode_guard = FeatureFlag::RepoMode.override_enabled(true);
    let _grouped_tabs_guard = FeatureFlag::GroupedTabs.override_enabled(true);
    let projects = one_repo_projects();
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        register_projects_model(&mut app, projects);
        let workspace = mock_workspace(&mut app);
        workspace.update(&mut app, |workspace, ctx| {
            // Tabs: [loose, repo-bound, user-grouped]
            workspace.add_terminal_tab(false, ctx);
            workspace.add_terminal_tab(false, ctx);

            let mut bound = TabGroup::new();
            bound.repo_root = Some("/repo/a".to_string());
            let bound_id = bound.id;
            workspace.tab_groups.insert(bound_id, bound);
            let user_group = TabGroup::new();
            let user_group_id = user_group.id;
            workspace.tab_groups.insert(user_group_id, user_group);

            workspace.tabs[1].group_id = Some(bound_id);
            workspace.tabs[2].group_id = Some(user_group_id);

            let entry_paths = workspace.repo_mode_entry_paths(ctx);
            assert_eq!(
                workspace.repo_mode_group_section(bound_id, &entry_paths),
                Some(PathBuf::from("/repo/a"))
            );
            assert_eq!(
                workspace.repo_mode_tab_section(1, &entry_paths),
                Some(PathBuf::from("/repo/a"))
            );
            assert_eq!(
                workspace.repo_mode_group_section(user_group_id, &entry_paths),
                None,
                "a user-created group is not a repository section"
            );
            assert_eq!(workspace.repo_mode_tab_section(2, &entry_paths), None);
            assert_eq!(
                workspace.repo_mode_tab_section(0, &entry_paths),
                None,
                "and a tab with no group at all is loose"
            );
        });
    });
}

/// With no group carrying a `repo_root`, every tab is loose — nothing in the
/// window is repo-bound, so drag has nothing to clamp.
#[test]
fn test_section_accessor_is_loose_when_no_group_is_repo_bound() {
    let _repo_mode_guard = FeatureFlag::RepoMode.override_enabled(true);
    let _grouped_tabs_guard = FeatureFlag::GroupedTabs.override_enabled(true);
    let projects = one_repo_projects();
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        register_projects_model(&mut app, projects);
        let workspace = mock_workspace(&mut app);
        workspace.update(&mut app, |workspace, ctx| {
            workspace.add_terminal_tab(false, ctx);

            let user_group = TabGroup::new();
            let user_group_id = user_group.id;
            workspace.tab_groups.insert(user_group_id, user_group);
            workspace.tabs[1].group_id = Some(user_group_id);

            let entry_paths = workspace.repo_mode_entry_paths(ctx);
            for index in 0..workspace.tabs.len() {
                assert_eq!(workspace.repo_mode_tab_section(index, &entry_paths), None);
            }
        });
    });
}

/// A bound root that has left the registry (removed in another window) reads
/// loose — the same ownership test `repo_mode_tab_partition` applies, so the
/// section a drag sees always matches the section that renders.
#[test]
fn test_section_accessor_is_loose_for_an_unregistered_bound_root() {
    let _repo_mode_guard = FeatureFlag::RepoMode.override_enabled(true);
    let _grouped_tabs_guard = FeatureFlag::GroupedTabs.override_enabled(true);
    let projects = one_repo_projects();
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        register_projects_model(&mut app, projects);
        let workspace = mock_workspace(&mut app);
        workspace.update(&mut app, |workspace, ctx| {
            let group_ids = bind_tabs(workspace, &[None, Some("/repo/gone")], ctx);
            let stale_id = group_ids["/repo/gone"];

            let entry_paths = workspace.repo_mode_entry_paths(ctx);
            assert!(
                entry_paths.contains(&PathBuf::from("/repo/a")),
                "the registry is non-empty; the bound root just is not in it"
            );
            assert_eq!(
                workspace.repo_mode_group_section(stale_id, &entry_paths),
                None
            );
            assert_eq!(workspace.repo_mode_tab_section(1, &entry_paths), None);

            // And it matches where the display partition puts that tab.
            let (_, loose) = workspace.repo_mode_tab_partition(&entry_paths);
            assert!(loose.contains(&1));
        });
    });
}

/// Covers R7. With repo mode off, a `repo_root` copied in by session restore
/// (which is not flag-gated) must not create a section — no build without repo
/// mode may start clamping drags.
#[test]
fn test_section_accessor_is_loose_with_repo_mode_off() {
    let _repo_mode_guard = FeatureFlag::RepoMode.override_enabled(false);
    let _grouped_tabs_guard = FeatureFlag::GroupedTabs.override_enabled(true);
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        register_projects_model(&mut app, Vec::new());
        let workspace = mock_workspace(&mut app);
        workspace.update(&mut app, |workspace, ctx| {
            let group_ids = bind_tabs(workspace, &[None, Some("/repo/a")], ctx);
            let restored_id = group_ids["/repo/a"];

            // Hand it a registry that *does* contain the root, so the flag gate
            // is the only thing that can make this loose.
            let entry_paths = vec![PathBuf::from("/repo/a")];
            assert_eq!(
                workspace.repo_mode_group_section(restored_id, &entry_paths),
                None
            );
            assert_eq!(workspace.repo_mode_tab_section(1, &entry_paths), None);
        });
    });
}

/// The second gate of KTD2: grouped tabs off means there are no sections at
/// all, so a restored `repo_root` cannot clamp anything in that build either.
#[test]
fn test_section_accessor_is_loose_with_grouped_tabs_off() {
    let _repo_mode_guard = FeatureFlag::RepoMode.override_enabled(true);
    let _grouped_tabs_guard = FeatureFlag::GroupedTabs.override_enabled(false);
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        register_projects_model(&mut app, Vec::new());
        let workspace = mock_workspace(&mut app);
        workspace.update(&mut app, |workspace, ctx| {
            let group_ids = bind_tabs(workspace, &[None, Some("/repo/a")], ctx);
            let bound_id = group_ids["/repo/a"];

            let entry_paths = vec![PathBuf::from("/repo/a")];
            assert_eq!(
                workspace.repo_mode_group_section(bound_id, &entry_paths),
                None
            );
            assert_eq!(workspace.repo_mode_tab_section(1, &entry_paths), None);
        });
    });
}

/// Covers AE1 / R1. A repo-bound tab dragged where no group resolves (the
/// flattened repo strip registers no rectangle) must not be unbound: the guard
/// fires on the source side, the call site skips reassignment, and the drag
/// falls through to the neighbour swap with the binding intact.
#[test]
fn test_repo_bound_source_blocks_reassignment_when_no_target_resolves() {
    let _repo_mode_guard = FeatureFlag::RepoMode.override_enabled(true);
    let _grouped_tabs_guard = FeatureFlag::GroupedTabs.override_enabled(true);
    let projects = one_repo_projects();
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        register_projects_model(&mut app, projects);
        let workspace = mock_workspace(&mut app);
        workspace.update(&mut app, |workspace, ctx| {
            let group_ids = bind_tabs(workspace, &[None, Some("/repo/a"), Some("/repo/a")], ctx);
            let bound_id = group_ids["/repo/a"];

            let entry_paths = workspace.repo_mode_entry_paths(ctx);
            assert!(
                workspace.repo_bound_drag_blocks_reassignment(1, None, &entry_paths),
                "a repo-bound tab is never unbound by a drag that resolves no group"
            );
            // Nothing about the guard mutates membership; the call site simply
            // never reaches `assign_tab_to_group`.
            assert_eq!(workspace.tabs[1].group_id, Some(bound_id));

            // Reordering inside its own repo group is the same skip: the source
            // is bound, so the drag falls through to the neighbour swap.
            assert!(workspace.repo_bound_drag_blocks_reassignment(1, Some(bound_id), &entry_paths));
        });
    });
}

/// Covers AE3 / AE7 / R4 / R9. A loose tab dragged over a repo-bound group is
/// clamped to "Other tabs": drag never *creates* a repository binding (KD3),
/// including on the horizontal bar where that group does render a container.
#[test]
fn test_repo_bound_target_blocks_a_loose_tab_from_joining() {
    let _repo_mode_guard = FeatureFlag::RepoMode.override_enabled(true);
    let _grouped_tabs_guard = FeatureFlag::GroupedTabs.override_enabled(true);
    let projects = one_repo_projects();
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        register_projects_model(&mut app, projects);
        let workspace = mock_workspace(&mut app);
        workspace.update(&mut app, |workspace, ctx| {
            let group_ids = bind_tabs(workspace, &[None, Some("/repo/a")], ctx);
            let bound_id = group_ids["/repo/a"];

            let entry_paths = workspace.repo_mode_entry_paths(ctx);
            assert_eq!(
                workspace.repo_mode_tab_section(0, &entry_paths),
                None,
                "tab 0 is the loose one being dragged"
            );
            assert!(
                workspace.repo_bound_drag_blocks_reassignment(0, Some(bound_id), &entry_paths),
                "a loose tab must not acquire a repository binding by drag"
            );
            assert_eq!(workspace.tabs[0].group_id, None);
        });
    });
}

/// The guard is scoped to repo-bound sections only: inside "Other tabs", a
/// loose tab dragged into a user-created group still joins it, exactly as
/// before this change.
#[test]
fn test_a_user_created_group_still_accepts_a_dragged_loose_tab() {
    let _repo_mode_guard = FeatureFlag::RepoMode.override_enabled(true);
    let _grouped_tabs_guard = FeatureFlag::GroupedTabs.override_enabled(true);
    let projects = one_repo_projects();
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        register_projects_model(&mut app, projects);
        let workspace = mock_workspace(&mut app);
        workspace.update(&mut app, |workspace, ctx| {
            workspace.add_terminal_tab(false, ctx);

            let user_group = TabGroup::new();
            let user_group_id = user_group.id;
            workspace.tab_groups.insert(user_group_id, user_group);
            workspace.tabs[1].group_id = Some(user_group_id);

            let entry_paths = workspace.repo_mode_entry_paths(ctx);
            assert!(!workspace.repo_bound_drag_blocks_reassignment(
                0,
                Some(user_group_id),
                &entry_paths
            ));
            // And dragging back out of it is equally unguarded.
            assert!(!workspace.repo_bound_drag_blocks_reassignment(1, None, &entry_paths));
        });
    });
}

/// Covers R7. In a repo-mode-off build restored from a repo-mode session, the
/// group still carries a `repo_root` (session restore copies it unconditionally,
/// unlike `pinned`). The guard must stay quiet, so a tab dragged into that group
/// joins it exactly as it does today.
#[test]
fn test_drag_reassignment_is_unguarded_with_repo_mode_off() {
    let _repo_mode_guard = FeatureFlag::RepoMode.override_enabled(false);
    let _grouped_tabs_guard = FeatureFlag::GroupedTabs.override_enabled(true);
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        register_projects_model(&mut app, Vec::new());
        let workspace = mock_workspace(&mut app);
        workspace.update(&mut app, |workspace, ctx| {
            let group_ids = bind_tabs(workspace, &[None, Some("/repo/a")], ctx);
            let restored_id = group_ids["/repo/a"];

            // Even handed a registry containing that root, both sides read loose.
            let entry_paths = vec![PathBuf::from("/repo/a")];
            assert!(!workspace.repo_bound_drag_blocks_reassignment(
                0,
                Some(restored_id),
                &entry_paths
            ));
            assert!(!workspace.repo_bound_drag_blocks_reassignment(1, None, &entry_paths));
        });
    });
}

/// Hands back the two-tab-minimum fixture the section tests share: `roots[i]`
/// puts tab `i` in a group bound to that repository root, `None` leaves it
/// loose. Tab 0 already exists on a fresh workspace, so only the rest are added.
/// Whether a root is *registered* is the caller's business — a test can bind to
/// one the registry never had.
///
/// Returns the group created per root, for tests that need to name one.
fn bind_tabs(
    workspace: &mut Workspace,
    roots: &[Option<&str>],
    ctx: &mut ViewContext<Workspace>,
) -> HashMap<String, TabGroupId> {
    while workspace.tabs.len() < roots.len() {
        workspace.add_terminal_tab(false, ctx);
    }
    let mut group_ids: HashMap<String, TabGroupId> = HashMap::new();
    for (index, root) in roots.iter().enumerate() {
        let Some(root) = root else { continue };
        let group_id = *group_ids.entry((*root).to_string()).or_insert_with(|| {
            let mut group = TabGroup::new();
            group.repo_root = Some((*root).to_string());
            let id = group.id;
            workspace.tab_groups.insert(id, group);
            id
        });
        workspace.tabs[index].group_id = Some(group_id);
    }
    group_ids
}

/// Every tab group still occupies one contiguous run of tab indices (R5).
fn assert_groups_contiguous(workspace: &Workspace) {
    let group_ids: Vec<Option<TabGroupId>> =
        workspace.tabs.iter().map(|tab| tab.group_id).collect();
    let broken = non_contiguous_groups(&group_ids);
    assert!(broken.is_empty(), "non-contiguous groups: {broken:?}");
}

fn one_repo_projects() -> Vec<Project> {
    let now = Utc::now().naive_utc();
    vec![Project {
        path: "/repo/a".to_string(),
        added_ts: now,
        last_opened_ts: Some(now),
        manual_position: None,
    }]
}

/// Covers AE1. Inside one repository's run, the neighbour on each axis is that
/// repository's adjacent tab — an interior drag reorders exactly as before.
#[test]
fn test_section_neighbor_moves_within_a_repository_run() {
    let _repo_mode_guard = FeatureFlag::RepoMode.override_enabled(true);
    let _grouped_tabs_guard = FeatureFlag::GroupedTabs.override_enabled(true);
    let projects = one_repo_projects();
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        register_projects_model(&mut app, projects);
        let workspace = mock_workspace(&mut app);
        workspace.update(&mut app, |workspace, ctx| {
            // [A1, A2, A3], all bound to /repo/a.
            bind_tabs(
                workspace,
                &[Some("/repo/a"), Some("/repo/a"), Some("/repo/a")],
                ctx,
            );
            let entry_paths = workspace.repo_mode_entry_paths(ctx);

            assert_eq!(
                workspace.section_neighbor(2, false, &entry_paths),
                Some(1),
                "the neighbour above A3 is A2"
            );
            assert_eq!(
                workspace.section_neighbor(0, true, &entry_paths),
                Some(1),
                "the neighbour below A1 is A2"
            );
        });
    });
}

/// Covers AE2 / R2 / R3. At each end of the repository's run there is no
/// same-section tab left in that direction, and the absent neighbour is the
/// clamp: the drag stops at the strip's edge instead of leaving it.
#[test]
fn test_section_neighbor_clamps_at_the_repository_strip_edges() {
    let _repo_mode_guard = FeatureFlag::RepoMode.override_enabled(true);
    let _grouped_tabs_guard = FeatureFlag::GroupedTabs.override_enabled(true);
    let projects = one_repo_projects();
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        register_projects_model(&mut app, projects);
        let workspace = mock_workspace(&mut app);
        workspace.update(&mut app, |workspace, ctx| {
            // [L0, A1, A2, A3, L4]. The repository's run is bounded by loose
            // tabs rather than by the list ends, so its edges are clamped by
            // the *section*, not by the bounds guard that was always there.
            bind_tabs(
                workspace,
                &[
                    None,
                    Some("/repo/a"),
                    Some("/repo/a"),
                    Some("/repo/a"),
                    None,
                ],
                ctx,
            );
            let entry_paths = workspace.repo_mode_entry_paths(ctx);

            assert_eq!(
                workspace.section_neighbor(1, false, &entry_paths),
                None,
                "nothing above A1 inside the repository, though a loose tab sits there"
            );
            assert_eq!(
                workspace.section_neighbor(3, true, &entry_paths),
                None,
                "nothing below A3 inside the repository, though a loose tab sits there"
            );
            // And the list ends still clamp, as they always did.
            assert_eq!(workspace.section_neighbor(0, false, &entry_paths), None);
            assert_eq!(workspace.section_neighbor(4, true, &entry_paths), None);
        });
    });
}

/// Covers AE3 / KTD3. The search *skips* foreign-section tabs rather than
/// stopping at them: two loose tabs separated by a repository tab in flat order
/// still reorder against each other, which stopping at the first foreign tab
/// would have frozen.
#[test]
fn test_section_neighbor_skips_a_foreign_tab_between_two_loose_ones() {
    let _repo_mode_guard = FeatureFlag::RepoMode.override_enabled(true);
    let _grouped_tabs_guard = FeatureFlag::GroupedTabs.override_enabled(true);
    let projects = one_repo_projects();
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        register_projects_model(&mut app, projects);
        let workspace = mock_workspace(&mut app);
        workspace.update(&mut app, |workspace, ctx| {
            // [L0, A1, L2]
            bind_tabs(workspace, &[None, Some("/repo/a"), None], ctx);
            let entry_paths = workspace.repo_mode_entry_paths(ctx);

            assert_eq!(
                workspace.section_neighbor(0, true, &entry_paths),
                Some(2),
                "L0's neighbour below is L2, past the repository tab between them"
            );
            assert_eq!(
                workspace.section_neighbor(2, false, &entry_paths),
                Some(0),
                "and symmetrically upward"
            );
        });
    });
}

/// Covers AE4. A repository whose run is a single tab has no same-section
/// neighbour in either direction, so that tab cannot be dragged anywhere.
#[test]
fn test_section_neighbor_is_absent_for_a_lone_repository_tab() {
    let _repo_mode_guard = FeatureFlag::RepoMode.override_enabled(true);
    let _grouped_tabs_guard = FeatureFlag::GroupedTabs.override_enabled(true);
    let projects = one_repo_projects();
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        register_projects_model(&mut app, projects);
        let workspace = mock_workspace(&mut app);
        workspace.update(&mut app, |workspace, ctx| {
            bind_tabs(workspace, &[None, Some("/repo/a"), None], ctx);
            let entry_paths = workspace.repo_mode_entry_paths(ctx);

            assert_eq!(workspace.section_neighbor(1, false, &entry_paths), None);
            assert_eq!(workspace.section_neighbor(1, true, &entry_paths), None);
        });
    });
}

/// Covers R5. Only same-section tabs ever swap, so every swap the neighbour
/// search authorises leaves each repository's tabs one contiguous run —
/// including a loose swap that reaches *across* a repository's run.
#[test]
fn test_section_scoped_swaps_keep_every_group_contiguous() {
    let _repo_mode_guard = FeatureFlag::RepoMode.override_enabled(true);
    let _grouped_tabs_guard = FeatureFlag::GroupedTabs.override_enabled(true);
    let projects = one_repo_projects();
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        register_projects_model(&mut app, projects);
        let workspace = mock_workspace(&mut app);
        workspace.update(&mut app, |workspace, ctx| {
            // [L0, A1, A2, L3]
            bind_tabs(
                workspace,
                &[None, Some("/repo/a"), Some("/repo/a"), None],
                ctx,
            );
            let entry_paths = workspace.repo_mode_entry_paths(ctx);
            assert_groups_contiguous(workspace);

            // The loose tab at 0 reaches its next loose tab across the run.
            let across = workspace
                .section_neighbor(0, true, &entry_paths)
                .expect("L0 has a loose neighbour below");
            assert_eq!(across, 3);
            workspace.tabs.swap(0, across);
            assert_groups_contiguous(workspace);

            // And the repository's own interior swap keeps it contiguous too.
            let inside = workspace
                .section_neighbor(1, true, &entry_paths)
                .expect("A1 has a same-repository neighbour below");
            assert_eq!(inside, 2);
            workspace.tabs.swap(1, inside);
            assert_groups_contiguous(workspace);
        });
    });
}

/// Covers R7. With nothing repo-bound in the window every tab is loose, so the
/// neighbour is always the plain adjacent index — the pre-change result, by
/// construction rather than by a branch.
#[test]
fn test_section_neighbor_is_the_adjacent_index_with_no_repo_bound_group() {
    let _repo_mode_guard = FeatureFlag::RepoMode.override_enabled(true);
    let _grouped_tabs_guard = FeatureFlag::GroupedTabs.override_enabled(true);
    let projects = one_repo_projects();
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        register_projects_model(&mut app, projects);
        let workspace = mock_workspace(&mut app);
        workspace.update(&mut app, |workspace, ctx| {
            bind_tabs(workspace, &[None, None, None, None], ctx);
            // One user-created group, which is not a repository section.
            let user_group = TabGroup::new();
            let user_group_id = user_group.id;
            workspace.tab_groups.insert(user_group_id, user_group);
            workspace.tabs[1].group_id = Some(user_group_id);

            let entry_paths = workspace.repo_mode_entry_paths(ctx);
            assert_adjacent_neighbors_everywhere(workspace, &entry_paths);
        });
    });
}

/// Covers R7. Session restore copies `repo_root` unconditionally, so a repo
/// mode *off* build can be holding one. It must not clamp: every neighbour is
/// still the adjacent index.
#[test]
fn test_section_neighbor_is_the_adjacent_index_with_repo_mode_off() {
    let _repo_mode_guard = FeatureFlag::RepoMode.override_enabled(false);
    let _grouped_tabs_guard = FeatureFlag::GroupedTabs.override_enabled(true);
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        register_projects_model(&mut app, Vec::new());
        let workspace = mock_workspace(&mut app);
        workspace.update(&mut app, |workspace, ctx| {
            bind_tabs(workspace, &[None, Some("/repo/a"), None, None], ctx);
            // Hand it a registry that *does* contain the root, so the flag gate
            // is the only thing that can keep this unclamped.
            let entry_paths = vec![PathBuf::from("/repo/a")];
            assert_adjacent_neighbors_everywhere(workspace, &entry_paths);
        });
    });
}

/// Covers R7. The neighbour search runs *outside* the drag path's
/// `groups_enabled` branch, so the accessor's second flag gate is the only
/// thing protecting a grouped-tabs-off build from a restored `repo_root`.
#[test]
fn test_section_neighbor_is_the_adjacent_index_with_grouped_tabs_off() {
    let _repo_mode_guard = FeatureFlag::RepoMode.override_enabled(true);
    let _grouped_tabs_guard = FeatureFlag::GroupedTabs.override_enabled(false);
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        register_projects_model(&mut app, Vec::new());
        let workspace = mock_workspace(&mut app);
        workspace.update(&mut app, |workspace, ctx| {
            bind_tabs(workspace, &[None, Some("/repo/a"), None, None], ctx);
            let entry_paths = vec![PathBuf::from("/repo/a")];
            assert_adjacent_neighbors_everywhere(workspace, &entry_paths);
        });
    });
}

/// Every tab's neighbour is its plain adjacent index, bounded by the tab list —
/// exactly what the hardcoded `current_index ± 1` produced before this existed.
fn assert_adjacent_neighbors_everywhere(workspace: &Workspace, entry_paths: &[PathBuf]) {
    let last = workspace.tabs.len() - 1;
    for index in 0..workspace.tabs.len() {
        assert_eq!(
            workspace.section_neighbor(index, false, entry_paths),
            index.checked_sub(1),
            "tab {index} should see its adjacent predecessor"
        );
        assert_eq!(
            workspace.section_neighbor(index, true, entry_paths),
            (index < last).then_some(index + 1),
            "tab {index} should see its adjacent successor"
        );
    }
}

/// Covers R8 / AE6. On the horizontal tab bar a repo-bound tab is reachable
/// past a loose tab in flat order, and must still refuse to swap with it: the
/// search resolves no neighbour in that direction, so no swap can fire.
#[test]
fn test_a_repo_bound_tab_resolves_no_neighbor_past_a_loose_tab() {
    let _repo_mode_guard = FeatureFlag::RepoMode.override_enabled(true);
    let _grouped_tabs_guard = FeatureFlag::GroupedTabs.override_enabled(true);
    let projects = one_repo_projects();
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        register_projects_model(&mut app, projects);
        let workspace = mock_workspace(&mut app);
        workspace.update(&mut app, |workspace, ctx| {
            // [L0, A1, A2]
            bind_tabs(workspace, &[None, Some("/repo/a"), Some("/repo/a")], ctx);
            let entry_paths = workspace.repo_mode_entry_paths(ctx);

            assert_eq!(
                workspace.section_neighbor(1, false, &entry_paths),
                None,
                "A1 cannot swap up past the loose tab beside it"
            );
            assert_eq!(
                workspace.section_neighbor(1, true, &entry_paths),
                Some(2),
                "but it still reorders inside its own repository"
            );
            assert_eq!(
                workspace.section_neighbor(0, true, &entry_paths),
                None,
                "and the loose tab has no loose tab below to swap with"
            );
        });
    });
}

/// The pinned clamp and the section clamp compose (KTD4): the neighbour search
/// is pure over sections and returns a pinned neighbour like any other. Refusing
/// that swap is the caller's existing `is_tab_effectively_pinned` check, which
/// runs against exactly the index resolved here.
#[test]
fn test_section_neighbor_leaves_the_pinned_refusal_to_the_caller() {
    let _repo_mode_guard = FeatureFlag::RepoMode.override_enabled(true);
    let _grouped_tabs_guard = FeatureFlag::GroupedTabs.override_enabled(true);
    let _pinned_tabs_guard = FeatureFlag::PinnedTabs.override_enabled(true);
    let projects = one_repo_projects();
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        register_projects_model(&mut app, projects);
        let workspace = mock_workspace(&mut app);
        workspace.update(&mut app, |workspace, ctx| {
            bind_tabs(workspace, &[Some("/repo/a"), Some("/repo/a")], ctx);
            workspace.tabs[0].pinned = true;

            let entry_paths = workspace.repo_mode_entry_paths(ctx);
            let neighbor = workspace.section_neighbor(1, false, &entry_paths);
            assert_eq!(
                neighbor,
                Some(0),
                "same section, so the neighbour resolves regardless of pin state"
            );

            let neighbor = neighbor.expect("resolved above");
            assert_ne!(
                workspace.is_tab_effectively_pinned(&workspace.tabs[1]),
                workspace.is_tab_effectively_pinned(&workspace.tabs[neighbor]),
                "and the caller's pinned check is what refuses this swap"
            );
        });
    });
}

/// One simulated drag step, at the seam a test can reach. The real path
/// resolves the hovered group from geometry, consults
/// `repo_bound_drag_blocks_reassignment` before touching membership, then swaps
/// with the section neighbour — so this replays those three moves in order.
/// `target_group` stands in for what the geometry lookup returned; `None` is
/// what the *flattened* repository strip hands back, since it registers no
/// rectangle, and that empty answer is the whole bug.
///
/// The pinned clamp the caller applies between the two is deliberately left
/// out: it is orthogonal to sections and is covered on its own above.
///
/// Membership goes through `assign_tab_to_group` rather than writing `group_id`
/// directly, so a drag that is *not* refused prunes the group it emptied — the
/// step that takes the "Move to group" route away.
///
/// Returns the index the dragged tab ended on. An unchanged index means the
/// section clamp refused the move.
fn drag_step(
    workspace: &mut Workspace,
    index: usize,
    forward: bool,
    target_group: Option<TabGroupId>,
    entry_paths: &[PathBuf],
    ctx: &mut ViewContext<Workspace>,
) -> usize {
    if !workspace.repo_bound_drag_blocks_reassignment(index, target_group, entry_paths) {
        workspace.assign_tab_to_group(index, target_group, ctx);
    }
    let Some(neighbor) = workspace.section_neighbor(index, forward, entry_paths) else {
        return index;
    };
    workspace.tabs.swap(neighbor, index);
    neighbor
}

/// The heading `repo_mode_tab_partition` files the tab at `index` under:
/// `Some(root)` for a repository entry, `None` for "Other tabs". Asserting
/// through the partition rather than through the decision functions is the
/// point of these tests — it is what the user actually sees.
fn partitioned_section(
    workspace: &Workspace,
    index: usize,
    entry_paths: &[PathBuf],
) -> Option<PathBuf> {
    let (by_entry, loose) = workspace.repo_mode_tab_partition(entry_paths);
    let under_entry = by_entry
        .iter()
        .find(|(_, indices)| indices.contains(&index))
        .map(|(root, _)| root.clone());
    assert_eq!(
        under_entry.is_none(),
        loose.contains(&index),
        "tab {index} must land in exactly one of the entry lists or the loose list"
    );
    under_entry
}

/// Tab indices filed under `root`, in tab order.
fn entry_indices(workspace: &Workspace, root: &str, entry_paths: &[PathBuf]) -> Vec<usize> {
    let (by_entry, _) = workspace.repo_mode_tab_partition(entry_paths);
    by_entry
        .get(&PathBuf::from(root))
        .cloned()
        .unwrap_or_default()
}

/// Identities of the tabs at `indices`, so a reorder is observable: the
/// partition returns *indices*, which an interior swap leaves alone.
fn tab_ids(workspace: &Workspace, indices: &[usize]) -> Vec<EntityId> {
    indices
        .iter()
        .map(|index| workspace.tabs[*index].pane_group.id())
        .collect()
}

/// `[L0, A1, A2, A3, L4]`: one registered repository holding three tabs, with a
/// loose tab on each side of its run. Tab 0 is the default tab a fresh
/// workspace already has.
fn repo_run_fixture(workspace: &mut Workspace, ctx: &mut ViewContext<Workspace>) {
    bind_tabs(
        workspace,
        &[
            None,
            Some("/repo/a"),
            Some("/repo/a"),
            Some("/repo/a"),
            None,
        ],
        ctx,
    );
}

/// Covers AE1 / R1 / R5. The reported gesture, end to end: dragging a tab
/// upward inside its repository's strip reorders it *within* that strip. It
/// keeps its binding, the repository still owns the same three slots, the loose
/// list is untouched, and every group is still one contiguous run.
///
/// Before the fix this drag ejected the tab into "Other tabs": the flattened
/// strip registers no rectangle, the target lookup returned `None`, and the
/// reassignment path read that as "dragged out of every group".
#[test]
fn test_dragging_inside_a_repository_reorders_within_it() {
    let _repo_mode_guard = FeatureFlag::RepoMode.override_enabled(true);
    let _grouped_tabs_guard = FeatureFlag::GroupedTabs.override_enabled(true);
    let projects = one_repo_projects();
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        register_projects_model(&mut app, projects);
        let workspace = mock_workspace(&mut app);
        workspace.update(&mut app, |workspace, ctx| {
            repo_run_fixture(workspace, ctx);
            let entry_paths = workspace.repo_mode_entry_paths(ctx);
            let repo_group = workspace.tabs[3].group_id.expect("A3 is bound");
            let [a2, a3] = tab_ids(workspace, &[2, 3])[..] else {
                unreachable!("two ids requested")
            };
            let loose_ids = tab_ids(workspace, &[0, 4]);

            // Drag A3 up. Repo mode renders the strip flattened, so the
            // geometry lookup resolves no target group.
            let landed = drag_step(workspace, 3, false, None, &entry_paths, ctx);
            assert_eq!(landed, 2, "A3 moved one slot up inside its repository");

            assert_eq!(
                partitioned_section(workspace, landed, &entry_paths),
                Some(PathBuf::from("/repo/a")),
                "and is still filed under the repository it was dragged inside"
            );
            assert_eq!(
                workspace.tabs[landed].group_id,
                Some(repo_group),
                "with the same group binding it started with"
            );
            assert_eq!(
                entry_indices(workspace, "/repo/a", &entry_paths),
                vec![1, 2, 3],
                "the repository still owns the same run of slots"
            );
            assert_eq!(
                tab_ids(workspace, &[2, 3]),
                vec![a3, a2],
                "but the two tabs inside it have swapped places"
            );
            let (_, loose) = workspace.repo_mode_tab_partition(&entry_paths);
            assert_eq!(loose, vec![0, 4], "the loose list is untouched");
            assert_eq!(tab_ids(workspace, &[0, 4]), loose_ids);
            assert_groups_contiguous(workspace);
        });
    });
}

/// Covers AE2 / R1 / R5. A drag that would carry the tab *out* of its
/// repository's run resolves no move at all — and the refusal leaves the tab
/// where it was, still under its repository, rather than unbinding it. The
/// clamp and the binding guard are the same gesture from the user's side.
#[test]
fn test_dragging_past_the_repository_edge_moves_nothing_and_keeps_the_binding() {
    let _repo_mode_guard = FeatureFlag::RepoMode.override_enabled(true);
    let _grouped_tabs_guard = FeatureFlag::GroupedTabs.override_enabled(true);
    let projects = one_repo_projects();
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        register_projects_model(&mut app, projects);
        let workspace = mock_workspace(&mut app);
        workspace.update(&mut app, |workspace, ctx| {
            repo_run_fixture(workspace, ctx);
            let entry_paths = workspace.repo_mode_entry_paths(ctx);
            let repo_group = workspace.tabs[1].group_id.expect("A1 is bound");
            let ids_before = tab_ids(workspace, &[0, 1, 2, 3, 4]);

            // Up past the top of the run, then down past the bottom of it.
            assert_eq!(
                drag_step(workspace, 1, false, None, &entry_paths, ctx),
                1,
                "A1 has no same-repository tab above it, so nothing moves"
            );
            assert_eq!(
                drag_step(workspace, 3, true, None, &entry_paths, ctx),
                3,
                "and A3 has none below it"
            );

            assert_eq!(
                tab_ids(workspace, &[0, 1, 2, 3, 4]),
                ids_before,
                "the tab order is exactly as it was"
            );
            for index in [1usize, 2, 3] {
                assert_eq!(
                    partitioned_section(workspace, index, &entry_paths),
                    Some(PathBuf::from("/repo/a")),
                    "tab {index} is still filed under its repository"
                );
                assert_eq!(workspace.tabs[index].group_id, Some(repo_group));
            }
            assert_groups_contiguous(workspace);
        });
    });
}

/// Covers AE3 / R4 / R5. A tab in "Other tabs" dragged toward — and past — a
/// repository's run never acquires that repository. It reorders against the
/// other loose tab and stays in the loose list; drag is not a route into a
/// repository binding.
#[test]
fn test_a_loose_tab_dragged_over_a_repository_stays_loose() {
    let _repo_mode_guard = FeatureFlag::RepoMode.override_enabled(true);
    let _grouped_tabs_guard = FeatureFlag::GroupedTabs.override_enabled(true);
    let projects = one_repo_projects();
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        register_projects_model(&mut app, projects);
        let workspace = mock_workspace(&mut app);
        workspace.update(&mut app, |workspace, ctx| {
            repo_run_fixture(workspace, ctx);
            let entry_paths = workspace.repo_mode_entry_paths(ctx);
            let repo_group = workspace.tabs[1].group_id.expect("A1 is bound");
            let repo_ids_before = tab_ids(workspace, &[1, 2, 3]);
            let dragged = workspace.tabs[4].pane_group.id();

            // Dragged upward over the repository's run. On the horizontal bar
            // that group *does* render a container, so hand the guard the
            // group the lookup would have found there.
            let landed = drag_step(workspace, 4, false, Some(repo_group), &entry_paths, ctx);
            assert_eq!(landed, 0, "it reordered against the other loose tab");

            assert_eq!(
                workspace.tabs[landed].pane_group.id(),
                dragged,
                "the tab that moved is the loose one"
            );
            assert_eq!(
                workspace.tabs[landed].group_id, None,
                "and it acquired no repository binding on the way"
            );
            assert_eq!(
                partitioned_section(workspace, landed, &entry_paths),
                None,
                "so it is still under \"Other tabs\""
            );
            assert_eq!(
                entry_indices(workspace, "/repo/a", &entry_paths),
                vec![1, 2, 3],
                "and the repository's own tabs are exactly where they were"
            );
            assert_eq!(tab_ids(workspace, &[1, 2, 3]), repo_ids_before);
            assert_groups_contiguous(workspace);
        });
    });
}

/// Covers AE4 / R1. The stranding case: a repository holding a *single* tab.
/// Dragged either way, the group must survive with that tab still in it —
/// "Move to group" is only offered while the repository's group still has a
/// member, so a drag that emptied it left the tab in "Other tabs" with no
/// in-product route back.
#[test]
fn test_dragging_a_repositorys_only_tab_leaves_its_group_intact() {
    let _repo_mode_guard = FeatureFlag::RepoMode.override_enabled(true);
    let _grouped_tabs_guard = FeatureFlag::GroupedTabs.override_enabled(true);
    let projects = one_repo_projects();
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        register_projects_model(&mut app, projects);
        let workspace = mock_workspace(&mut app);
        workspace.update(&mut app, |workspace, ctx| {
            // [L0, A1, L2] — the repository's whole run is one tab.
            bind_tabs(workspace, &[None, Some("/repo/a"), None], ctx);
            let entry_paths = workspace.repo_mode_entry_paths(ctx);
            let repo_group = workspace.tabs[1].group_id.expect("A1 is bound");
            let dragged = workspace.tabs[1].pane_group.id();

            for forward in [false, true] {
                assert_eq!(
                    drag_step(workspace, 1, forward, None, &entry_paths, ctx),
                    1,
                    "a lone repository tab has nowhere to go (forward={forward})"
                );
                assert_eq!(
                    workspace.tabs[1].pane_group.id(),
                    dragged,
                    "and it is still the tab sitting there"
                );
                assert_eq!(
                    workspace.tabs[1].group_id,
                    Some(repo_group),
                    "still a member of its repository's group"
                );
                assert_eq!(
                    workspace
                        .tab_groups
                        .get(&repo_group)
                        .and_then(|group| group.repo_root.as_deref()),
                    Some("/repo/a"),
                    "which still exists and is still bound to the repository"
                );
                assert_eq!(
                    entry_indices(workspace, "/repo/a", &entry_paths),
                    vec![1],
                    "so the entry still lists it — the way back stays reachable"
                );
            }
            assert_groups_contiguous(workspace);
        });
    });
}
