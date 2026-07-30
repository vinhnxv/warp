use repo_mode::{RemoteProbeFailure, RemoteProbeState, RemoteTarget};

use super::*;

fn remote(probe: RemoteProbeState) -> RemoteListEntry {
    RemoteListEntry {
        target: RemoteTarget {
            server: "10.0.0.7".to_string(),
            port: 22,
            user: "vinh".to_string(),
            identity: "/k".to_string(),
            remote_path: "/srv/app".to_string(),
        },
        probe,
    }
}

/// Covers R8/R10: a resolved repository shows its branch and names the machine
/// it points at.
#[test]
fn resolved_repo_row_shows_branch_and_host() {
    let state = remote_row_state(
        &remote(RemoteProbeState::Resolved {
            kind: RepoEntryKind::Repo,
            branch: Some("main".to_string()),
        }),
        SECONDARY_LABEL_BUDGET,
    );
    assert_eq!(state.branch.as_deref(), Some("main"));
    assert_eq!(state.status, RemoteRowStatus::Ready);
    assert_eq!(state.secondary, "vinh@10.0.0.7");
    assert_eq!(state.tooltip, "vinh@10.0.0.7");
}

/// Covers AE4: a plain directory renders as a folder with no branch and
/// nothing errors.
#[test]
fn resolved_folder_row_shows_no_branch() {
    let state = remote_row_state(
        &remote(RemoteProbeState::Resolved {
            kind: RepoEntryKind::Folder,
            branch: None,
        }),
        SECONDARY_LABEL_BUDGET,
    );
    assert_eq!(state.branch, None);
    assert_eq!(state.status, RemoteRowStatus::Ready);
}

/// Covers R9: an entry that has not been probed yet says so, rather than
/// claiming to be an empty folder.
#[test]
fn unprobed_row_is_pending() {
    let state = remote_row_state(&remote(RemoteProbeState::Pending), SECONDARY_LABEL_BUDGET);
    assert_eq!(state.status, RemoteRowStatus::Pending);
    assert_eq!(state.branch, None);
    assert!(state.tooltip.contains("vinh@10.0.0.7"));
}

/// Covers R11/R7: a failed probe dims the row and keeps the mapped reason
/// reachable on hover, so the state is diagnosable without reopening the form —
/// nothing rechecks it in the background.
#[test]
fn failed_row_is_unreachable_and_explains_why() {
    for reason in [
        RemoteProbeFailure::Unreachable,
        RemoteProbeFailure::NeedsFirstHandConnect,
        RemoteProbeFailure::PathNotFound,
        RemoteProbeFailure::SshUnavailable,
    ] {
        let state = remote_row_state(
            &remote(RemoteProbeState::Failed { reason }),
            SECONDARY_LABEL_BUDGET,
        );
        assert_eq!(state.status, RemoteRowStatus::Unreachable(reason));
        assert_eq!(state.branch, None);
        assert!(
            state.tooltip.contains(reason.message()),
            "tooltip {:?} should carry {:?}",
            state.tooltip,
            reason.message()
        );
        assert!(state.tooltip.contains("vinh@10.0.0.7"));
    }
}

/// A long `user@host` is ellipsized so it cannot push the row wider than the
/// sidebar, and the full value stays available on hover.
#[test]
fn long_host_label_truncates_but_the_tooltip_keeps_it_whole() {
    let long = RemoteListEntry {
        target: RemoteTarget {
            server: "build-runner-07.internal.example-company.com".to_string(),
            port: 22,
            user: "deployment-service".to_string(),
            identity: String::new(),
            remote_path: "/srv/app".to_string(),
        },
        probe: RemoteProbeState::Pending,
    };
    let full = long.target.user_host();
    let state = remote_row_state(&long, SECONDARY_LABEL_BUDGET);

    assert!(state.secondary.chars().count() <= SECONDARY_LABEL_BUDGET);
    assert!(state.secondary.contains('…'));
    assert!(state.tooltip.contains(&full));
}

/// NA11: the row body is never destructive. A dead row's body dispatches
/// nothing at all; removal is reachable only through the row's own "Remove"
/// button, which is wired separately in `render_entry_row`.
#[test]
fn a_dead_rows_body_click_never_removes_the_entry() {
    let path = Path::new("/repo/gone");

    assert!(
        repo_row_click_action(true, false, path).is_none(),
        "a left-click on a dead row's body must not remove the registry entry"
    );
    assert!(
        repo_row_click_action(true, true, path).is_none(),
        "and that holds whether or not the dead row is the selected one"
    );

    // A live row still selects and deselects.
    assert!(matches!(
        repo_row_click_action(false, false, path),
        Some(WorkspaceAction::SelectRepoModeEntry(ref p)) if p == path
    ));
    assert!(matches!(
        repo_row_click_action(false, true, path),
        Some(WorkspaceAction::SelectRepoModeAll)
    ));
}

