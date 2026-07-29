//! Registry bridge and selection helpers for repo mode.
//!
//! Selection state lives on [`Workspace`] (`selected_repo_root`). This module
//! owns list/add/remove/select operations against `ProjectManagementModel`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use chrono::NaiveDateTime;
use instant::Instant;
use pathfinder_geometry::vector::{Vector2F, vec2f};
use repo_mode::{
    RemoteProbeFailure, RemoteProbeOutcome, RemoteProbeState, RemoteTarget, RepoEntryKind,
    canonicalize_repo_path, classify_entry_kind, classify_probe_failure, display_name_for_path,
    display_name_for_registry_path, is_dead_path, is_remote_key, is_remote_path,
    parse_probe_output, parse_remote_key, remote_cd_command, remote_probe_args,
    remote_probe_script, remote_ssh_command,
};
use warp_errors::report_error;
use warpui::{AppContext, SingletonEntity, UpdateView, ViewContext, ViewHandle};
use warpui_core::r#async::FutureExt as _;

/// TTL for the cached repo-kind / liveness filesystem probes so
/// `repo_mode_entries` does not stat every registered path on each render
/// (mirrors the vertical-tabs branch cache). A stalled network/removable
/// mount would otherwise block the UI thread on every frame.
const REPO_FS_CACHE_TTL: Duration = Duration::from_secs(5);

/// Wall-clock bound on one SSH probe (R6). A host that accepts the connection
/// and then never answers must not leave the form hanging, so the whole round
/// trip is capped regardless of what `ConnectTimeout` does.
const REMOTE_PROBE_TIMEOUT: Duration = Duration::from_secs(12);
/// `ConnectTimeout` for the same probe: shorter than the wall-clock bound so a
/// dead host fails at the connect stage with a usable stderr message rather
/// than being killed by the outer timeout.
const REMOTE_PROBE_CONNECT_TIMEOUT_SECS: u64 = 8;

use super::Workspace;
use crate::context_chips::display_chip::GitLineChanges;
use crate::features::FeatureFlag;
use crate::menu::MenuItemFields;
use crate::pane_group::{NewTerminalOptions, PanesLayout};
use crate::projects::ProjectManagementModel;
use crate::terminal::TerminalView;
#[cfg(feature = "local_tty")]
use crate::terminal::local_shell::LocalShellState;
use crate::terminal::model::session::{BootstrapSessionType, SessionsEvent};
use crate::workspace::tab_group::{TabGroup, TabGroupId};
use crate::workspace::{TabContextMenuAnchor, WorkspaceAction, WorkspaceRegistry};

/// Snapshot of a registry entry for UI rendering, ordered by recency at launch.
#[derive(Clone, Debug)]
pub struct RepoModeListEntry {
    pub path: PathBuf,
    pub display_name: String,
    /// For a remote entry this mirrors the last probe and stays `Folder` until
    /// one resolves — read `remote` to tell "folder" from "not probed yet".
    pub kind: RepoEntryKind,
    pub is_dead: bool,
    pub last_opened_ts: Option<NaiveDateTime>,
    pub added_ts: NaiveDateTime,
    /// Connection and probe state for a remote (SSH) entry; `None` for a local
    /// one.
    pub remote: Option<RemoteListEntry>,
}

/// The remote half of a [`RepoModeListEntry`]: what machine it points at, and
/// what the last probe said about it.
#[derive(Clone, Debug)]
pub struct RemoteListEntry {
    pub target: RemoteTarget,
    pub probe: RemoteProbeState,
}

/// Git badge data for a repository row, sourced from the repo's terminals so
/// the row always matches what the tab rows display.
#[derive(Clone, Debug, Default)]
pub struct RepoModeEntryBadges {
    pub diff_stats: Option<GitLineChanges>,
    pub pull_request_url: Option<String>,
}

impl Workspace {
    /// True when repo mode is compiled in and the runtime flag is on.
    pub(super) fn repo_mode_enabled() -> bool {
        FeatureFlag::RepoMode.is_enabled()
    }

