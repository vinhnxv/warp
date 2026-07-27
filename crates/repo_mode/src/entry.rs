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
