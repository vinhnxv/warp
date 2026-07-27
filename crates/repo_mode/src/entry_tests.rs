use std::fs;
use std::path::PathBuf;

use tempfile::tempdir;

use super::*;

#[test]
fn canonicalize_dedups_trailing_slash_variants() {
    let dir = tempdir().unwrap();
    let base = dir.path().to_path_buf();
    let with_slash = PathBuf::from(format!("{}/", base.display()));

    let a = canonicalize_repo_path(&base).unwrap();
    let b = canonicalize_repo_path(&with_slash).unwrap();
    assert_eq!(a, b);
}

#[test]
fn classify_repo_vs_folder() {
    let dir = tempdir().unwrap();
    let folder = dir.path().join("plain");
    fs::create_dir(&folder).unwrap();
    assert_eq!(classify_entry_kind(&folder), Some(RepoEntryKind::Folder));

    let repo = dir.path().join("gitrepo");
    fs::create_dir(&repo).unwrap();
    fs::create_dir(repo.join(".git")).unwrap();
    assert_eq!(classify_entry_kind(&repo), Some(RepoEntryKind::Repo));
}

#[test]
fn dead_path_check() {
    let dir = tempdir().unwrap();
    let existing = dir.path().join("exists");
    fs::create_dir(&existing).unwrap();
    assert!(!is_dead_path(&existing));

    let missing = dir.path().join("gone");
    assert!(is_dead_path(&missing));
}

#[test]
fn repo_entry_from_path_sets_display_name() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("my-project");
    fs::create_dir(&path).unwrap();
    let entry = RepoEntry::from_path(&path).unwrap();
    assert_eq!(entry.display_name, "my-project");
    assert_eq!(entry.kind, RepoEntryKind::Folder);
}

fn target(server: &str, port: u16, user: &str, identity: &str, remote_path: &str) -> RemoteTarget {
    RemoteTarget {
        server: server.to_string(),
        port,
        user: user.to_string(),
        identity: identity.to_string(),
        remote_path: remote_path.to_string(),
    }
}

/// R4/AE1: the machine is part of the identity, so the same path on two hosts
/// is two entries.
#[test]
fn remote_keys_differ_by_host() {
    let a = format_remote_key("10.0.0.1", 22, "vinh", "/k", "/srv/app");
    let b = format_remote_key("10.0.0.2", 22, "vinh", "/k", "/srv/app");
    assert_ne!(a, b);
}

/// R4/AE2: a remote key can never collide with a local absolute path.
#[test]
fn remote_key_never_collides_with_local_path() {
    let remote = format_remote_key("10.0.0.1", 22, "vinh", "/k", "/srv/app");
    assert_ne!(remote, "/srv/app");
    assert!(is_remote_key(&remote));
    assert!(!is_remote_key("/srv/app"));
    assert!(!is_remote_key(r"C:\Users\v\app"));
}

#[test]
fn remote_key_round_trips_plain_components() {
    let want = target(
        "example.com",
        22,
        "vinh",
        "/Users/v/.ssh/id_ed25519",
        "/srv/app",
    );
    let key = want.key();
    assert_eq!(
        key,
        "ssh://vinh@example.com:22/srv/app?i=/Users/v/.ssh/id_ed25519"
    );
    assert_eq!(parse_remote_key(&key), Some(want));
}

/// KTD1: spaces in the identity or path must survive the key, and must not
/// arrive at the parser as raw bytes.
#[test]
fn remote_key_round_trips_spaces() {
    let want = target("h", 22, "vinh", "/Users/v/my keys/id", "/srv/my app");
    let key = want.key();
    assert!(!key.contains(' '), "space must be encoded in {key}");
    assert_eq!(parse_remote_key(&key), Some(want));
}

/// KTD1: `?`, `#`, `%`, `@` and `:` are legal in a Unix path, and R3 stores
/// whatever the host's `~`-expansion returns. An un-encoded delimiter would
/// silently yield a wrong path and a wrong identity.
#[test]
fn remote_key_round_trips_reserved_delimiters() {
    let want = target("h", 22, "us?er", "/keys/a#b%c", "/srv/a?b#c%d@e:f");
    let key = want.key();
    assert_eq!(
        key.matches('?').count(),
        1,
        "only the query delimiter may be a raw '?' in {key}"
    );
    assert_eq!(
        key.matches('@').count(),
        1,
        "only the userinfo delimiter may be a raw '@' in {key}"
    );
    assert!(!key.contains('#'), "'#' must be encoded in {key}");
    assert_eq!(parse_remote_key(&key), Some(want));
}

/// KTD1: an IPv6 host is bracketed so its colons cannot be mistaken for the
/// `:port` delimiter.
#[test]
fn remote_key_brackets_ipv6_host() {
    let want = target("::1", 2222, "root", "/k", "/srv");
    let key = want.key();
    assert_eq!(key, "ssh://root@[::1]:2222/srv?i=/k");
    assert_eq!(parse_remote_key(&key), Some(want));
}

