use chrono::{Duration, NaiveDateTime, Utc};
use warpui::{App, AppContext};

use super::*;

/// A registry entry. The paths used here do not exist on disk, so
/// `canonicalize_registry_key` falls through to the path as written and the
/// assertions stay platform-independent.
fn project(path: &str, last_used_at: NaiveDateTime, manual_position: Option<i32>) -> Project {
    Project {
        path: path.to_string(),
        added_ts: last_used_at,
        last_opened_ts: Some(last_used_at),
        manual_position,
    }
}

fn register(app: &mut App, projects: Vec<Project>) {
    app.add_singleton_model(|ctx| ProjectManagementModel::new(projects, None, ctx));
}

fn ordered_paths(ctx: &AppContext) -> Vec<String> {
    ProjectManagementModel::handle(ctx).read(ctx, |projects, _| {
        projects
            .projects_in_manual_order()
            .into_iter()
            .map(|project| project.path.clone())
            .collect()
    })
}

fn manual_positions(ctx: &AppContext) -> Vec<(String, Option<i32>)> {
    ProjectManagementModel::handle(ctx).read(ctx, |projects, _| {
        projects
            .projects_in_manual_order()
            .into_iter()
            .map(|project| (project.path.clone(), project.manual_position))
            .collect()
    })
}

/// Covers R6: a set order is what the ordered read reports back, verbatim.
#[test]
fn test_set_manual_order_is_read_back_in_that_order() {
    App::test((), |mut app| async move {
        let now = Utc::now().naive_utc();
        register(
            &mut app,
            vec![
                project("/repo/a", now, None),
                project("/repo/b", now, None),
                project("/repo/c", now, None),
            ],
        );

        ProjectManagementModel::handle(&app).update(&mut app, |projects, ctx| {
            projects.set_manual_order(
                vec![
                    PathBuf::from("/repo/c"),
                    PathBuf::from("/repo/a"),
                    PathBuf::from("/repo/b"),
                ],
                ctx,
            );
        });

        app.read(|ctx| {
            assert_eq!(ordered_paths(ctx), vec!["/repo/c", "/repo/a", "/repo/b"]);
        });
    });
}

/// Covers R5: a path the registry has never heard of — a stale order from
/// another window naming a since-removed repository — is skipped, not a panic,
/// and does not consume a position.
#[test]
fn test_set_manual_order_ignores_unregistered_paths() {
    App::test((), |mut app| async move {
        let now = Utc::now().naive_utc();
        register(
            &mut app,
            vec![project("/repo/a", now, None), project("/repo/b", now, None)],
        );

        ProjectManagementModel::handle(&app).update(&mut app, |projects, ctx| {
            projects.set_manual_order(
                vec![
                    PathBuf::from("/repo/b"),
                    PathBuf::from("/repo/never-registered"),
                    PathBuf::from("/repo/a"),
                ],
                ctx,
            );
        });

        app.read(|ctx| {
            assert_eq!(
                manual_positions(ctx),
                vec![
                    ("/repo/b".to_string(), Some(0)),
                    ("/repo/a".to_string(), Some(1)),
                ]
            );
        });
    });
}

/// Covers R4: a repository added while a manual order is in effect gets a null
/// position and lands at the end of the list.
#[test]
fn test_projects_without_a_manual_position_sort_last() {
    App::test((), |mut app| async move {
        let now = Utc::now().naive_utc();
        // `/repo/newest` is the most recently used, so recency ordering alone
        // would put it first. The manual order has to beat that.
        register(
            &mut app,
            vec![
                project("/repo/a", now - Duration::hours(2), None),
                project("/repo/b", now - Duration::hours(1), None),
                project("/repo/newest", now, None),
            ],
        );

        ProjectManagementModel::handle(&app).update(&mut app, |projects, ctx| {
            projects.set_manual_order(
                vec![PathBuf::from("/repo/b"), PathBuf::from("/repo/a")],
                ctx,
            );
        });

        app.read(|ctx| {
            assert_eq!(
                ordered_paths(ctx),
                vec!["/repo/b", "/repo/a", "/repo/newest"]
            );
        });
    });
}

/// Covers R5: removing a repository drops it from the ordered read and leaves
/// the survivors on the positions they already held.
#[test]
fn test_removing_a_project_drops_it_from_the_order_without_renumbering() {
    App::test((), |mut app| async move {
        let now = Utc::now().naive_utc();
        register(
            &mut app,
            vec![
                project("/repo/a", now, None),
                project("/repo/b", now, None),
                project("/repo/c", now, None),
            ],
        );

        ProjectManagementModel::handle(&app).update(&mut app, |projects, ctx| {
            projects.set_manual_order(
                vec![
                    PathBuf::from("/repo/a"),
                    PathBuf::from("/repo/b"),
                    PathBuf::from("/repo/c"),
                ],
                ctx,
            );
            projects.remove_project(PathBuf::from("/repo/b"), ctx);
        });

        app.read(|ctx| {
            assert_eq!(
                manual_positions(ctx),
                vec![
                    ("/repo/a".to_string(), Some(0)),
                    ("/repo/c".to_string(), Some(2)),
                ]
            );
        });
    });
}

/// Covers R8: Reset order discards the manual order outright.
#[test]
fn test_clear_manual_order_leaves_every_project_without_a_position() {
    App::test((), |mut app| async move {
        let now = Utc::now().naive_utc();
        register(
            &mut app,
            vec![
                project("/repo/a", now - Duration::hours(1), Some(0)),
                project("/repo/b", now, Some(1)),
            ],
        );

        ProjectManagementModel::handle(&app).update(&mut app, |projects, ctx| {
            projects.clear_manual_order(ctx);
        });

        app.read(|ctx| {
            assert_eq!(
                manual_positions(ctx),
                vec![("/repo/b".to_string(), None), ("/repo/a".to_string(), None),],
                "with no manual order left, the read falls back to recency"
            );
        });
    });
}