    /// Ordered registry list captured for the section (recency: last_opened then added).
    pub(super) fn repo_mode_entries(&self, ctx: &AppContext) -> Vec<RepoModeListEntry> {
        if !Self::repo_mode_enabled() {
            return Vec::new();
        }
        // Read the registry rows first (no filesystem work under the model
        // read-lock), then classify kind/liveness through the TTL cache below.
        let projects: Vec<(PathBuf, Option<NaiveDateTime>, NaiveDateTime)> =
            ProjectManagementModel::handle(ctx).read(ctx, |projects, _| {
                projects
                    .all_projects()
                    .map(|project| {
                        (
                            PathBuf::from(&project.path),
                            project.last_opened_ts,
                            project.added_ts,
                        )
                    })
                    .collect()
            });
        let now = Instant::now();
        let remote_probes = self.repo_mode_remote_probes.borrow();
        let mut fs_cache = self.repo_mode_fs_cache.borrow_mut();
        let mut entries: Vec<RepoModeListEntry> = projects
            .into_iter()
            .map(|(path, last_opened_ts, added_ts)| {
                let key = path.to_string_lossy().into_owned();
                if is_remote_key(&key) {
                    return remote_list_entry(key, path, last_opened_ts, added_ts, &remote_probes);
                }
                // `.git`/exists() stats hit the disk; reuse the last probe
                // within the TTL rather than re-statting on every render.
                let (kind, is_dead) = match fs_cache.get(&key) {
                    Some((probed_at, kind, dead))
                        if now.duration_since(*probed_at) < REPO_FS_CACHE_TTL =>
                    {
                        (*kind, *dead)
                    }
                    _ => {
                        let kind = classify_entry_kind(&path).unwrap_or(RepoEntryKind::Folder);
                        let dead = is_dead_path(&path);
                        fs_cache.insert(key, (now, kind, dead));
                        (kind, dead)
                    }
                };
                RepoModeListEntry {
                    display_name: display_name_for_path(&path),
                    is_dead,
                    path,
                    kind,
                    last_opened_ts,
                    added_ts,
                    remote: None,
                }
            })
            .collect();
        drop(fs_cache);
        drop(remote_probes);
        entries.sort_by(|a, b| {
            b.last_opened_ts
                .cmp(&a.last_opened_ts)
                .then(b.added_ts.cmp(&a.added_ts))
                .then(a.display_name.cmp(&b.display_name))
        });

        // R3: order settles at launch. Capture the recency order on first use;
        // later renders keep that order (selection bumps last_opened_ts for the
        // NEXT launch without reshuffling this session). Entries added during
        // the session append at the end.
        let mut launch_order = self.repo_mode_launch_order.borrow_mut();
        let order = launch_order.get_or_insert_with(|| {
            entries
                .iter()
                .map(|e| e.path.to_string_lossy().into_owned())
                .collect()
        });
        for entry in &entries {
            let key = entry.path.to_string_lossy();
            if !order.iter().any(|k| *k == key) {
                order.push(key.into_owned());
            }
        }
        entries.sort_by_key(|e| {
            let key = e.path.to_string_lossy();
            order.iter().position(|k| *k == key).unwrap_or(usize::MAX)
        });
        entries
    }

    /// Opens the "+ Add" menu (R1). Built the same way as the row context menu
    /// and the picker — `tab_right_click_menu` anchored at the click position —
    /// so the affordance stays native to the sidebar.
    pub(super) fn toggle_repo_mode_add_menu(
        &mut self,
        position: Vector2F,
        ctx: &mut ViewContext<Self>,
    ) {
        if !Self::repo_mode_enabled() {
            return;
        }
        if self.show_repo_mode_menu.is_some() {
            self.show_repo_mode_menu = None;
            ctx.notify();
            return;
        }
        let items: Vec<_> = repo_mode_add_menu_entries()
            .into_iter()
            .map(|(label, action)| {
                MenuItemFields::new(label)
                    .with_on_select_action(action)
                    .into_item()
            })
            .collect();
        ctx.update_view(&self.tab_right_click_menu, |menu, view_ctx| {
            menu.set_items(items, view_ctx);
        });
        self.show_tab_right_click_menu = None;
        self.show_tab_group_right_click_menu = None;
        self.show_repo_mode_menu = Some(TabContextMenuAnchor::Pointer(position));
        ctx.focus(&self.tab_right_click_menu);
        ctx.notify();
    }

    pub(super) fn open_folder_picker_for_repo_mode(&mut self, ctx: &mut ViewContext<Self>) {
        if !Self::repo_mode_enabled() {
            return;
        }
        // Select in the window that opened the picker (F1), not the first window.
        let window_id = ctx.window_id();
        ctx.open_file_picker(
            move |result, ctx| {
                let Ok(paths) = result else { return };
                let Some(path) = paths.into_iter().next() else {
                    return;
                };
                let canonical = match canonicalize_repo_path(Path::new(&path)) {
                    Ok(canonical) => canonical,
                    Err(err) => {
                        log::warn!(
                            "repo_mode: failed to canonicalize picked folder {:?}; not adding: {err}",
                            path
                        );
                        return;
                    }
                };
                ProjectManagementModel::handle(ctx).update(ctx, |projects, ctx| {
                    projects.upsert_project(canonical.clone(), ctx);
                });
                if let Some(workspace) = WorkspaceRegistry::as_ref(ctx).get(window_id, ctx) {
                    workspace.update(ctx, |workspace, ctx| {
                        workspace.select_repo_mode_entry(&canonical, ctx);
                    });
                }
            },
            warpui::platform::FilePickerConfiguration::new().folders_only(),
        );
    }

