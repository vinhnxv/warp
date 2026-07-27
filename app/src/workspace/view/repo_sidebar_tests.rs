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
        REMOTE_LABEL_BUDGET,
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
        REMOTE_LABEL_BUDGET,
    );
    assert_eq!(state.branch, None);
    assert_eq!(state.status, RemoteRowStatus::Ready);
}

/// Covers R9: an entry that has not been probed yet says so, rather than
/// claiming to be an empty folder.
#[test]
fn unprobed_row_is_pending() {
    let state = remote_row_state(&remote(RemoteProbeState::Pending), REMOTE_LABEL_BUDGET);
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
    ] {
        let state = remote_row_state(
            &remote(RemoteProbeState::Failed { reason }),
            REMOTE_LABEL_BUDGET,
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
    let state = remote_row_state(&long, REMOTE_LABEL_BUDGET);

    assert!(state.secondary.chars().count() <= REMOTE_LABEL_BUDGET);
    assert!(state.secondary.contains('…'));
    assert!(state.tooltip.contains(&full));
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