#[test]
fn remote_key_preserves_port() {
    let default_port = parse_remote_key(&format_remote_key("h", 22, "u", "/k", "/srv")).unwrap();
    assert_eq!(default_port.port, 22);
    let custom = parse_remote_key(&format_remote_key("h", 2222, "u", "/k", "/srv")).unwrap();
    assert_eq!(custom.port, 2222);
}

#[test]
fn remote_key_round_trips_empty_identity() {
    let want = target("h", 22, "u", "", "/srv/app");
    let key = want.key();
    assert!(!key.contains('?'), "no query segment expected in {key}");
    assert_eq!(parse_remote_key(&key), Some(want));
}

#[test]
fn parse_remote_key_rejects_malformed_keys() {
    assert_eq!(parse_remote_key("/srv/app"), None);
    assert_eq!(
        parse_remote_key("ssh://example.com:22/srv"),
        None,
        "no user"
    );
    assert_eq!(parse_remote_key("ssh://u@h/srv"), None, "no port");
    assert_eq!(parse_remote_key("ssh://u@h:notaport/srv"), None);
    assert_eq!(parse_remote_key("ssh://u@h:22/sr%ZZv"), None, "bad escape");
}

/// R10: the row label is the path leaf, and the host when there is no leaf.
#[test]
fn remote_display_name_uses_path_leaf_then_host() {
    let with_leaf = parse_remote_key("ssh://u@h:22/srv/app?i=/k").unwrap();
    assert_eq!(with_leaf.display_name(), "app");
    let no_leaf = parse_remote_key("ssh://u@h:22/?i=/k").unwrap();
    assert_eq!(no_leaf.display_name(), "h");
    assert_eq!(no_leaf.user_host(), "u@h");
}

/// KTD10: an unquoted space in an identity or remote path word-splits into a
/// second positional argument, which silently drops warpification.
#[test]
fn shell_quote_wraps_and_escapes() {
    assert_eq!(shell_quote("/srv/app"), "'/srv/app'");
    assert_eq!(shell_quote("/srv/my app"), "'/srv/my app'");
    assert_eq!(shell_quote("a$b`c\\d"), "'a$b`c\\d'");
    assert_eq!(shell_quote("it's"), r#"'it'\''s'"#);
    assert_eq!(shell_quote(""), "''");
}

/// One label entry point for both kinds of registry key.
#[test]
fn registry_label_handles_local_and_remote_keys() {
    let dir = tempdir().unwrap();
    let local = dir.path().join("my-project");
    assert_eq!(display_name_for_registry_path(&local), "my-project");

    let key = format_remote_key("h", 22, "u", "/k", "/srv/app");
    assert_eq!(display_name_for_registry_path(Path::new(&key)), "app");
}

/// R9/R11: an entry with no probe yet is pending, and only a resolved probe
/// carries a kind — the local `.git` rules never answer for a remote entry.
#[test]
fn remote_probe_state_reports_kind_only_when_resolved() {
    assert_eq!(RemoteProbeState::default(), RemoteProbeState::Pending);
    assert_eq!(RemoteProbeState::Pending.kind(), None);
    assert_eq!(
        RemoteProbeState::Failed {
            reason: RemoteProbeFailure::Unreachable
        }
        .kind(),
        None
    );
    assert_eq!(
        RemoteProbeState::Resolved {
            kind: RepoEntryKind::Repo,
            branch: Some("main".to_string()),
        }
        .kind(),
        Some(RepoEntryKind::Repo)
    );
}

/// R7/KTD6: each failure class names what the user must actually do, so a
/// `BatchMode` false negative is not reported as "unreachable".
#[test]
fn remote_probe_failures_have_distinct_messages() {
    let messages = [
        RemoteProbeFailure::Unreachable.message(),
        RemoteProbeFailure::NeedsFirstHandConnect.message(),
        RemoteProbeFailure::PathNotFound.message(),
    ];
    for message in messages {
        assert!(!message.is_empty());
    }
    assert_ne!(messages[0], messages[1]);
    assert_ne!(messages[1], messages[2]);
    assert_ne!(messages[0], messages[2]);
}

/// KTD2: the local-filesystem rules must never run for a remote key.
#[test]
fn local_fs_rules_are_gated_to_local_keys() {
    let key = format_remote_key("h", 22, "u", "/k", "/srv/app");
    let remote = Path::new(&key);
    assert!(canonicalize_repo_path(remote).is_err());
    assert_eq!(classify_entry_kind(remote), None);
    // Never "dead": liveness for a remote entry comes from the probe (R11).
    assert!(!is_dead_path(remote));
}
