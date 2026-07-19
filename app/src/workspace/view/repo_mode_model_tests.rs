use chrono::{Duration, Utc};
use warpui::App;

use super::*;
use crate::persistence::model::Project;
use crate::workspace::view::tests::{initialize_app, mock_workspace};

fn register_projects_model(app: &mut App, projects: Vec<Project>) {
    app.add_singleton_model(|ctx| ProjectManagementModel::new(projects, None, ctx));
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

            // Selected entry with no matching tabs: empty set, never
            // unrelated tabs (R10).
            workspace.selected_repo_root = Some("/repo/missing".to_string());
            assert_eq!(
                workspace.repo_mode_visible_tab_indices(ctx),
                Some(Vec::new())
            );
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

/// Covers R11 / F5: closing the last tab of the selected entry keeps the
/// selection and auto-opens a fresh tab at the entry root.
#[test]
fn test_last_tab_close_keeps_selection_and_reopens() {
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

            workspace.remove_tab(member_index, false, false, ctx);

            // Selection survives and a fresh group + tab exist (R11).
            assert_eq!(
                workspace.selected_repo_root.as_deref(),
                Some(root_str.as_str())
            );
            let new_group_id = workspace
                .selected_repo_mode_group_id()
                .expect("group recreated after last tab close");
            assert_eq!(
                workspace
                    .tabs
                    .iter()
                    .filter(|t| t.group_id == Some(new_group_id))
                    .count(),
                1
            );
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