    /// Covers R6/R7/R9: register the connection as a pending entry so its row
    /// appears at once, then probe the host under a wall-clock bound. Success
    /// resolves the row in place and closes the form; failure leaves nothing
    /// registered and hands the form back with the reason.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn add_remote_repo_mode_entry(
        &mut self,
        token: u64,
        server: String,
        port: u16,
        user: String,
        identity: String,
        path: String,
        ctx: &mut ViewContext<Self>,
    ) {
        if !Self::repo_mode_enabled() {
            return;
        }
        let target = RemoteTarget {
            server,
            port,
            user,
            identity,
            remote_path: path,
        };
        let pending_key = target.key();
        ProjectManagementModel::handle(ctx).update(ctx, |projects, ctx| {
            projects.upsert_project(PathBuf::from(&pending_key), ctx);
        });
        self.repo_mode_remote_probes
            .borrow_mut()
            .insert(pending_key.clone(), RemoteProbeState::Pending);
        ctx.notify();
        self.spawn_remote_probe(target, pending_key, Some(token), ctx);
    }

    /// R11: remote entries are not polled. Displayed state is refreshed by use
    /// — selecting an entry reprobes it — so a stale row corrects itself the
    /// next time the user touches it.
    fn reprobe_remote_entry(&mut self, target: RemoteTarget, ctx: &mut ViewContext<Self>) {
        let key = target.key();
        self.spawn_remote_probe(target, key, None, ctx);
    }

    /// `token` is `Some` for an add-time probe, whose result drives the open
    /// form, and `None` for a reprobe of an entry the user already has.
    fn spawn_remote_probe(
        &mut self,
        target: RemoteTarget,
        key: String,
        token: Option<u64>,
        ctx: &mut ViewContext<Self>,
    ) {
        let args = remote_probe_args(&target, REMOTE_PROBE_CONNECT_TIMEOUT_SECS);
        let script = remote_probe_script(&target.remote_path);
        // The `ssh` the probe spawns is looked up on `PATH`, and a Warp launched
        // from the macOS GUI inherits launchd's `PATH` rather than the user's —
        // in the limit an empty one, where the lookup fails before any packet is
        // sent. Borrow the interactive shell's `PATH`, the same way `git`/`gh`
        // lookups already do.
        #[cfg(feature = "local_tty")]
        let path_future = if ctx.has_singleton_model::<LocalShellState>() {
            LocalShellState::handle(ctx).update(ctx, |shell_state, ctx| {
                shell_state.get_interactive_path_env_var(ctx)
            })
        } else {
            // No local shell to borrow a `PATH` from (a headless test, or a
            // build without one): fall back to the process environment.
            Box::pin(futures::future::ready(None))
        };
        #[cfg(not(feature = "local_tty"))]
        let path_future = futures::future::ready(None);
        ctx.spawn(
            async move {
                let path_env = path_future.await;
                run_remote_probe(args, script, REMOTE_PROBE_TIMEOUT, path_env).await
            },
            move |workspace, result, ctx| {
                workspace.apply_remote_probe_result(&target, &key, token, result, ctx);
            },
        );
    }

    /// Land a probe result on the registry, the probe cache, and (for an
    /// add-time probe) the form. Split out from the spawn so the resolution
    /// rules are exercisable without a subprocess.
    pub(super) fn apply_remote_probe_result(
        &mut self,
        target: &RemoteTarget,
        probed_key: &str,
        token: Option<u64>,
        result: Result<RemoteProbeOutcome, RemoteProbeFailure>,
        ctx: &mut ViewContext<Self>,
    ) {
        let failure = match result {
            Ok(RemoteProbeOutcome::Found {
                remote_path,
                kind,
                branch,
            }) => {
                // R3: the entry stores the path as the *host* expanded it, so a
                // `~` the user typed never survives into the key.
                let resolved = RemoteTarget {
                    remote_path,
                    ..target.clone()
                };
                let resolved_key = resolved.key();
                if resolved_key != probed_key {
                    self.replace_registry_key(probed_key, &resolved_key, ctx);
                }
                self.repo_mode_remote_probes
                    .borrow_mut()
                    .insert(resolved_key, RemoteProbeState::Resolved { kind, branch });
                if token.is_some() {
                    self.close_remote_connection_modal(ctx);
                }
                ctx.notify();
                return;
            }
            Ok(RemoteProbeOutcome::Missing) => RemoteProbeFailure::PathNotFound,
            Err(failure) => failure,
        };

        match token {
            // AE3: an add that fails registers nothing — the pending row goes
            // away and the form comes back with the reason.
            Some(token) => {
                self.drop_pending_remote_entry(probed_key, ctx);
                self.fail_remote_connection_modal(token, failure, ctx);
            }
            // A reprobe of an entry the user already has: mark it unreachable
            // and leave it in place. Removing it behind their back would lose
            // the entry over a temporary network blip.
            None => {
                self.repo_mode_remote_probes.borrow_mut().insert(
                    probed_key.to_string(),
                    RemoteProbeState::Failed { reason: failure },
                );
                ctx.notify();
            }
        }
    }

    /// Move a registry row to the key the probe resolved, without leaving the
    /// pending key behind in the registry, the probe cache, or the pinned
    /// launch order.
    fn replace_registry_key(&mut self, old: &str, new: &str, ctx: &mut ViewContext<Self>) {
        ProjectManagementModel::handle(ctx).update(ctx, |projects, ctx| {
            projects.remove_project(PathBuf::from(old), ctx);
            projects.upsert_project(PathBuf::from(new), ctx);
        });
        self.forget_remote_key(old);
    }

    fn drop_pending_remote_entry(&mut self, key: &str, ctx: &mut ViewContext<Self>) {
        ProjectManagementModel::handle(ctx).update(ctx, |projects, ctx| {
            projects.remove_project(PathBuf::from(key), ctx);
        });
        self.forget_remote_key(key);
        ctx.notify();
    }

    fn forget_remote_key(&self, key: &str) {
        self.repo_mode_remote_probes.borrow_mut().remove(key);
        if let Some(order) = self.repo_mode_launch_order.borrow_mut().as_mut() {
            order.retain(|pinned| pinned != key);
        }
    }

    pub(super) fn remove_repo_mode_entry(&mut self, path: &Path, ctx: &mut ViewContext<Self>) {
        if !Self::repo_mode_enabled() {
            return;
        }
        let path_buf = registry_key_path(path, "remove");
        let path_str = path_buf.to_string_lossy().into_owned();

        let group_ids: Vec<TabGroupId> = self
            .tab_groups
            .values()
            .filter(|g| g.repo_root.as_deref() == Some(path_str.as_str()))
            .map(|g| g.id)
            .collect();
        for group_id in group_ids {
            self.ungroup_tabs(group_id, ctx);
        }

        if self.selected_repo_root.as_deref() == Some(path_str.as_str()) {
            self.selected_repo_root = None;
        }

        // Drop the removed path from the pinned launch order so a later re-add
        // in the same session appends at the end (R3) instead of resurfacing at
        // its stale slot.
        if let Some(order) = self.repo_mode_launch_order.borrow_mut().as_mut() {
            order.retain(|k| k != &path_str);
        }
        self.repo_mode_fs_cache.borrow_mut().remove(&path_str);
        self.repo_mode_remote_probes.borrow_mut().remove(&path_str);

        ProjectManagementModel::handle(ctx).update(ctx, |projects, ctx| {
            projects.remove_project(path_buf, ctx);
        });
        ctx.notify();
    }

    pub(super) fn select_repo_mode_all(&mut self, ctx: &mut ViewContext<Self>) {
        if !Self::repo_mode_enabled() {
            return;
        }
        self.selected_repo_root = None;
        ctx.notify();
    }

    /// Collapses the repo selection when the user focuses a tab that lives
    /// outside the selected repo (e.g. a loose terminal below the divider), so
    /// the filtered tab strip always contains the active tab.
    pub(super) fn sync_repo_mode_selection_to_active_tab(&mut self, ctx: &mut ViewContext<Self>) {
        if !Self::repo_mode_enabled() {
            return;
        }
        let Some(visible) = self.repo_mode_visible_tab_indices(ctx) else {
            return;
        };
        if !visible.contains(&self.active_tab_index) {
            self.selected_repo_root = None;
            ctx.notify();
        }
    }

    /// Opens a plain terminal detached from every repo entry (R6): the
    /// selection is cleared before the tab is created and any group inherited
    /// from the active tab is dropped after, so the tab lands in "Other tabs"
    /// no matter which repo row was selected or focused when the user pressed
    /// "+ New". The tab starts at the user's home directory
    /// explicitly — the stock new-tab flow can inherit the previous session's
    /// cwd, and a "loose" terminal silently starting inside some repo's
    /// working tree would be surprising even though only group binding (not
    /// cwd) decides the sidebar section.
    pub(super) fn new_repo_mode_loose_tab(&mut self, ctx: &mut ViewContext<Self>) {
        if !Self::repo_mode_enabled() {
            return;
        }
        self.selected_repo_root = None;
        // Force an explicit directory so the new tab does not inherit the
        // prior session's cwd. If home is unavailable, fall back to the temp
        // dir rather than None — None would re-enable the inherit path.
        let initial_directory = dirs::home_dir().or_else(|| {
            log::warn!("repo_mode: no home directory; loose tab falling back to temp dir");
            Some(std::env::temp_dir())
        });
        self.add_tab_with_pane_layout(
            PanesLayout::SingleTerminal(Box::new(NewTerminalOptions {
                initial_directory,
                hide_homepage: true,
                ..Default::default()
            })),
            Arc::new(HashMap::new()),
            None,
            ctx,
        );
        // Clearing the selection only suppresses the R6 "join the selected
        // entry's group" branch; the stock new-tab path still inherits the
        // *active* tab's group, so opening this from a repo tab would file the
        // loose terminal under that repo. Detach it and move it past the
        // group's members (keeping group ranges contiguous).
        self.remove_tab_from_group(self.active_tab_index, ctx);
        ctx.notify();
    }

    pub(super) fn select_repo_mode_entry(&mut self, path: &Path, ctx: &mut ViewContext<Self>) {
        if !Self::repo_mode_enabled() {
            return;
        }
        let path_buf = registry_key_path(path, "select");
        self.selected_repo_root = Some(path_buf.to_string_lossy().into_owned());

        // R11: selecting a remote entry is the moment its displayed state gets
        // refreshed — there is no background poll to do it.
        if let Some(target) = path_buf.to_str().and_then(parse_remote_key) {
            self.reprobe_remote_entry(target, ctx);
        }

        // R3: record recency for the next launch. The section order is pinned
        // by `repo_mode_launch_order`, so this bump cannot reshuffle it now.
        ProjectManagementModel::handle(ctx).update(ctx, |projects, ctx| {
            projects.upsert_project(path_buf.clone(), ctx);
        });

        // Focus the MRU tab shown under this repo (bound tabs only, per the
        // display partition); with no such tab, open a fresh one at the entry
        // root — never absorb a loose tab that merely sits in this directory.
        let entry_paths: Vec<PathBuf> = self
            .repo_mode_entries(ctx)
            .into_iter()
            .map(|e| e.path)
            .collect();
        let (mut by_entry, _) = self.repo_mode_tab_partition(&entry_paths);
        let members = by_entry.remove(&path_buf).unwrap_or_default();
        if members.is_empty() {
            // Defense in depth. Attribution is by bound root, so a group bound
            // to this entry cannot report zero members here — the partition
            // reads the same binding this reads. Under the old cwd-based
            // attribution it could, and this branch then spawned a second
            // terminal for a repo that already had one. If a future
            // attribution change reintroduces that gap, activate the group's
            // real tab and report it rather than growing a redundant terminal.
            let bound = self.repo_mode_bound_group_tab_indices(&path_buf);
            if let Some(index) = self.mru_first_among(&bound) {
                report_error!(
                    anyhow::anyhow!(
                        "repo mode: display partition reported no tabs for a repo whose bound \
                         group has {} — attribution and binding have diverged",
                        bound.len()
                    ),
                    ReportErrorLogMode::OncePerRun
                );
                self.activate_tab(index, ctx);
            } else {
                match path_buf.to_str().and_then(parse_remote_key) {
                    Some(target) => self.open_remote_repo_mode_tab(&path_buf, &target, ctx),
                    None => self.create_repo_mode_group_with_tab(&path_buf, ctx),
                }
            }
        } else if !members.contains(&self.active_tab_index) {
            if let Some(index) = self.mru_first_among(&members) {
                self.activate_tab(index, ctx);
            }
        }
        ctx.notify();
    }

    pub(super) fn create_repo_mode_group_with_tab(
        &mut self,
        path: &Path,
        ctx: &mut ViewContext<Self>,
    ) {
        let path_str = path.to_string_lossy().into_owned();
        // Reuse any group already bound to this repo root instead of minting a
        // duplicate. The live-cwd partition can report zero members for a repo
        // whose only tab has cd'd away, but its bound group still exists and
        // must not be cloned (that would break the one-group-per-entry
        // invariant and make new-tab routing pick a group nondeterministically).
        let group_id = match self
            .tab_groups
            .values()
            .find(|g| g.repo_root.as_deref() == Some(path_str.as_str()))
            .map(|g| g.id)
        {
            Some(existing) => existing,
            None => {
                let mut group = TabGroup::new();
                group.repo_root = Some(path_str);
                group.name = Some(display_name_for_registry_path(path));
                let id = group.id;
                self.tab_groups.insert(id, group);
                id
            }
        };

        // Guard against a dead/removed root: spawning a shell at a missing dir
        // is shell-dependent breakage. Fall back to home when the root is gone.
        let initial_directory = if path.is_dir() {
            Some(path.to_path_buf())
        } else {
            dirs::home_dir()
        };
        self.add_tab_with_pane_layout(
            PanesLayout::SingleTerminal(Box::new(NewTerminalOptions {
                initial_directory,
                hide_homepage: true,
                ..Default::default()
            })),
            Arc::new(HashMap::new()),
            None,
            ctx,
        );
        if let Some(tab) = self.tabs.get_mut(self.active_tab_index) {
            tab.group_id = Some(group_id);
        }
    }

    /// Open a tab for a remote entry: connect to the host, then land in the
    /// entry's path (R12).
    ///
    /// Grouping is the local path exactly (KTD8/R14) — the group is bound by
    /// `repo_root`, and the remote key is just another value for it, so the tab
    /// filters like any local entry's. Only this path assigns a `group_id`, so
    /// an `ssh` the user types by hand stays ungrouped with no extra code (R15).
    ///
    /// The `cd` is deliberately *not* appended to the `ssh` line: a second
    /// positional argument would silently cost warpification (KTD7). It runs
    /// instead when the remote shell reports itself bootstrapped — see
    /// [`Self::land_in_remote_path_when_connected`].
    fn open_remote_repo_mode_tab(
        &mut self,
        key: &Path,
        target: &RemoteTarget,
        ctx: &mut ViewContext<Self>,
    ) {
        self.create_repo_mode_group_with_tab(key, ctx);

        let Some(terminal) = self
            .active_tab_pane_group()
            .as_ref(ctx)
            .active_session_view(ctx)
        else {
            log::warn!("repo_mode: remote tab opened without a terminal to connect from");
            return;
        };

        self.land_in_remote_path_when_connected(&terminal, &target.remote_path, ctx);

        // The tab's shell is still bootstrapping, so this queues and fires on
        // `BootstrapPrecmdDone` — the same route saved launch-config commands
        // take.
        let ssh_command = remote_ssh_command(target);
        terminal.update(ctx, |terminal, ctx| {
            terminal.execute_command_or_set_pending(&ssh_command, ctx);
        });
    }

    /// Run `cd <remote path>` once the remote shell is up, and only then.
    ///
    /// `SessionBootstrapped` with a [`BootstrapSessionType::WarpifiedRemote`]
    /// session is the first moment the remote shell can be commanded — the tab's
    /// own local shell bootstraps first and is filtered out here. Firing on the
    /// local bootstrap instead is exactly the mistake KTD7 rejects: the `cd`
    /// would land in the local shell before `ssh` had connected.
    ///
    /// Fires at most once. The subscription outlives that only in the sense that
    /// it stays registered on the tab's `Sessions` model, which dies with the
    /// tab — so a tab closed before its shell connects drops it silently.
    fn land_in_remote_path_when_connected(
        &mut self,
        terminal: &ViewHandle<TerminalView>,
        remote_path: &str,
        ctx: &mut ViewContext<Self>,
    ) {
        let sessions = terminal.as_ref(ctx).sessions_model().clone();
        let terminal = terminal.downgrade();
        let cd_command = remote_cd_command(remote_path);
        let mut landed = false;

        ctx.subscribe_to_model(&sessions, move |_, _, event, ctx| {
            if landed {
                return;
            }
            let SessionsEvent::SessionBootstrapped(bootstrapped) = event else {
                return;
            };
            if !matches!(
                bootstrapped.session_type,
                BootstrapSessionType::WarpifiedRemote
            ) {
                return;
            }
            let Some(terminal) = terminal.upgrade(ctx) else {
                return;
            };
            landed = true;
            let cd_command = cd_command.clone();
            terminal.update(ctx, |terminal, ctx| {
                terminal.execute_command_or_set_pending(&cd_command, ctx);
            });
        });
    }

    /// Most-recently-used index among `indices`, by `tab_mru_order` (front =
    /// most recent). Falls back to the first index given.
    fn mru_first_among(&self, indices: &[usize]) -> Option<usize> {
        for pane_group_id in &self.tab_mru_order {
            if let Some(&index) = indices.iter().find(|&&i| {
                self.tabs
                    .get(i)
                    .is_some_and(|t| t.pane_group.id() == *pane_group_id)
            }) {
                return Some(index);
            }
        }
        indices.first().copied()
    }

    /// Most-recently-used member of `group_id`, by `tab_mru_order` (front = most
    /// recent). Falls back to the first member by tab index.
    pub(super) fn mru_first_tab_index_in_group(&self, group_id: TabGroupId) -> Option<usize> {
        for pane_group_id in &self.tab_mru_order {
            if let Some(index) = self
                .tabs
                .iter()
                .position(|t| t.group_id == Some(group_id) && t.pane_group.id() == *pane_group_id)
            {
                return Some(index);
            }
        }
        self.tabs.iter().position(|t| t.group_id == Some(group_id))
    }

    /// Opens the row context menu for a registry entry (R4: healthy entries
    /// remove via context menu). Reuses `tab_right_click_menu`.
    pub(super) fn toggle_repo_mode_entry_menu(
        &mut self,
        path: &Path,
        position: Vector2F,
        ctx: &mut ViewContext<Self>,
    ) {
        if !Self::repo_mode_enabled() {
            return;
        }
        if self.show_repo_mode_menu.is_some() {
            self.show_repo_mode_menu = None;
            ctx.notify();
            return;
        }
        let items = vec![
            MenuItemFields::new("Remove from Repositories")
                .with_on_select_action(WorkspaceAction::RemoveRepoModeEntry(path.to_path_buf()))
                .into_item(),
        ];
        ctx.update_view(&self.tab_right_click_menu, |menu, view_ctx| {
            menu.set_items(items, view_ctx);
        });
        self.show_tab_right_click_menu = None;
        self.show_tab_group_right_click_menu = None;
        self.show_repo_mode_menu = Some(TabContextMenuAnchor::Pointer(position));
        ctx.focus(&self.tab_right_click_menu);
        ctx.notify();
    }

    /// Opens a picker menu listing "All" plus healthy registry entries (R13:
    /// repo switching with the vertical tabs panel closed).
    pub(super) fn open_repo_mode_picker_menu(&mut self, ctx: &mut ViewContext<Self>) {
        if !Self::repo_mode_enabled() {
            return;
        }
        if self.show_repo_mode_menu.is_some() {
            self.show_repo_mode_menu = None;
            ctx.notify();
            return;
        }
        let mut items = vec![
            MenuItemFields::new("All")
                .with_on_select_action(WorkspaceAction::SelectRepoModeAll)
                .into_item(),
        ];
        for entry in self.repo_mode_entries(ctx) {
            if entry.is_dead {
                continue;
            }
            items.push(
                MenuItemFields::new(entry.display_name.as_str())
                    .with_on_select_action(WorkspaceAction::SelectRepoModeEntry(entry.path.clone()))
                    .into_item(),
            );
        }
        ctx.update_view(&self.tab_right_click_menu, |menu, view_ctx| {
            menu.set_items(items, view_ctx);
        });
        self.show_tab_right_click_menu = None;
        self.show_tab_group_right_click_menu = None;
        self.show_repo_mode_menu = Some(TabContextMenuAnchor::Pointer(vec2f(80., 80.)));
        ctx.focus(&self.tab_right_click_menu);
        ctx.notify();
    }

    /// Diff stats + PR link for a repo row, read from terminals whose current
    /// git repository IS this entry's path — group membership is irrelevant, so
    /// a terminal that cd'd into another repo never leaks that repo's status
    /// onto this row (membership is static per R12, but badges describe the
    /// repository, not the group). MRU tabs are consulted first so the badges
    /// track the terminal the user last touched.
    pub(super) fn repo_mode_entry_badges(
        &self,
        entry_path: &Path,
        app: &AppContext,
    ) -> RepoModeEntryBadges {
        let mut badges = RepoModeEntryBadges::default();

        // Tab indices in MRU order, then any tabs missing from the MRU list.
        let mut indices: Vec<usize> = self
            .tab_mru_order
            .iter()
            .filter_map(|pane_group_id| {
                self.tabs
                    .iter()
                    .position(|t| t.pane_group.id() == *pane_group_id)
            })
            .collect();
        for index in 0..self.tabs.len() {
            if !indices.contains(&index) {
                indices.push(index);
            }
        }

        for index in indices {
            let Some(tab) = self.tabs.get(index) else {
                continue;
            };
            for terminal_view in tab.pane_group.as_ref(app).terminal_views(app) {
                let terminal_view = terminal_view.as_ref(app);
                if terminal_view.current_local_repo_path() != Some(entry_path) {
                    continue;
                }
                if badges.diff_stats.is_none() {
                    badges.diff_stats = terminal_view.current_diff_line_changes(app);
                }
                if badges.pull_request_url.is_none() {
                    badges.pull_request_url = terminal_view.current_pull_request_url(app);
                }
                if badges.diff_stats.is_some() && badges.pull_request_url.is_some() {
                    return badges;
                }
            }
        }
        badges
    }

    /// Partition of tabs across registry entries for display. Only tabs bound
    /// to a repo group participate, and each belongs to the entry its group is
    /// bound to — see `repo_mode_bound_tab_owner` for why binding rather than
    /// live cwd. A bound root that has left the registry drops its tabs loose.
    /// Tabs with no repo-bound group always stay loose — even when their cwd
    /// sits inside a registered entry — so registering a new repository never
    /// absorbs pre-existing "Other tabs". Group membership itself is not
    /// mutated.
    ///
    /// Reads only `tabs` and `tab_groups`, so it touches no terminal state and
    /// is safe to call from a render path.
    pub(super) fn repo_mode_tab_partition(
        &self,
        entry_paths: &[PathBuf],
    ) -> (HashMap<PathBuf, Vec<usize>>, Vec<usize>) {
        let mut by_entry: HashMap<PathBuf, Vec<usize>> = HashMap::new();
        let mut loose = Vec::new();
        for (index, tab) in self.tabs.iter().enumerate() {
            let bound_root = tab
                .group_id
                .and_then(|gid| self.tab_groups.get(&gid))
                .and_then(|g| g.repo_root.as_deref())
                .map(PathBuf::from);
            // Loose tabs never depend on the cwd, so skip the probe entirely.
            let Some(bound_root) = bound_root else {
                loose.push(index);
                continue;
            };
            match repo_mode_bound_tab_owner(&bound_root, entry_paths) {
                Some(path) => by_entry.entry(path).or_default().push(index),
                None => loose.push(index),
            }
        }
        (by_entry, loose)
    }

    /// Tab indices in the group bound to `root`, in tab order; empty when no
    /// group is bound to it.
    ///
    /// Reads group membership directly rather than going through the display
    /// partition, which is what makes it usable as a backstop *for* that
    /// partition.
    pub(super) fn repo_mode_bound_group_tab_indices(&self, root: &Path) -> Vec<usize> {
        let root = root.to_string_lossy();
        let Some(group_id) = self
            .tab_groups
            .values()
            .find(|g| g.repo_root.as_deref() == Some(root.as_ref()))
            .map(|g| g.id)
        else {
            return Vec::new();
        };
        self.tabs
            .iter()
            .enumerate()
            .filter(|(_, tab)| tab.group_id == Some(group_id))
            .map(|(index, _)| index)
            .collect()
    }

    /// Bound tab-group id for the current selection, if any.
    pub(super) fn selected_repo_mode_group_id(&self) -> Option<TabGroupId> {
        let selected = self.selected_repo_root.as_deref()?;
        self.tab_groups
            .values()
            .find(|g| g.repo_root.as_deref() == Some(selected))
            .map(|g| g.id)
    }

    /// Tabs visible under the current repo-mode selection (all tabs when no
    /// selection / flag off), by the repo display partition. A selected entry
    /// with no matching tabs yields an empty list — never unrelated tabs (R10).
    pub(super) fn repo_mode_visible_tab_indices(&self, app: &AppContext) -> Option<Vec<usize>> {
        if !Self::repo_mode_enabled() {
            return None;
        }
        let selected = PathBuf::from(self.selected_repo_root.as_deref()?);
        let entry_paths: Vec<PathBuf> = self
            .repo_mode_entries(app)
            .into_iter()
            .map(|e| e.path)
            .collect();
        // If the selected root is no longer registered (e.g. removed in another
        // window), behave as "All" instead of stranding this window with an
        // empty strip and no in-tree way back.
        if !entry_paths.iter().any(|p| p == &selected) {
            return None;
        }
        let (mut by_entry, _) = self.repo_mode_tab_partition(&entry_paths);
        Some(by_entry.remove(&selected).unwrap_or_default())
    }
}

