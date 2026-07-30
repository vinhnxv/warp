//! Domain types and path rules for Warp repo mode.
//!
//! Pure helpers only — no app/UI dependency. Selection state lives in
//! `app` (`RepoModeModel`).

mod entry;

pub use entry::{
    DEFAULT_SSH_PORT, REMOTE_PROBE_SHELL_COMMAND, RemoteProbeFailure, RemoteProbeOutcome,
    RemoteProbeState, RemoteTarget, RepoEntry, RepoEntryKind, canonicalize_repo_path,
    classify_entry_kind, classify_probe_failure, display_name_for_path,
    display_name_for_registry_path, format_remote_key, is_dead_path, is_remote_key, is_remote_path,
    parse_probe_output, parse_remote_key, remote_cd_command, remote_probe_args,
    remote_probe_script, remote_ssh_command, shell_quote,
};
