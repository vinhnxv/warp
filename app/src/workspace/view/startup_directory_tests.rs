use super::*;
use crate::features::FeatureFlag;

/// NA12: a repo-mode entry root is a path in the host filesystem, so it must
/// not be handed to a session that does not share that filesystem.
///
/// The old code returned the selected root before `chosen_shell` was consulted
/// at all, so opening a WSL tab with a repo selected started the shell at a
/// host path — one that does not exist inside the distribution, or that names
/// something entirely different there.
#[test]
fn a_wsl_session_does_not_receive_the_host_repo_root() {
    let _repo_mode_guard = FeatureFlag::RepoMode.override_enabled(true);
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().to_string_lossy().into_owned();

    assert_eq!(
        repo_mode_startup_directory(Some(&root), false),
        None,
        "a session off the host system must fall through to the stock resolution"
    );

    // The same-system case still receives the root — the guard narrows the
    // branch rather than disabling it.
    assert_eq!(
        repo_mode_startup_directory(Some(&root), true),
        Some(dir.path().to_path_buf())
    );
}

/// A root that has been deleted since it was selected yields nothing, so the
/// shell is not launched at a missing directory.
#[test]
fn a_dead_root_falls_through_rather_than_launching_at_a_missing_directory() {
    let _repo_mode_guard = FeatureFlag::RepoMode.override_enabled(true);
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().to_string_lossy().into_owned();
    drop(dir);

    assert_eq!(repo_mode_startup_directory(Some(&root), true), None);
}

/// No selection, and the flag being off, both leave the stock resolution alone.
#[test]
fn no_selection_or_flag_off_supplies_no_directory() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().to_string_lossy().into_owned();

    {
        let _repo_mode_guard = FeatureFlag::RepoMode.override_enabled(true);
        assert_eq!(repo_mode_startup_directory(None, true), None);
    }

    let _repo_mode_guard = FeatureFlag::RepoMode.override_enabled(false);
    assert_eq!(repo_mode_startup_directory(Some(&root), true), None);
}