/// One bounded SSH round trip: the probe script goes in over stdin and a
/// classified answer comes back.
///
/// Last-resort `PATH` for the probe's `ssh` lookup: the POSIX default, which is
/// where every platform we ship on keeps its system `ssh`.
#[cfg(not(target_family = "wasm"))]
const FALLBACK_PROBE_PATH: Option<&str> = if cfg!(unix) {
    Some("/usr/bin:/bin:/usr/sbin:/sbin")
} else {
    None
};

/// The `PATH` to hand the probe's `ssh`, or `None` to inherit the process one.
///
/// Prefers the interactive shell's `PATH` so an `ssh` installed only where the
/// user's shell looks (Homebrew, `~/.local/bin`) is still found. Falls back to
/// [`FALLBACK_PROBE_PATH`] for the one inherited value that cannot work: an
/// *empty* `PATH` fails the lookup outright, where an absent one is already
/// backstopped by the system default.
///
/// `process_path_env` is passed in rather than read here so the rule is
/// exercisable without mutating the test process's environment.
#[cfg(not(target_family = "wasm"))]
pub(super) fn probe_path_env(
    shell_path_env: Option<String>,
    process_path_env: Option<std::ffi::OsString>,
) -> Option<String> {
    if let Some(path) = shell_path_env.filter(|path| !path.is_empty()) {
        return Some(path);
    }
    if process_path_env.is_some_and(|path| path.is_empty()) {
        return FALLBACK_PROBE_PATH.map(str::to_string);
    }
    None
}

