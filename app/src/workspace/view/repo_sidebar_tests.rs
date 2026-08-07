use std::sync::Arc;

use pathfinder_geometry::rect::RectF;
use repo_mode::{RemoteProbeFailure, RemoteProbeState, RemoteTarget};

use super::*;
use crate::workspace::view::repo_mode_model::repo_mode_row_swap_target;

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
        repo_row_click_action(false, true, false, path).is_none(),
        "a left-click on a dead row's body must not remove the registry entry"
    );
    assert!(
        repo_row_click_action(false, true, true, path).is_none(),
        "and that holds whether or not the dead row is the selected one"
    );

    // A live row still selects and deselects.
    assert!(matches!(
        repo_row_click_action(false, false, false, path),
        Some(WorkspaceAction::SelectRepoModeEntry(RepoRegistryKey(ref p))) if p == path
    ));
    assert!(matches!(
        repo_row_click_action(false, false, true, path),
        Some(WorkspaceAction::SelectRepoModeAll)
    ));
}

/// A registry row, as `repo_mode_entries` hands it to the sidebar. Registered,
/// so its key is settled whatever the probe currently says.
fn list_entry(path: &str, probe: Option<RemoteProbeState>, is_dead: bool) -> RepoModeListEntry {
    let now = chrono::Utc::now().naive_utc();
    RepoModeListEntry {
        path: PathBuf::from(path),
        display_name: path.to_string(),
        kind: RepoEntryKind::Folder,
        is_dead,
        last_opened_ts: Some(now),
        added_ts: now,
        remote: probe.map(remote),
        unverified: false,
    }
}

/// A remote row projected into the list ahead of the registry — see
/// [`RepoModeListEntry::unverified`].
fn unverified_entry(path: &str) -> RepoModeListEntry {
    RepoModeListEntry {
        unverified: true,
        ..list_entry(path, Some(RemoteProbeState::Pending), false)
    }
}

/// Covers AE9/R14. A remote row whose key the registry has not accepted cannot
/// be picked up: an order written against a provisional key would name a
/// repository that is about to stop existing. A resolved row and a dead one are
/// draggable on the same terms as any other (R12) — a dead row's key is
/// settled, only its path is unreachable.
#[test]
fn an_unverified_remote_row_is_the_only_row_that_cannot_be_dragged() {
    assert!(!repo_row_is_draggable(&unverified_entry(
        "ssh://vinh@10.0.0.7/srv/app"
    )));
    // The regression this gate was rewritten for: a registered row reads
    // `Pending` after every launch, and its key is settled all the same — see
    // [`RepoModeListEntry::unverified`].
    assert!(repo_row_is_draggable(&list_entry(
        "ssh://vinh@10.0.0.7/srv/app",
        Some(RemoteProbeState::Pending),
        false
    )));
    assert!(repo_row_is_draggable(&list_entry(
        "ssh://vinh@10.0.0.7/srv/app",
        Some(RemoteProbeState::Resolved {
            kind: RepoEntryKind::Repo,
            branch: Some("main".to_string()),
        }),
        false
    )));
    assert!(repo_row_is_draggable(&list_entry(
        "ssh://vinh@10.0.0.7/srv/app",
        Some(RemoteProbeState::Failed {
            reason: RemoteProbeFailure::Unreachable,
        }),
        false
    )));
    assert!(repo_row_is_draggable(&list_entry("/repo/gone", None, true)));
    assert!(repo_row_is_draggable(&list_entry("/repo/a", None, false)));
}

/// An unverified remote row still publishes a position id, even though it
/// cannot be dragged. A row with no published rect is invisible to the
/// neighbour lookup, so an unwrapped one would clamp every drag that had to
/// cross it — and it sorts to the *top* of the list, where it would sit between
/// the first row and every row below it.
#[test]
fn an_unverified_remote_row_still_publishes_a_position() {
    let entries = [
        unverified_entry("ssh://vinh@10.0.0.7/srv/app"),
        list_entry("/repo/a", None, false),
        list_entry("/repo/b", None, false),
    ];
    let ids: Vec<String> = entries
        .iter()
        .map(|entry| repo_row_position_id(&entry.path))
        .collect();
    assert_eq!(ids.len(), 3);
    assert!(
        ids.iter().all(|id| !id.is_empty()),
        "every row publishes a rect, draggable or not"
    );
    assert_eq!(
        ids.iter().collect::<HashSet<_>>().len(),
        3,
        "and the ids are per-row, so one row's rect cannot answer for another"
    );

    // With the pending row at index 0, the row below it still resolves it as a
    // reachable swap target rather than clamping there. The pending row sits at
    // y=0..40; /repo/a has been dragged up past its midpoint.
    let paths: Vec<PathBuf> = entries.iter().map(|entry| entry.path.clone()).collect();
    let viewport = RectF::new(vec2f(0., 0.), vec2f(200., 400.));
    assert_eq!(
        repo_mode_row_swap_target(
            &paths,
            Path::new("/repo/a"),
            RectF::new(vec2f(0., -5.), vec2f(200., 40.)),
            Some(viewport),
            Some(RectF::new(vec2f(0., 0.), vec2f(200., 40.))),
            None,
        ),
        Some(0)
    );
}

