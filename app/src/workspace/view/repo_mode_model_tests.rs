use chrono::{Duration, Utc};
use repo_mode::{RemoteProbeOutcome, RemoteProbeState, format_remote_key};
use warpui::App;

use super::*;
use crate::persistence::model::Project;
use crate::workspace::view::tests::{initialize_app, mock_workspace};

fn register_projects_model(app: &mut App, projects: Vec<Project>) {
    app.add_singleton_model(|ctx| ProjectManagementModel::new(projects, None, ctx));
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
        },
        Project {
            path: "/repo/b".to_string(),
            added_ts: now,
            last_opened_ts: Some(now),
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
            let (by_entry, loose) = workspace.repo_mode_tab_partition(&entry_paths, ctx);
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

/// Sticky-loose display rule: the owner helper only ever sees repo-bound
/// tabs — loose tabs stay under "Other tabs" unconditionally, which is what
/// keeps a newly registered repository from absorbing a pre-existing loose
/// tab whose cwd happens to live inside it. For bound tabs, cwd picks the
/// deepest matching entry and the bound root anchors everything else.
#[test]
fn test_bound_tab_owner_rules() {
    let entries = vec![
        PathBuf::from("/repo/a"),
        PathBuf::from("/repo/a/nested"),
        PathBuf::from("/repo/b"),
    ];
    // cwd inside another entry: follow the cwd, deepest ancestor wins.
    assert_eq!(
        repo_mode_bound_tab_owner(
            Path::new("/repo/b"),
            Some(Path::new("/repo/a/nested/src")),
            &entries,
        ),
        Some(PathBuf::from("/repo/a/nested"))
    );
    // cwd outside every entry: the tab stays under its bound root.
    assert_eq!(
        repo_mode_bound_tab_owner(
            Path::new("/repo/b"),
            Some(Path::new("/home/user")),
            &entries
        ),
        Some(PathBuf::from("/repo/b"))
    );
    // Unknown cwd (booting session, or a tab with no terminal): bound root.
    assert_eq!(
        repo_mode_bound_tab_owner(Path::new("/repo/a"), None, &entries),
        Some(PathBuf::from("/repo/a"))
    );
    // Bound root no longer registered and cwd matches nothing: loose.
    assert_eq!(
        repo_mode_bound_tab_owner(Path::new("/repo/gone"), None, &entries),
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
        RemoteProbeState::Resolved {
            kind: RepoEntryKind::Repo,
            branch: Some("main".to_string()),
        },
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
        RemoteProbeState::Failed {
            reason: repo_mode::RemoteProbeFailure::Unreachable,
        },
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
    }];
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        register_projects_model(&mut app, projects);
        let workspace = mock_workspace(&mut app);
        workspace.update(&mut app, |workspace, ctx| {
            workspace
                .repo_mode_remote_probes
                .borrow_mut()
                .insert(pending_key.clone(), RemoteProbeState::Pending);

            workspace.apply_remote_probe_result(
                &target,
                &pending_key,
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
    let now = Utc::now().naive_utc();
    let projects = vec![Project {
        path: key.clone(),
        added_ts: now,
        last_opened_ts: Some(now),
    }];
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        register_projects_model(&mut app, projects);
        let workspace = mock_workspace(&mut app);
        workspace.update(&mut app, |workspace, ctx| {
            workspace
                .repo_mode_remote_probes
                .borrow_mut()
                .insert(key.clone(), RemoteProbeState::Pending);

            workspace.apply_remote_probe_result(
                &target,
                &key,
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
    }];
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        register_projects_model(&mut app, projects);
        let workspace = mock_workspace(&mut app);
        workspace.update(&mut app, |workspace, ctx| {
            workspace.apply_remote_probe_result(
                &target,
                &key,
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
        },
        Project {
            path: key_a.clone(),
            added_ts: now,
            last_opened_ts: Some(now),
        },
        Project {
            path: key_b.clone(),
            added_ts: now,
            last_opened_ts: Some(now),
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
        },
        Project {
            path: root_b.to_string_lossy().into_owned(),
            added_ts: now - Duration::days(1),
            last_opened_ts: Some(now - Duration::days(1)),
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
