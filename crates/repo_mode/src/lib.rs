//! Domain types and path rules for Warp repo mode.
//!
//! Pure helpers only — no app/UI dependency. Selection state lives in
//! `app` (`RepoModeModel`).

mod entry;

pub use entry::{
    canonicalize_repo_path, classify_entry_kind, display_name_for_path,
    display_name_for_registry_path, format_remote_key, is_dead_path, is_remote_key, is_remote_path,
    parse_remote_key, shell_quote, RemoteTarget, RepoEntry, RepoEntryKind, DEFAULT_SSH_PORT,
};