/// Per-entry drag state is dropped with the rest of a row's state when its
/// entry leaves the registry, and kept for one that has not.
#[test]
fn prune_to_drops_drag_state_for_a_departed_entry() {
    let state = RepoSidebarState::default();
    for key in ["/repo/a", "/repo/b"] {
        state
            .entry_drags
            .borrow_mut()
            .insert(key.to_string(), DraggableState::default());
    }

    let live: HashSet<String> = ["/repo/a".to_string()].into_iter().collect();
    state.prune_to(&live);

    assert_eq!(
        state
            .entry_drags
            .borrow()
            .keys()
            .cloned()
            .collect::<Vec<_>>(),
        vec!["/repo/a".to_string()]
    );
}

/// Covers AE6/R15. The selected repository's tab block folds away for the
/// duration of any repository-row drag, so every row in the list is the same
/// height and the drag's midpoint rule applies unchanged.
///
/// Visibility is derived from the drag state map rather than restored by the
/// drop handler (KTD11): `DraggableState` returns to not-dragging on mouse-up
/// on every path, so no terminal path can leave those tabs invisible.
#[test]
fn the_tab_block_folds_away_while_a_repository_row_is_dragged() {
    let state = RepoSidebarState::default();
    let dragged = DraggableState::default();
    state
        .entry_drags
        .borrow_mut()
        .insert("/repo/a".to_string(), dragged.clone());
    state
        .entry_drags
        .borrow_mut()
        .insert("/repo/b".to_string(), DraggableState::default());

    assert!(!state.any_entry_drag_active());
    assert!(
        repo_tab_block_visible(true, state.any_entry_drag_active()),
        "the selected repository shows its tabs when nothing is being dragged"
    );

    dragged.set_dragging(vec2f(0., 0.), vec2f(0., 0.));

    assert!(state.any_entry_drag_active());
    assert!(
        !repo_tab_block_visible(true, state.any_entry_drag_active()),
        "and hides them while *any* row is dragged, not just its own"
    );
    // An unselected row has no tab block either way.
    assert!(!repo_tab_block_visible(false, false));
}

/// Covers AE8/R13's actual mechanism. `repo_row_click_action`'s `is_dragging`
/// parameter is never `true` at runtime; what stops a released drag from also
/// selecting the repository — and spawning a terminal, and for a remote entry an
/// SSH session — is the drag start clearing the press off the row body's
/// interaction state. That only works because both sides address one handle.
///
/// The reset itself cannot be driven from here: `MouseState` exposes no way to
/// arm interaction state from outside `warpui_core`, and `EventContext` has no
/// public constructor, so there is no unit-level way to press a row. What is
/// assertable is the sharing, which is the half a refactor is likely to break.
#[test]
fn a_repository_rows_body_and_its_drag_share_one_interaction_state() {
    let state = RepoSidebarState::default();

    let row_body = state.entry_row_mouse("/repo/a");
    let drag_start = state.entry_row_mouse("/repo/a");
    assert!(
        Arc::ptr_eq(&row_body, &drag_start),
        "the drag has to clear the very state the row body recorded its press on"
    );
    assert!(
        !Arc::ptr_eq(&row_body, &state.entry_row_mouse("/repo/b")),
        "and one row's state must not answer for another's"
    );

    // Pruning drops the handle, so a re-added row starts from a clean one
    // rather than inheriting a press that was never released.
    state.prune_to(&HashSet::new());
    assert!(!Arc::ptr_eq(&row_body, &state.entry_row_mouse("/repo/a")));
}

/// Covers AE8/R13. Selecting a repository spawns a terminal, and for a remote
/// entry an SSH session, so a row that crossed the drag threshold must dispatch
/// nothing at all. A press and release below the threshold still selects, as it
/// does today.
#[test]
fn a_row_that_crossed_the_drag_threshold_dispatches_no_selection() {
    let path = Path::new("/repo/a");

    assert!(
        repo_row_click_action(true, false, false, path).is_none(),
        "no selection and no session spawn while the row is being dragged"
    );
    assert!(
        repo_row_click_action(true, false, true, path).is_none(),
        "and no deselection either"
    );
    assert!(matches!(
        repo_row_click_action(false, false, false, path),
        Some(WorkspaceAction::SelectRepoModeEntry(RepoRegistryKey(ref p))) if p == path
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