/// Every failure mode collapses into a [`RemoteProbeFailure`] the form can
/// explain — including the wall-clock timeout, which is what stops a host that
/// accepts the connection and then goes silent from hanging the add (R6).
#[cfg(not(target_family = "wasm"))]
async fn run_remote_probe(
    args: Vec<String>,
    script: String,
    timeout: Duration,
    path_env: Option<String>,
) -> Result<RemoteProbeOutcome, RemoteProbeFailure> {
    use std::process::Stdio;

    use futures::AsyncWriteExt as _;

    let mut command = command::r#async::Command::new("ssh");
    command
        .args(&args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    if let Some(path_env) = probe_path_env(path_env, std::env::var_os("PATH")) {
        command.env("PATH", path_env);
    }
    let mut child = command.spawn().map_err(|err| {
        // A spawn failure is a *local* one — no packet reached the host — so it
        // must not be reported as an unreachable server (R7).
        log::warn!("repo_mode: failed to spawn the ssh probe: {err}");
        RemoteProbeFailure::SshUnavailable
    })?;

    if let Some(mut stdin) = child.stdin.take() {
        if let Err(err) = stdin.write_all(script.as_bytes()).await {
            log::warn!("repo_mode: failed to write the probe script to ssh: {err}");
            return Err(RemoteProbeFailure::Unreachable);
        }
        // Closing stdin is what makes the remote shell run and exit.
        drop(stdin);
    }

    let output = match child.output().with_timeout(timeout).await {
        Ok(Ok(output)) => output,
        Ok(Err(err)) => {
            log::warn!("repo_mode: ssh probe failed: {err}");
            return Err(RemoteProbeFailure::Unreachable);
        }
        Err(_) => {
            log::info!("repo_mode: ssh probe timed out after {timeout:?}");
            return Err(RemoteProbeFailure::Unreachable);
        }
    };

    if !output.status.success() {
        return Err(classify_probe_failure(
            output.status.code(),
            &String::from_utf8_lossy(&output.stderr),
        ));
    }
    parse_probe_output(&String::from_utf8_lossy(&output.stdout)).ok_or_else(|| {
        log::warn!("repo_mode: unreadable ssh probe output");
        RemoteProbeFailure::Unreachable
    })
}

/// Remote entries are a desktop affordance: the browser build has no local
/// `ssh` to spawn at all, which is exactly what
/// [`RemoteProbeFailure::SshUnavailable`] says.
#[cfg(target_family = "wasm")]
async fn run_remote_probe(
    _args: Vec<String>,
    _script: String,
    _timeout: Duration,
    _path_env: Option<String>,
) -> Result<RemoteProbeOutcome, RemoteProbeFailure> {
    Err(RemoteProbeFailure::SshUnavailable)
}

/// Label and action for each "+ Add" menu item (R1): the single add control
/// becomes a choice between a local and a remote entry.
fn repo_mode_add_menu_entries() -> [(&'static str, WorkspaceAction); 2] {
    [
        (
            "Local Repository or Folder…",
            WorkspaceAction::AddLocalRepositoryOrFolder,
        ),
        (
            "Remote Repository or Folder…",
            WorkspaceAction::AddRemoteRepositoryOrFolder,
        ),
    ]
}

/// Build the list row for a remote key. No filesystem and no network work
/// happens here (R11): kind, branch, and reachability come from the ephemeral
/// probe cache, which is empty until the entry is added or selected — so a
/// restored entry renders pending until it is next used (F4).
///
/// A key that carries the remote scheme but does not parse (hand-edited or
/// corrupted registry row) is surfaced as dead so the row offers "Remove",
/// rather than rendering as a live local folder that can never open.
fn remote_list_entry(
    key: String,
    path: PathBuf,
    last_opened_ts: Option<NaiveDateTime>,
    added_ts: NaiveDateTime,
    probes: &HashMap<String, RemoteProbeState>,
) -> RepoModeListEntry {
    let Some(target) = parse_remote_key(&key) else {
        log::warn!("repo_mode: unparseable remote registry key {key:?}");
        return RepoModeListEntry {
            display_name: key,
            kind: RepoEntryKind::Folder,
            is_dead: true,
            path,
            last_opened_ts,
            added_ts,
            remote: None,
        };
    };
    let probe = probes.get(&key).cloned().unwrap_or_default();
    RepoModeListEntry {
        display_name: target.display_name(),
        kind: probe.kind().unwrap_or(RepoEntryKind::Folder),
        // Remote entries are never polled for liveness (R11); an unreachable
        // host shows through the probe state, not the dead-entry affordance.
        is_dead: false,
        path,
        last_opened_ts,
        added_ts,
        remote: Some(RemoteListEntry { target, probe }),
    }
}

/// Registry key for `path`: canonicalized for a local entry, verbatim for a
/// remote key (KTD3) — a remote key names a directory on another machine and
/// has no local form to resolve.
fn registry_key_path(path: &Path, operation: &str) -> PathBuf {
    if is_remote_path(path) {
        return path.to_path_buf();
    }
    canonicalize_repo_path(path).unwrap_or_else(|err| {
        log::warn!("repo_mode: failed to canonicalize {path:?} on {operation}: {err}");
        path.to_path_buf()
    })
}

/// Owning entry for a repo-bound tab: its bound root, while that root is still
/// registered. `None` means the tab lands loose because its bound root left the
/// registry. Loose tabs never reach this function — they stay under "Other
/// tabs" unconditionally.
///
/// Attribution is by binding, deliberately *not* by the terminal's live cwd.
/// A tab's group binding is what the user established; a cwd is where a shell
/// happens to be standing. Following the cwd meant a tab that `cd`'d into
/// another registered repo silently moved out of its own repo's row, and then
/// selecting that repo found zero tabs and spawned a second terminal for a
/// repo that already had one. Binding is also stable: it does not change under
/// the user while sessions bootstrap, and reading it costs no per-frame probe
/// of every terminal's pwd.
fn repo_mode_bound_tab_owner(bound_root: &Path, entry_paths: &[PathBuf]) -> Option<PathBuf> {
    entry_paths
        .iter()
        .find(|p| p.as_path() == bound_root)
        .cloned()
}

#[cfg(test)]
#[path = "repo_mode_model_tests.rs"]
mod tests;