/// Per-entry sidebar state is dropped when its entry leaves the registry,
/// rather than accumulating for the window's lifetime.
#[test]
fn removing_an_entry_leaves_no_sidebar_state_behind() {
    let state = RepoSidebarState::default();
    for key in ["/repo/a", "/repo/b"] {
        state
            .entry_rows
            .borrow_mut()
            .insert(key.to_string(), MouseStateHandle::default());
        state
            .pr_badges
            .borrow_mut()
            .insert(key.to_string(), MouseStateHandle::default());
        state
            .remove_buttons
            .borrow_mut()
            .insert(key.to_string(), MouseStateHandle::default());
        state
            .branch_cache
            .borrow_mut()
            .insert(key.to_string(), (Instant::now(), Some("main".to_string())));
    }

    let live: HashSet<String> = ["/repo/a".to_string()].into_iter().collect();
    state.prune_to(&live);

    for map_keys in [
        state
            .entry_rows
            .borrow()
            .keys()
            .cloned()
            .collect::<Vec<_>>(),
        state.pr_badges.borrow().keys().cloned().collect::<Vec<_>>(),
        state
            .remove_buttons
            .borrow()
            .keys()
            .cloned()
            .collect::<Vec<_>>(),
        state
            .branch_cache
            .borrow()
            .keys()
            .cloned()
            .collect::<Vec<_>>(),
    ] {
        assert_eq!(map_keys, vec!["/repo/a".to_string()]);
    }
}

#[test]
fn truncate_label_keeps_both_ends() {
    assert_eq!(truncate_label("short", 10), "short");
    assert_eq!(truncate_label("exactly-10", 10), "exactly-10");
    assert_eq!(truncate_label("abcdefghijkl", 9).chars().count(), 9);
    let truncated = truncate_label("vinh@build-runner.example.com", 12);
    assert!(truncated.starts_with("vinh@"));
    assert!(truncated.ends_with(".com"));
    // A budget too small to ellipsize meaningfully leaves the label alone
    // rather than returning something unreadable.
    assert_eq!(truncate_label("abcdef", 2), "abcdef");
}

/// The sidebar is fixed-width, so a long name or a deep path used to be cut off
/// by the panel edge with nothing to say it had been. Both are now ellipsized,
/// and hover still carries what was removed.
#[test]
fn a_long_name_and_a_deep_path_are_both_clipped() {
    // A local row's name is its path's last component, which is what makes the
    // full path a sufficient hover text for both.
    let name = "warp-terminal-experimental-rendering-backend";
    let path = format!("/Users/vinh/Desktop/repos/clients/acme/{name}");
    let labels = row_labels(name, path.clone(), path.clone());

    assert!(labels.name.chars().count() <= NAME_BUDGET);
    assert!(labels.name.contains('…'));
    assert!(labels.secondary.chars().count() <= SECONDARY_LABEL_BUDGET);
    assert!(labels.secondary.contains('…'));

    // The local row's hover text is its full path, which is where the clipped
    // characters remain readable. It is not prefixed with the name, because the
    // path already ends in it.
    assert_eq!(labels.hover, path);
}

/// A row that already fits is left exactly as it was — no stray ellipsis, and
/// no hover text invented for text that is fully visible.
#[test]
fn a_row_that_fits_is_left_alone() {
    let labels = row_labels(
        "warp",
        "~/repos/warp".to_string(),
        "/repos/warp".to_string(),
    );
    assert_eq!(labels.name, "warp");
    assert_eq!(labels.secondary, "~/repos/warp");
    assert_eq!(labels.hover, "/repos/warp");
}

/// A remote row's hover text is `user@host`, so a clipped *name* would vanish
/// with nowhere to read it — unlike a local row, whose path already contains
/// the name. The full name is prefixed onto the hover text in that case.
///
/// The secondary line survives the second pass unchanged: `remote_row_state`
/// already clipped it to the same budget, so this must not add an ellipsis to
/// an ellipsis.
#[test]
fn a_clipped_remote_name_stays_readable_on_hover() {
    let name = "billing-service-integration-tests";
    let already_clipped = truncate_label(
        "deployment-service@build-runner-07.internal.example.com",
        SECONDARY_LABEL_BUDGET,
    );
    let labels = row_labels(name, already_clipped.clone(), already_clipped.clone());

    assert!(labels.name.contains('…'));
    assert!(
        labels.hover.starts_with(name),
        "the full name must survive somewhere the user can reach it"
    );
    assert_eq!(
        labels.secondary, already_clipped,
        "clipping an already-clipped label must be a no-op"
    );
}
