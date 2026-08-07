//! Registry bridge and selection helpers for repo mode.
//!
//! Selection state lives on [`Workspace`] (`selected_repo_root`). This module
//! owns list/add/remove/select operations against `ProjectManagementModel`.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use chrono::NaiveDateTime;
use instant::Instant;
use pathfinder_geometry::rect::RectF;
use pathfinder_geometry::vector::{Vector2F, vec2f};
use repo_mode::{
    RemoteProbeFailure, RemoteProbeOutcome, RemoteProbeState, RemoteTarget, RepoEntryKind,
    canonicalize_repo_path, classify_entry_kind, classify_probe_failure, display_name_for_path,
    display_name_for_registry_path, is_dead_path, is_remote_key, is_remote_path,
    parse_probe_output, parse_remote_key, remote_cd_command, remote_probe_args,
    remote_probe_script, remote_ssh_command, remote_ssh_command_landing_in_path,
};
use settings::Setting as _;
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
/// Bound on borrowing the interactive shell's `PATH` before a probe runs.
///
/// That borrow runs `$SHELL -i -l -c` and has no bound of its own, so an rc
/// file that hangs — a stalled network mount, a slow plugin manager — hangs it
/// indefinitely. It is awaited *before* [`REMOTE_PROBE_TIMEOUT`] starts, so
/// without a bound here the probe never runs at all: the result callback is
/// never reached, the session stays in flight, and `begin_remote_probe` then
/// refuses every later reprobe of that key. The row reads "Connecting…" for the
/// life of the window with no way back.
const REMOTE_PROBE_PATH_TIMEOUT: Duration = Duration::from_secs(5);

use super::Workspace;
use super::repo_sidebar::repo_row_position_id;
use super::vertical_tabs::repo_tree_viewport_position_id;
use crate::context_chips::display_chip::GitLineChanges;
use crate::features::FeatureFlag;
use crate::menu::{MenuItem, MenuItemFields};
use crate::pane_group::{NewTerminalOptions, PanesLayout};
use crate::projects::ProjectManagementModel;
use crate::terminal::TerminalView;
#[cfg(feature = "local_tty")]
use crate::terminal::local_shell::LocalShellState;
use crate::terminal::model::session::{BootstrapSessionType, SessionsEvent};
use crate::terminal::warpify::settings::WarpifySettings;
use crate::workspace::tab_group::{TabGroup, TabGroupId};
use crate::workspace::{RepoRegistryKey, TabContextMenuAnchor, WorkspaceAction, WorkspaceRegistry};

/// One registry row as the ordering pass reads it: key, last-opened, added.
type RegistryRow = (PathBuf, Option<NaiveDateTime>, NaiveDateTime);

/// Snapshot of a registry entry for UI rendering, ordered at launch by the
/// manual order when there is one and by recency when there is not.
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
    /// Whether this row is a remote key the add form is still waiting on and the
    /// registry has yet to accept — the registry is what "verified" means (R9).
    /// Such a key is provisional, because the host can expand the path into a
    /// different key, so anything that depends on the key being stable turns on
    /// this.
    ///
    /// Deliberately not the same question as "is the probe pending", which is
    /// what `remote`'s probe state answers. The probe cache is per-window and
    /// runtime-only, so after a launch every registered remote row reads
    /// `Pending` until the user clicks it — and clicking a remote row with no
    /// tabs also force-opens one. A gate on `Pending` would therefore treat
    /// every remote row as provisional after a restart and make settling one
    /// mean opening a tab in it. A registered key is settled whether or not this
    /// window has probed it yet.
    pub unverified: bool,
}

/// The remote half of a [`RepoModeListEntry`]: what machine it points at, and
/// what the last probe said about it.
#[derive(Clone, Debug)]
pub struct RemoteListEntry {
    pub target: RemoteTarget,
    pub probe: RemoteProbeState,
}

/// One remote key's probe history: what the last probe said, and which
/// generation is entitled to speak for the key.
///
/// A probe is a subprocess that can outlive everything it was started for — the
/// modal that requested it, the entry it describes, even a later entry that
/// happens to reuse the key. Every callback therefore carries the generation it
/// was spawned under and is dropped unless the key still agrees, so a result can
/// only ever land on the state it was asked about.
#[derive(Clone, Debug)]
pub(super) struct RemoteProbeSession {
    /// Generation of the most recent probe started for this key. A callback
    /// holding any other value is stale.
    generation: u64,
    /// Whether that probe is still running. A second probe for a key is not
    /// started while one is in flight — a row cannot be more pending than
    /// pending, and two `ssh` connections per click is what RC4 is about.
    in_flight: bool,
    state: RemoteProbeState,
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

    /// Ordered registry list for the section: the manual order when one exists,
    /// otherwise recency (last_opened then added), re-sorted by this window's
    /// session pin.
    ///
    /// Despite the name this is not a getter. It is the render path's single
    /// pass over the live registry, so it is also where the per-window ordering
    /// state is maintained: it captures the session pin on first use and extends
    /// it with keys that have appeared since, records that this window has
    /// resolved a list against a stored order, retires the "shown connecting"
    /// marker for a key it has just placed in the pin, and cancels a row drag
    /// whose row has stopped being listed.
    ///
    /// That last one takes `borrow_mut()` on `repo_mode_row_drag`, so a caller
    /// that reaches this while holding `borrow()` on that cell panics — and only
    /// while a row is actually held, which is the hardest case to reach in a
    /// test.
    pub(super) fn repo_mode_entries(&self, ctx: &AppContext) -> Vec<RepoModeListEntry> {
        if !Self::repo_mode_enabled() {
            return Vec::new();
        }
        // Read the registry rows first (no filesystem work under the model
        // read-lock), then classify kind/liveness through the TTL cache below.
        // The manual order, if the user has ever set one, is read in the same
        // pass so the list is resolved against one snapshot of the registry.
        let (mut projects, manual_order): (Vec<RegistryRow>, Vec<String>) =
            ProjectManagementModel::handle(ctx).read(ctx, |projects, _| {
                // Whether anything is positioned falls out of the pass that
                // builds the rows, so the ordered read below is skipped
                // entirely until the user's first drag — it sorts the whole
                // registry, and this runs on every render.
                let mut any_manual_position = false;
                let rows = projects
                    .all_projects()
                    .map(|project| {
                        any_manual_position |= project.manual_position.is_some();
                        (
                            PathBuf::from(&project.path),
                            project.last_opened_ts,
                            project.added_ts,
                        )
                    })
                    .collect();
                if !any_manual_position {
                    return (rows, Vec::new());
                }
                // Positioned rows lead `projects_in_manual_order`, so the first
                // unpositioned one ends the manual order.
                let manual_order = projects
                    .projects_in_manual_order()
                    .into_iter()
                    .take_while(|project| project.manual_position.is_some())
                    .map(|project| project.path.clone())
                    .collect();
                (rows, manual_order)
            });

        // The key the add form is waiting on, while the registry has yet to
        // accept it — the registry is what "verified" means (R9). Project it
        // into the list so the row is visible while it connects, timestamped now
        // so it sorts to the top like the newest entry it is about to become.
        //
        // Read off the form rather than off "cached probe session, minus the
        // registry": a session is cached for every remote row the user clicks
        // and kept for the life of the window on success, so that difference
        // also matches a key another window has *removed*. Such a key would be
        // projected straight back into this window's list, at the top, refusing
        // a drag and re-registering itself on the next click.
        let registered: HashSet<String> = projects
            .iter()
            .map(|(path, _, _)| path.to_string_lossy().into_owned())
            .collect();
        let unverified = self
            .repo_mode_pending_remote_key
            .as_deref()
            .filter(|key| !registered.contains(*key));
        if let Some(key) = unverified {
            // Remember that this window put the key on screen as a
            // "Connecting…" row, so the pin can keep it near the top when it
            // verifies instead of letting it fall to the appended slot.
            self.repo_mode_projected_unverified
                .borrow_mut()
                .insert(key.to_string());
            let added = chrono::Utc::now().naive_utc();
            projects.push((PathBuf::from(key), Some(added), added));
        }

        let now = Instant::now();
        let remote_probes = self.repo_mode_remote_probes.borrow();
        let mut fs_cache = self.repo_mode_fs_cache.borrow_mut();
        let mut entries: Vec<RepoModeListEntry> = projects
            .into_iter()
            .map(|(path, last_opened_ts, added_ts)| {
                let key = path.to_string_lossy().into_owned();
                if is_remote_key(&key) {
                    let is_unverified = unverified == Some(key.as_str());
                    return remote_list_entry(
                        key,
                        path,
                        last_opened_ts,
                        added_ts,
                        &remote_probes,
                        is_unverified,
                    );
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
                    // Only remote keys are ever projected ahead of the registry.
                    unverified: false,
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

        // R2: once a manual order exists it owns the list, and recency stops
        // reordering it. Registry rows the order does not name keep their
        // recency order at the end — this sort is stable, so that survives —
        // which is what appends a newly added repository (R4).
        //
        // A projected pending-remote row is deliberately never in the registry
        // and so is never in the manual order. It stays at the top, where the
        // `now` timestamp above put it, rather than dropping into that tail: a
        // "Connecting…" row at the bottom of a long list can sit below the fold
        // with nothing to scroll it into view. The session-pin sort below
        // carries the same exception — it runs last, and a key that first
        // appears mid-session is appended to the pin, so without it the
        // guarantee would hold only in a window that had never rendered.
        if !manual_order.is_empty() {
            // R8: this window has now resolved its list against a stored order,
            // so an empty stored order later is somebody else's reset rather
            // than the pre-first-drag state. Recorded here because this is the
            // one place the stored order is read — by everything that resolves
            // the entry list, not only by a paint.
            self.repo_mode_saw_stored_order.set(true);
            entries.sort_by_key(|entry| {
                if entry.unverified {
                    return 0;
                }
                let key = entry.path.to_string_lossy();
                manual_order
                    .iter()
                    .position(|ordered| *ordered == key)
                    .map_or(usize::MAX, |position| position + 1)
            });
        }

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
        let mut projected_unverified = self.repo_mode_projected_unverified.borrow_mut();
        for entry in &entries {
            let key = entry.path.to_string_lossy();
            if order.iter().any(|k| *k == key) {
                continue;
            }
            // A key this window has shown connecting joins the pin at the
            // front rather than the end. It is rendering at the top right now —
            // the unverified exception below puts it there — and appending
            // would drop it the whole length of the list the moment the probe
            // lands and that exception stops applying. Special-cased once: it
            // is in the pin from here on, and every other newly appearing key
            // still appends, which is R4.
            if projected_unverified.remove::<str>(key.as_ref()) {
                order.insert(0, key.into_owned());
            } else {
                order.push(key.into_owned());
            }
        }
        drop(projected_unverified);
        entries.sort_by_key(|e| {
            if e.unverified {
                return 0;
            }
            let key = e.path.to_string_lossy();
            order
                .iter()
                .position(|k| *k == key)
                .map_or(usize::MAX, |position| position + 1)
        });

        // A row removed by another window mid-drag takes its `Draggable` with
        // it, so `on_drop` never fires and the drag would outlive the row it
        // names. This is the one place that sees the live key set every frame.
        let mut row_drag = self.repo_mode_row_drag.borrow_mut();
        if row_drag
            .as_ref()
            .is_some_and(|drag| !entries.iter().any(|entry| entry.path == drag.path))
        {
            *row_drag = None;
        }

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

    /// Covers R6/R7/R9: show the connection as a pending row at once, then probe
    /// the host under a wall-clock bound. Success registers the row at the key
    /// the host resolved and closes the form; failure leaves nothing registered
    /// and hands the form back with the reason.
    ///
    /// The pending row lives in the probe session, not the registry. Writing it
    /// on submit made the registry mean "the user typed this", so a host that
    /// never answered — or one the user cancelled — still came back after a
    /// restart as an entry they never successfully connected to.
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
        // A previous submit in this same form was for a different host: drop
        // its row rather than leaving two pending connections the user only
        // asked for one of.
        if let Some(previous) = self.repo_mode_pending_remote_key.take()
            && previous != pending_key
        {
            self.drop_pending_remote_entry(&previous, ctx);
        }
        let generation = self.restart_remote_probe(&pending_key);
        self.repo_mode_pending_remote_key = Some(pending_key.clone());
        ctx.notify();
        self.spawn_remote_probe(target, pending_key, generation, Some(token), ctx);
    }

    /// R11: remote entries are not polled. Displayed state is refreshed by use
    /// — selecting an entry reprobes it — so a stale row corrects itself the
    /// next time the user touches it.
    ///
    /// Skipped while a probe for the key is already running: selecting a row
    /// twice, or a row that is still resolving its first probe, must not open a
    /// second `ssh` connection to the same host.
    fn reprobe_remote_entry(&mut self, target: RemoteTarget, ctx: &mut ViewContext<Self>) {
        let key = target.key();
        let Some(generation) = self.begin_remote_probe(&key) else {
            return;
        };
        self.spawn_remote_probe(target, key, generation, None, ctx);
    }

    /// Claim the next generation for `key`, unless a probe is already running
    /// for it. `None` means "already covered — do not spawn".
    fn begin_remote_probe(&self, key: &str) -> Option<u64> {
        let mut sessions = self.repo_mode_remote_probes.borrow_mut();
        if sessions.get(key).is_some_and(|session| session.in_flight) {
            return None;
        }
        let generation = self.next_probe_generation();
        sessions
            .entry(key.to_string())
            .and_modify(|session| {
                session.generation = generation;
                session.in_flight = true;
            })
            .or_insert(RemoteProbeSession {
                generation,
                in_flight: true,
                // The row exists from this moment, before anything is
                // registered, and renders as pending until the probe answers.
                state: RemoteProbeState::Pending,
            });
        Some(generation)
    }

    /// Claim the next generation for `key` unconditionally, orphaning any probe
    /// already running for it.
    ///
    /// The user resubmitting the form is an explicit request for a fresh answer,
    /// and the form is waiting on a token only this probe carries — deferring to
    /// an older probe would leave the modal waiting for a result that will be
    /// discarded.
    fn restart_remote_probe(&self, key: &str) -> u64 {
        let generation = self.next_probe_generation();
        self.repo_mode_remote_probes.borrow_mut().insert(
            key.to_string(),
            RemoteProbeSession {
                generation,
                in_flight: true,
                state: RemoteProbeState::Pending,
            },
        );
        generation
    }

    fn next_probe_generation(&self) -> u64 {
        let generation = self.repo_mode_probe_generation.get().wrapping_add(1);
        self.repo_mode_probe_generation.set(generation);
        generation
    }

    /// Record a landed probe, or report that it is no longer wanted.
    ///
    /// `false` means the key was removed, re-added, or reprobed since this probe
    /// was spawned — the caller must drop the result rather than apply it.
    fn finish_remote_probe(&self, key: &str, generation: u64, state: RemoteProbeState) -> bool {
        let mut sessions = self.repo_mode_remote_probes.borrow_mut();
        let Some(session) = sessions.get_mut(key) else {
            return false;
        };
        if session.generation != generation {
            return false;
        }
        session.in_flight = false;
        session.state = state;
        true
    }

    /// `token` is `Some` for an add-time probe, whose result drives the open
    /// form, and `None` for a reprobe of an entry the user already has.
    fn spawn_remote_probe(
        &mut self,
        target: RemoteTarget,
        key: String,
        generation: u64,
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
                let path_env = borrowed_path_env(path_future, REMOTE_PROBE_PATH_TIMEOUT).await;
                run_remote_probe(args, script, REMOTE_PROBE_TIMEOUT, path_env).await
            },
            move |workspace, result, ctx| {
                workspace.apply_remote_probe_result(&target, &key, generation, token, result, ctx);
            },
        );
    }

    /// Land a probe result on the registry, the probe session, and (for an
    /// add-time probe) the form. Split out from the spawn so the resolution
    /// rules are exercisable without a subprocess.
    ///
    /// A result whose `generation` no longer owns `probed_key` is dropped
    /// without touching anything: the entry was removed, re-added, or reprobed
    /// while this probe was running, and it is describing a question nobody is
    /// still asking.
    pub(super) fn apply_remote_probe_result(
        &mut self,
        target: &RemoteTarget,
        probed_key: &str,
        generation: u64,
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
                if !self.finish_remote_probe(
                    probed_key,
                    generation,
                    RemoteProbeState::Resolved { kind, branch },
                ) {
                    return;
                }
                // R6/R9: the registry means "verified". Only a probe that found
                // the path writes a row, so a connection that never answered
                // cannot come back after a restart as an entry the user never
                // successfully made.
                self.commit_remote_key(probed_key, &resolved_key, ctx);
                if token.is_some() {
                    // Cleared before the close so the form's own cleanup does
                    // not drop the row this probe just verified.
                    self.repo_mode_pending_remote_key = None;
                    self.close_remote_connection_modal(ctx);
                }
                ctx.notify();
                return;
            }
            Ok(RemoteProbeOutcome::Missing) => RemoteProbeFailure::PathNotFound,
            Err(failure) => failure,
        };

        if !self.finish_remote_probe(
            probed_key,
            generation,
            RemoteProbeState::Failed { reason: failure },
        ) {
            return;
        }

        match token {
            // AE3: an add that fails registers nothing — the pending row goes
            // away and the form comes back with the reason.
            Some(token) => {
                if self.repo_mode_pending_remote_key.as_deref() == Some(probed_key) {
                    self.repo_mode_pending_remote_key = None;
                }
                self.drop_pending_remote_entry(probed_key, ctx);
                self.fail_remote_connection_modal(token, failure, ctx);
            }
            // A reprobe of an entry the user already has: mark it unreachable
            // and leave it in place. Removing it behind their back would lose
            // the entry over a temporary network blip.
            None => ctx.notify(),
        }
    }

    /// Register the key a probe resolved, retiring the key it was probed under
    /// when the host expanded the path to something else.
    ///
    /// An already-registered key is left alone rather than upserted: `upsert`
    /// bumps `last_opened_ts`, and merely confirming an entry is still reachable
    /// is not the user opening it (R3).
    fn commit_remote_key(&mut self, probed: &str, resolved: &str, ctx: &mut ViewContext<Self>) {
        let registered = ProjectManagementModel::handle(ctx).read(ctx, |projects, _| {
            projects.all_projects().any(|p| p.path == resolved)
        });
        ProjectManagementModel::handle(ctx).update(ctx, |projects, ctx| {
            if probed != resolved {
                projects.remove_project(PathBuf::from(probed), ctx);
            }
            if !registered {
                projects.upsert_project(PathBuf::from(resolved), ctx);
            }
        });
        // The key has stopped being provisional, so the "shown connecting"
        // marker has done its job — for the same-key case the pin already holds
        // it at the front, and for a key the host expanded the pin is rewritten
        // below.
        self.repo_mode_projected_unverified
            .borrow_mut()
            .remove(probed);
        if probed != resolved {
            // Move the session across, so the resolved row keeps the state the
            // probe just wrote and the retired key stops being rendered.
            let session = self.repo_mode_remote_probes.borrow_mut().remove(probed);
            if let Some(session) = session {
                self.repo_mode_remote_probes
                    .borrow_mut()
                    .insert(resolved.to_string(), session);
            }
            if let Some(order) = self.repo_mode_launch_order.borrow_mut().as_mut() {
                // The row the user watched connect keeps its place. The key
                // it was watched under is retired, so the pin takes the
                // resolved key in that same slot rather than dropping the row
                // to the end of the list the instant it verifies. A resolved
                // key the pin already holds stays where it is — the user
                // re-added a connection they already had — and the retired key
                // simply goes.
                match order.iter().position(|pinned| pinned == probed) {
                    Some(index) if !order.iter().any(|pinned| pinned == resolved) => {
                        order[index] = resolved.to_string();
                    }
                    _ => order.retain(|pinned| pinned != probed),
                }
            }
        }
    }

    /// Drop a row whose probe never succeeded. Nothing was persisted for it, but
    /// it was rendered and therefore clickable, so it can have collected a
    /// selection and a bound group like any other row.
    ///
    /// A key that is *registered* is not a pending row: the user re-added a
    /// connection they already have, or reopened the form on one. Failing that
    /// probe says the host is unreachable right now, which is not grounds to
    /// delete the entry, its bound group, and its tabs — the row keeps its
    /// unreachable state and stays exactly where it was.
    pub(super) fn drop_pending_remote_entry(&mut self, key: &str, ctx: &mut ViewContext<Self>) {
        let registered = ProjectManagementModel::handle(ctx).read(ctx, |projects, _| {
            projects.all_projects().any(|p| p.path == key)
        });
        if registered {
            return;
        }
        self.detach_repo_mode_key(key, ctx);
        self.forget_remote_key(key);
        ctx.notify();
    }

    fn forget_remote_key(&self, key: &str) {
        self.repo_mode_remote_probes.borrow_mut().remove(key);
        self.repo_mode_projected_unverified.borrow_mut().remove(key);
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

        self.detach_repo_mode_key(&path_str, ctx);
        self.repo_mode_fs_cache.borrow_mut().remove(&path_str);
        self.forget_remote_key(&path_str);

        ProjectManagementModel::handle(ctx).update(ctx, |projects, ctx| {
            projects.remove_project(path_buf, ctx);
        });
        ctx.notify();
    }

    /// A repository row's drag crossed the threshold (R1).
    ///
    /// Nothing is written here. The drag records the row's rect before the tab
    /// block folds away, which is the anchor the midpoint comparison re-bases on
    /// (KTD9/R19), and resolves the row it reads scrolling off. What the drop
    /// needs — whether the row actually moved (R16) — accumulates as the swaps
    /// fire.
    ///
    /// The path arrives straight off the rendered row, so it is already the
    /// registry key — deliberately not re-canonicalized, which would stat the
    /// filesystem on a pointer path.
    pub(super) fn start_repo_mode_entry_drag(
        &mut self,
        path: &Path,
        row_position: RectF,
        ctx: &mut ViewContext<Self>,
    ) {
        if !Self::repo_mode_enabled() {
            return;
        }
        self.anchor_repo_mode_row_drag(path, row_position, ctx);
        // R15: the tab block has to be gone before the first swap is decided,
        // so the rows the drag measures against are the ones the user sees.
        ctx.notify();
    }

    /// Start tracking a repository-row drag from this frame's geometry,
    /// discarding whatever was being tracked before.
    fn anchor_repo_mode_row_drag(
        &mut self,
        path: &Path,
        row_position: RectF,
        ctx: &mut ViewContext<Self>,
    ) {
        // R19: the row this drag will read scrolling off, resolved once so it is
        // the same row for the whole gesture. Absent until the list has been
        // painted, which in practice it always has by the time a row can be
        // picked up — a drag with no reference simply cannot tell a scroll from
        // the collapse, exactly as before.
        let scroll_reference = self.repo_mode_entry_paths(ctx).first().and_then(|first| {
            ctx.element_position_by_id(repo_row_position_id(first))
                .map(|rect| DragScrollReference {
                    key: first.clone(),
                    origin: rect.origin(),
                })
        });
        let drag = RepoModeRowDrag::new(path.to_path_buf(), row_position, scroll_reference);
        self.repo_mode_row_drag.replace(Some(drag));
    }

    /// One frame of a repository-row drag: swap the dragged row with the
    /// neighbour it has passed, if any.
    ///
    /// The swap lands in this window's session pin — the fourth
    /// pin-maintenance point (KTD7) — because `repo_mode_entries` re-applies
    /// that pin last, so a window that has drawn its list renders the pin and
    /// nothing else. Writing only the registry would leave R1's "renders in the
    /// new order immediately" invisible until the next relaunch.
    ///
    /// Every rect comes from the same position cache: the dragged row's own
    /// laid-out slot, its neighbours' slots, and the tree viewport. Notably
    /// *not* through `neighbor_drag_rect`, whose every branch is keyed to the
    /// tab list — it would compare this row against a tab's rect and swap at
    /// arbitrary thresholds.
    pub(super) fn drag_repo_mode_entry(
        &mut self,
        path: &Path,
        row_position: RectF,
        ctx: &mut ViewContext<Self>,
    ) {
        if !Self::repo_mode_enabled() {
            return;
        }
        let slot_rect = ctx.element_position_by_id(repo_row_position_id(path));
        // R19: the scroll reference's key is the drag's, but its rect comes from
        // the position cache — read before the drag is borrowed, so `anchored`
        // can take a whole frame's geometry at once.
        let reference_key = self
            .repo_mode_row_drag
            .borrow()
            .as_ref()
            .filter(|drag| drag.path == path)
            .and_then(|drag| drag.scroll_reference_key().map(Path::to_path_buf));
        let reference_rect = reference_key
            .as_deref()
            .and_then(|key| ctx.element_position_by_id(repo_row_position_id(key)));
        // Scoped so the borrow is released before `repo_mode_entry_paths` below,
        // which reads the same cell to prune a drag whose row is gone.
        let anchored = {
            let mut row_drag = self.repo_mode_row_drag.borrow_mut();
            row_drag
                .as_mut()
                .filter(|drag| drag.path == path)
                .map(|drag| drag.anchored(row_position, slot_rect, reference_rect))
        };
        let Some(dragged_rect) = anchored else {
            // A move with no matching start: this window never saw the
            // drag-start, or a previous drag never reached its drop. Re-anchor
            // on this frame rather than correcting by another drag's geometry.
            self.anchor_repo_mode_row_drag(path, row_position, ctx);
            return;
        };

        let entry_paths = self.repo_mode_entry_paths(ctx);
        let neighbor_rect = |forward| {
            repo_mode_row_neighbor(&entry_paths, path, forward)
                .and_then(|index| entry_paths.get(index))
                .and_then(|neighbor| ctx.element_position_by_id(repo_row_position_id(neighbor)))
        };
        let above_rect = neighbor_rect(false);
        let below_rect = neighbor_rect(true);
        let target = repo_mode_row_swap_target(
            &entry_paths,
            path,
            dragged_rect,
            ctx.element_position_by_id(repo_tree_viewport_position_id()),
            above_rect,
            below_rect,
        );
        let Some(target_index) = target else {
            return;
        };
        let Some(neighbor) = entry_paths.get(target_index) else {
            return;
        };
        // Which side the row is travelling to decides which rect it is about to
        // exchange slots with, and therefore how far its slot is about to move.
        let forward = repo_mode_row_neighbor(&entry_paths, path, true) == Some(target_index);
        let target_rect = if forward { below_rect } else { above_rect };
        if self.swap_repo_mode_pinned_rows(path, neighbor) {
            // A swap moves the dragged row's slot for a reason that is not the
            // tab block. Record how far, so the next frame can subtract it and
            // still measure a collapse that has not been paid for yet.
            if let Some(drag) = self.repo_mode_row_drag.borrow_mut().as_mut() {
                drag.record_swap(swap_slot_shift(slot_rect, target_rect, forward), neighbor);
            }
            ctx.notify();
        }
    }

    /// Swap two repository rows inside this window's session pin. Returns
    /// whether the pin actually changed.
    ///
    /// Guarded rather than assuming the pin is populated, like the three
    /// existing pin-maintenance sites: nothing forces a render between the
    /// window opening and the first drag event.
    ///
    /// A swap that lands is also recorded on the drag in flight, because this is
    /// the only place a repository row changes places with another one — see
    /// [`RepoModeRowDrag::passed_rows`] for what the record is for.
    pub(super) fn swap_repo_mode_pinned_rows(&self, dragged: &Path, neighbor: &Path) -> bool {
        let dragged_key = dragged.to_string_lossy();
        let neighbor_key = neighbor.to_string_lossy();
        let mut swapped = false;
        if let Some(order) = self.repo_mode_launch_order.borrow_mut().as_mut()
            && let Some(from) = order.iter().position(|key| key.as_str() == &*dragged_key)
            && let Some(to) = order.iter().position(|key| key.as_str() == &*neighbor_key)
        {
            order.swap(from, to);
            swapped = true;
        }
        if swapped
            && let Some(drag) = self.repo_mode_row_drag.borrow_mut().as_mut()
            && drag.path.as_path() == dragged
        {
            drag.record_passed_row(&neighbor_key);
        }
        swapped
    }

    /// The dragged repository row was released (R17): whatever the list is
    /// showing is what gets written, from wherever the pointer happens to be.
    ///
    /// Writes nothing when the row is on the same side of every other row as it
    /// was at drag start (R16) — see [`RepoModeRowDrag::passed_rows`].
    pub(super) fn drop_repo_mode_entry(&mut self, path: &Path, ctx: &mut ViewContext<Self>) {
        if !Self::repo_mode_enabled() {
            return;
        }
        let drag = self.repo_mode_row_drag.borrow_mut().take();
        let Some(drag) = drag.filter(|drag| drag.path == path) else {
            return;
        };
        if !drag.moved_a_row() {
            return;
        }
        let Some(pin) = self.repo_mode_launch_order.borrow().clone() else {
            return;
        };
        let key = path.to_string_lossy();
        let Some(pin_index) = pin.iter().position(|pinned| pinned.as_str() == &*key) else {
            return;
        };

        let stored: Vec<PathBuf> = ProjectManagementModel::handle(ctx).read(ctx, |projects, _| {
            projects
                .projects_in_manual_order()
                .into_iter()
                .take_while(|project| project.manual_position.is_some())
                .map(|project| PathBuf::from(&project.path))
                .collect()
        });
        // R8: another window reset the order since this one last rendered with
        // it. This window's pin is the pre-reset arrangement, and merging into
        // an empty stored order would write that whole arrangement straight
        // back — undoing the reset from a window that never asked to.
        //
        // The pin is dropped instead of replayed, so this window re-sorts by
        // recency on its next render and the drag that landed on the stale list
        // is discarded, once. KTD6 is not weakened by that: the rendered list
        // only changes because the user just acted on it.
        if stored.is_empty() && self.repo_mode_saw_stored_order.get() {
            self.repo_mode_launch_order.replace(None);
            self.repo_mode_saw_stored_order.set(false);
            ctx.notify();
            return;
        }
        let merged = merge_dragged_row(stored, &pin, pin_index);
        ProjectManagementModel::handle(ctx).update(ctx, |projects, ctx| {
            projects.set_manual_order(merged, ctx);
        });
        ctx.notify();
    }

    /// Give the list back to recency (R8).
    ///
    /// Both halves are needed: clearing the stored positions is what survives a
    /// relaunch, and dropping the session pin is what lets this window re-sort
    /// without one — `repo_mode_entries` renders the pin and nothing else once
    /// it has captured a pin.
    pub(super) fn reset_repo_mode_order(&mut self, ctx: &mut ViewContext<Self>) {
        if !Self::repo_mode_enabled() {
            return;
        }
        self.repo_mode_launch_order.replace(None);
        // The acting window is about to render with no stored order, which is
        // exactly the state it was in before anyone's first drag — so its next
        // drag has to take the ordinary first-drag path and write the whole
        // list. Leaving the flag set would make this window discard its own
        // next drag as if some other window had done the reset.
        self.repo_mode_saw_stored_order.set(false);
        ProjectManagementModel::handle(ctx).update(ctx, |projects, ctx| {
            projects.clear_manual_order(ctx);
        });
        ctx.notify();
    }

    /// Cut a registry key loose from everything in this window that points at
    /// it: its bound group, the selection, and the pinned launch order.
    ///
    /// Shared by removal and by dropping a failed pending row, because a pending
    /// row is rendered and clickable and so can have collected all three.
    fn detach_repo_mode_key(&mut self, key: &str, ctx: &mut ViewContext<Self>) {
        let group_ids: Vec<TabGroupId> = self
            .tab_groups
            .values()
            .filter(|g| g.repo_root.as_deref() == Some(key))
            .map(|g| g.id)
            .collect();
        for group_id in group_ids {
            self.ungroup_tabs(group_id, ctx);
        }

        if self.selected_repo_root.as_deref() == Some(key) {
            self.selected_repo_root = None;
        }

        // Drop the key from the pinned launch order so a later re-add in the
        // same session appends at the end (R3) instead of resurfacing at its
        // stale slot.
        if let Some(order) = self.repo_mode_launch_order.borrow_mut().as_mut() {
            order.retain(|pinned| pinned != key);
        }
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
        let entry_paths = self.repo_mode_entry_paths(ctx);
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
        } else if !members.contains(&self.active_tab_index)
            && let Some(index) = self.mru_first_among(&members)
        {
            self.activate_tab(index, ctx);
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
        // The new tab becomes active before it can be bound below, and every
        // activation reconciles the repo selection against the active tab
        // (`sync_repo_mode_selection_to_active_tab`). An unbound tab belongs to
        // no entry's visible set, so that reconciliation would collapse the very
        // selection this call is servicing. Restore it once the binding lands,
        // at which point the tab really is a member and the invariant holds.
        let selection = self.selected_repo_root.clone();
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
        self.selected_repo_root = selection;
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
    ///
    /// That trade only pays while warpification is on. With it off no
    /// `WarpifiedRemote` session is ever created, so the bootstrap the `cd`
    /// waits for never arrives and the tab lands in the remote home directory,
    /// silently ignoring the path the user picked. There is no other "remote
    /// shell is ready" signal to hang it on, so that case sends the path on the
    /// command line instead — see [`remote_ssh_command_landing_in_path`].
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

        let warpification_enabled = *WarpifySettings::as_ref(ctx)
            .enable_ssh_warpification
            .value();

        if warpification_enabled {
            self.land_in_remote_path_when_connected(&terminal, &target.remote_path, ctx);
        }

        // The tab's shell is still bootstrapping, so this queues and fires on
        // `BootstrapPrecmdDone` — the same route saved launch-config commands
        // take.
        let ssh_command = if warpification_enabled {
            remote_ssh_command(target)
        } else {
            remote_ssh_command_landing_in_path(target)
        };
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
        let has_manual_order = ProjectManagementModel::handle(ctx).read(ctx, |projects, _| {
            projects
                .all_projects()
                .any(|project| project.manual_position.is_some())
        });
        let items: Vec<MenuItem<WorkspaceAction>> =
            repo_mode_entry_menu_entries(path, has_manual_order)
                .into_iter()
                .map(|entry| match entry {
                    Some((label, action)) => MenuItemFields::new(label)
                        .with_on_select_action(action)
                        .into_item(),
                    None => MenuItem::Separator,
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
                    .with_on_select_action(WorkspaceAction::SelectRepoModeEntry(RepoRegistryKey(
                        entry.path.clone(),
                    )))
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

    /// Diff stats + PR link for every entry in `entry_paths`, read from
    /// terminals whose current git repository IS that entry's path — group
    /// membership is irrelevant, so a terminal that cd'd into another repo never
    /// leaks that repo's status onto the wrong row (membership is static per
    /// R12, but badges describe the repository, not the group). MRU tabs are
    /// consulted first so the badges track the terminal the user last touched.
    ///
    /// One sweep for all entries, not one sweep per entry. The per-entry form
    /// walked every tab and every pane looking for one repository, so a sidebar
    /// with N entries over M panes did N×M terminal reads on every frame — and
    /// nothing memoized it, because the underlying git state changes on its own
    /// and a stale badge is worse than a recomputed one. Inverting the loop
    /// makes it M reads per frame with no cache to go stale.
    ///
    /// Entries with no matching terminal are absent from the map; the caller
    /// reads a default for them.
    pub(super) fn repo_mode_badges_by_entry(
        &self,
        entry_paths: &[PathBuf],
        app: &AppContext,
    ) -> HashMap<PathBuf, RepoModeEntryBadges> {
        let mut badges: HashMap<PathBuf, RepoModeEntryBadges> = HashMap::new();
        if entry_paths.is_empty() {
            return badges;
        }
        let wanted: HashSet<&Path> = entry_paths.iter().map(PathBuf::as_path).collect();

        for index in self.tab_indices_in_mru_order() {
            let Some(tab) = self.tabs.get(index) else {
                continue;
            };
            for terminal_view in tab.pane_group.as_ref(app).terminal_views(app) {
                let terminal_view = terminal_view.as_ref(app);
                let Some(repo_path) = terminal_view.current_local_repo_path() else {
                    continue;
                };
                if !wanted.contains(repo_path) {
                    continue;
                }
                let entry = badges.entry(repo_path.to_path_buf()).or_default();
                if entry.diff_stats.is_some() && entry.pull_request_url.is_some() {
                    // An earlier, more recently used terminal already answered
                    // for this repository.
                    continue;
                }
                if entry.diff_stats.is_none() {
                    entry.diff_stats = terminal_view.current_diff_line_changes(app);
                }
                if entry.pull_request_url.is_none() {
                    entry.pull_request_url = terminal_view.current_pull_request_url(app);
                }
            }
        }
        badges
    }

    /// Tab indices in most-recently-used order, then any tab the MRU list does
    /// not mention, in tab order.
    ///
    /// The MRU list is keyed by pane-group id and can fall out of step with
    /// `tabs` — it holds ids for closed tabs, and a tab can exist before it is
    /// ever activated — so it is a ranking over `tabs`, never a listing of it.
    /// Every tab appears exactly once.
    pub(super) fn tab_indices_in_mru_order(&self) -> Vec<usize> {
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
        indices
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

    /// Registered entry paths. Walks the project registry, so hoist it once per
    /// drag event rather than calling it per lookup. `repo_mode_entries` already
    /// short-circuits to empty when `RepoMode` is off, so a flag-off build pays
    /// nothing.
    pub(super) fn repo_mode_entry_paths(&self, ctx: &AppContext) -> Vec<PathBuf> {
        self.repo_mode_entries(ctx)
            .into_iter()
            .map(|e| e.path)
            .collect()
    }

    /// The section a tab group belongs to: `Some(root)` when the group is bound
    /// to a still-registered repository entry, `None` (loose, i.e. "Other
    /// tabs") otherwise. One rule for every mode — repo mode renders the
    /// selected repository's tabs flattened, so there is no rendered container
    /// to ask, and the section has to come from the tab list itself.
    ///
    /// Ownership is resolved through `repo_mode_bound_tab_owner`, the same test
    /// `repo_mode_tab_partition` applies, so a root that has left the registry
    /// (removed in another window) reads loose here exactly as it renders loose
    /// there.
    ///
    /// Both flag gates are load-bearing, not defensive. Session restore copies
    /// `repo_root` unconditionally (unlike `pinned`, which is flag-gated), so a
    /// build with either flag off can be holding a group that carries a root it
    /// never displays. `RepoMode` off must behave exactly as before this
    /// existed; `GroupedTabs` off has no sections at all, and without that gate
    /// a restored `repo_root` would clamp reordering in a grouped-tabs-off
    /// build via callers that do not sit behind a `groups_enabled` check.
    pub(super) fn repo_mode_group_section(
        &self,
        group_id: TabGroupId,
        entry_paths: &[PathBuf],
    ) -> Option<PathBuf> {
        if !Self::repo_mode_enabled() || !FeatureFlag::GroupedTabs.is_enabled() {
            return None;
        }
        let bound_root = PathBuf::from(self.tab_groups.get(&group_id)?.repo_root.as_deref()?);
        repo_mode_bound_tab_owner(&bound_root, entry_paths)
    }

    /// The section of the tab at `tab_index`. Loose (`None`) when it has no
    /// group, its group carries no repo root, that root is not a registered
    /// entry, or either feature flag is off — see `repo_mode_group_section`.
    pub(super) fn repo_mode_tab_section(
        &self,
        tab_index: usize,
        entry_paths: &[PathBuf],
    ) -> Option<PathBuf> {
        let group_id = self.tabs.get(tab_index)?.group_id?;
        self.repo_mode_group_section(group_id, entry_paths)
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
        let entry_paths = self.repo_mode_entry_paths(app);
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

/// The interactive shell's `PATH` if it can be had within `timeout`, and the
/// process environment's otherwise.
///
/// Giving up is not a failure: a build with no local shell to borrow from
/// already passes `None` here and probes fine. The point is that waiting
/// forever *is* a failure, because this is awaited ahead of the probe's own
/// wall-clock bound — see [`REMOTE_PROBE_PATH_TIMEOUT`] for what that costs.
async fn borrowed_path_env(
    path_future: impl std::future::Future<Output = Option<String>>,
    timeout: Duration,
) -> Option<String> {
    match path_future.with_timeout(timeout).await {
        Ok(path_env) => path_env,
        Err(_) => {
            log::info!(
                "repo_mode: borrowing the interactive shell PATH timed out after {timeout:?}; \
                 probing with the process environment instead"
            );
            None
        }
    }
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
        return Err(classify_probe_failure(&String::from_utf8_lossy(
            &output.stderr,
        )));
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
    probes: &HashMap<String, RemoteProbeSession>,
    unverified: bool,
) -> RepoModeListEntry {
    let Some(target) = parse_remote_key(&key) else {
        // Neither logged nor displayed. The key is the whole connection string —
        // user, host, port, remote path, and the local path to a private key —
        // and this runs on the render path, so a warn here wrote those to the
        // log file on every frame, and showing the raw URI as the row's name put
        // them on screen. The row still offers "Remove", which is all the user
        // can do with a key nothing can read.
        return RepoModeListEntry {
            display_name: "Unreadable entry".to_string(),
            kind: RepoEntryKind::Folder,
            is_dead: true,
            path,
            last_opened_ts,
            added_ts,
            remote: None,
            unverified,
        };
    };
    // No session means a row persisted by an older build that registered on
    // submit rather than on success: not resolved, and not dead either. Pending
    // is the honest answer, and touching the row reprobes it (R11).
    let probe = probes
        .get(&key)
        .map(|session| session.state.clone())
        .unwrap_or_default();
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
        unverified,
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

/// The repository row `dragged_path` would swap with when it travels one step
/// in `forward`'s direction, as an index into the rendered order
/// (`repo_mode_entry_paths`). `None` is the clamp: the drag stays put.
///
/// Pure over the path list on purpose, and the same division of labour
/// `section_neighbor` uses for tabs. Geometry stays in the caller, which
/// resolves this index's rect and compares midpoints before committing the
/// swap — no test can populate the position cache that read goes through, and
/// this clamp has to be exercisable.
///
/// R11 falls out of the return type rather than out of a check: the result is
/// always an index into the Repositories list or nothing at all, so "Other
/// tabs" — which sits below that list and is not in it — can never be named as
/// a swap target, and no drag can push a row past the end of the list into it.
///
/// A `dragged_path` that is not in the list clamps rather than panics. A drag
/// outlives a frame, and another window can unregister the repository under it
/// mid-drag.
pub(super) fn repo_mode_row_neighbor(
    entry_paths: &[PathBuf],
    dragged_path: &Path,
    forward: bool,
) -> Option<usize> {
    let current_index = entry_paths
        .iter()
        .position(|path| path.as_path() == dragged_path)?;
    if forward {
        let below = current_index + 1;
        (below < entry_paths.len()).then_some(below)
    } else {
        current_index.checked_sub(1)
    }
}

/// The row the dragged repository should swap with this frame, as an index into
/// the rendered order, or `None` for "stay put".
///
/// Midpoints rather than edges, like the vertical tab variant
/// (`calculate_updated_tab_index_vertical`): with the tab block folded away for
/// the duration of the drag (R15) every row is the same height, so a midpoint
/// comparison cannot oscillate.
///
/// Three things all read as the same clamp, deliberately:
/// - no neighbour in that direction — the end of the list (R11);
/// - a neighbour with no rect at all, which is a row that stopped being painted
///   mid-drag (KTD8b) rather than one that moved;
/// - a neighbour whose rect does not intersect `viewport` (KTD8/R18).
///
/// That last one needs its own test because `ClippedScrollable` does not cull:
/// it paints its whole child inside a clip layer, so a row scrolled out of view
/// keeps publishing a perfectly fresh rect that would otherwise be a legal swap
/// target. A `viewport` of `None` clamps everything — it means the tree was not
/// painted, and swapping against rects from an unpainted tree is exactly what
/// there is no way to make safe.
pub(super) fn repo_mode_row_swap_target(
    entry_paths: &[PathBuf],
    dragged_path: &Path,
    dragged_rect: RectF,
    viewport: Option<RectF>,
    above_rect: Option<RectF>,
    below_rect: Option<RectF>,
) -> Option<usize> {
    let dragged_midpoint = (dragged_rect.min_y() + dragged_rect.max_y()) / 2.;
    // The annotation is load-bearing: the body resolves `Option::filter` on
    // this parameter, so its type has to be known here rather than at the call
    // sites below.
    let on_screen = |rect: Option<RectF>| {
        rect.filter(|rect| viewport.is_some_and(|viewport| viewport.intersects(*rect)))
    };

    if let Some(above_index) = repo_mode_row_neighbor(entry_paths, dragged_path, false)
        && let Some(rect) = on_screen(above_rect)
        && dragged_midpoint < (rect.min_y() + rect.max_y()) / 2.
    {
        return Some(above_index);
    }
    if let Some(below_index) = repo_mode_row_neighbor(entry_paths, dragged_path, true)
        && let Some(rect) = on_screen(below_rect)
        && dragged_midpoint > (rect.min_y() + rect.max_y()) / 2.
    {
        return Some(below_index);
    }
    None
}

/// Where the dragged row lands in the order the registry is holding.
///
/// The first drag hands the whole list over: with nothing stored, the session
/// pin *is* the order the user is looking at. Every drag after that moves only
/// the dragged row (KTD7b) and leaves every other stored position in its
/// existing relative order — writing the whole pin instead would make the
/// shared order last-writer-wins across every row, so a ten-repository
/// arrangement made in one window would be discarded by a one-row drag in
/// another, invisibly until the next relaunch.
///
/// The pin and the stored order are not index-aligned: the pin keeps keys for
/// repositories that have left the registry and gains keys the stored order
/// never had. The dragged row is therefore re-inserted after the nearest
/// preceding pin entry the stored order also knows, rather than at the pin's
/// raw index.
fn merge_dragged_row(stored: Vec<PathBuf>, pin: &[String], pin_index: usize) -> Vec<PathBuf> {
    let dragged = PathBuf::from(pin[pin_index].as_str());
    if stored.is_empty() {
        return pin.iter().map(|key| PathBuf::from(key.as_str())).collect();
    }

    let mut merged = stored;
    merged.retain(|path| *path != dragged);
    let insert_at = pin[..pin_index]
        .iter()
        .rev()
        .find_map(|preceding| {
            let preceding = Path::new(preceding.as_str());
            merged.iter().position(|path| path.as_path() == preceding)
        })
        .map_or(0, |index| index + 1);
    merged.insert(insert_at, dragged);
    merged
}

/// Rows of a repository row's context menu, in order; `None` is a separator.
///
/// "Reset order" is a list-level action living in a per-row menu, which is the
/// cheapest surface that already exists. It appears only once a manual order
/// exists — before the first drag there is nothing to reset, and an always-on
/// item would read as a control that does nothing.
fn repo_mode_entry_menu_entries(
    path: &Path,
    has_manual_order: bool,
) -> Vec<Option<(&'static str, WorkspaceAction)>> {
    let mut entries = vec![Some((
        "Remove from Repositories",
        WorkspaceAction::RemoveRepoModeEntry(RepoRegistryKey(path.to_path_buf())),
    ))];
    if has_manual_order {
        entries.push(None);
        entries.push(Some(("Reset order", WorkspaceAction::ResetRepoModeOrder)));
    }
    entries
}

/// A repository-row drag in flight.
///
/// Two things have to survive from the threshold crossing to the release. The
/// anchor, because `Draggable` freezes the cursor offset at mouse-down and
/// never recomputes it, so the rect it reports stays in pre-collapse
/// coordinates while every row below the tab block has moved up (KTD9/R19). And
/// the rows this drag has moved past, because R16 asks whether the row ended up
/// back where it began.
pub(super) struct RepoModeRowDrag {
    /// Registry key of the row being dragged.
    pub(super) path: PathBuf,
    /// The row's rect when the drag crossed the threshold — the last reading
    /// taken before the tab block can fold away.
    start_rect: RectF,
    /// How far the drag-start rect sits from where the row's slot actually is,
    /// captured once on the first frame that slot moves and subtracted from
    /// every reported rect afterwards.
    anchor_delta: Option<Vector2F>,
    /// Rows the dragged row is now on the *other* side of than it was at drag
    /// start — the whole of R16's question, since a drag moves nothing else.
    ///
    /// Membership is toggled rather than accumulated, so a drag that passes a
    /// row and passes it back leaves nothing behind. Two cheaper rules fail
    /// here: a "did a swap fire" flag reports a there-and-back drag as a move
    /// and hands the whole list over to a manual order the user never asked
    /// for, while comparing the session pin against a copy taken at drag start
    /// reports another window's mid-drag add or remove as one. Comparing only
    /// the keys those two pins share fixes the second at the cost of missing a
    /// genuine move *past* a row that appeared mid-drag, which is a move the
    /// user watched happen.
    passed_rows: HashSet<String>,
    /// Total slot displacement the swaps so far are expected to have caused the
    /// dragged row, so a reordered slot is never mistaken for the collapse.
    swap_shift: Vector2F,
    /// The row this drag reads scrolling off (R19), when one could be resolved
    /// at drag start.
    scroll_reference: Option<DragScrollReference>,
}

/// The row a repository-row drag measures scrolling against, and where its slot
/// sat when the drag started.
///
/// A scroll moves every row in the list by the same amount; the tab block
/// folding away (R15) moves only the rows *below* the selected repository. The
/// list's first row is the one row a collapse can never move — the block always
/// renders directly under the selected repository's own row, so nothing above
/// that row shifts, and the first row is never below it. Everything the first
/// row does is therefore scrolling, and taking it back out of the dragged row's
/// slot movement leaves the collapse on its own.
///
/// Deliberately *not* the dragged row's neighbour, which the drag already has to
/// hand: two adjacent rows are all but always on the same side of the tab block,
/// so a neighbour moves with the collapse exactly as the dragged row does and
/// subtracting it would cancel the very correction the anchor exists to make.
struct DragScrollReference {
    /// Registry key of the reference row, fixed for the life of the drag.
    key: PathBuf,
    /// Where its slot sat at drag start, advanced by the displacement each swap
    /// that moved it is expected to have caused — a swap reorders the list
    /// under the reference exactly as it does under the dragged row, and that
    /// is not scrolling either.
    origin: Vector2F,
}

impl RepoModeRowDrag {
    fn new(
        path: PathBuf,
        start_rect: RectF,
        scroll_reference: Option<DragScrollReference>,
    ) -> Self {
        Self {
            path,
            start_rect,
            anchor_delta: None,
            passed_rows: HashSet::new(),
            swap_shift: Vector2F::zero(),
            scroll_reference,
        }
    }

    /// Note that the dragged row has exchanged places with `neighbor`.
    fn record_passed_row(&mut self, neighbor: &str) {
        if !self.passed_rows.remove(neighbor) {
            self.passed_rows.insert(neighbor.to_string());
        }
    }

    /// Whether the dragged row ended up on a different side of any other row
    /// than it started on (R16).
    fn moved_a_row(&self) -> bool {
        !self.passed_rows.is_empty()
    }

    /// The row whose slot this drag reads scrolling off, if it resolved one.
    fn scroll_reference_key(&self) -> Option<&Path> {
        self.scroll_reference
            .as_ref()
            .map(|reference| reference.key.as_path())
    }

    /// The reported rect, moved into the coordinates the neighbour rects are in.
    ///
    /// `slot_rect` is the dragged row's own laid-out slot, read from the same
    /// position cache as those neighbours (the row's `SavePosition` wraps its
    /// `Draggable`, so it keeps publishing the slot while the overlay follows
    /// the cursor). Before any swap fires, a difference between that slot and
    /// the drag-start rect is the tab block collapsing and nothing else.
    ///
    /// Two properties make this the definition rather than "the first move
    /// frame after the collapse". It yields exactly zero in the three cases
    /// where nothing folds away — no repository selected, a selected repository
    /// with no open tabs, and a row dragged from above the tab block — so the
    /// correction never turns into an accumulated pointer offset. And it does
    /// not depend on detecting which frame the collapse landed on: `on_drag_start`
    /// and `on_drag` arrive on separate mouse events with no guaranteed render
    /// between them, so a rule keyed to the first move frame can capture
    /// pre-collapse geometry and stay skewed by the block's height for the rest
    /// of the drag. Until the slot moves, the neighbour rects have not moved
    /// either, so an uncorrected comparison is the consistent one.
    ///
    /// The slot has two other ways to move, and both are taken back out before
    /// what is left is called a collapse: a swap, which reorders the list under
    /// the row (`record_swap`), and a scroll, which moves every row in the list
    /// including `reference_rect`'s (see [`DragScrollReference`]).
    ///
    /// Two things hold that the expression below does not show. Its three terms
    /// compose by cancellation rather than by accumulation: the first is a
    /// *negated* displacement (drag-start rect minus current slot) while the
    /// other two are un-negated, so adding them subtracts the two slot movements
    /// that are not the collapse. And the sum is evaluated at most once per
    /// drag — only while `anchor_delta` is still `None` — even though
    /// `record_swap` goes on updating both of those terms for the rest of the
    /// gesture.
    fn anchored(
        &mut self,
        reported: RectF,
        slot_rect: Option<RectF>,
        reference_rect: Option<RectF>,
    ) -> RectF {
        let delta = match self.anchor_delta {
            Some(delta) => delta,
            None => {
                let Some(slot) = slot_rect else {
                    return reported;
                };
                let delta = self.start_rect.origin() - slot.origin()
                    + self.swap_shift
                    + self.scrolled_by(reference_rect);
                if delta == Vector2F::zero() {
                    return reported;
                }
                self.anchor_delta = Some(delta);
                delta
            }
        };
        RectF::new(reported.origin() - delta, reported.size())
    }

    /// How far the list has scrolled under this drag (R19), or zero when it has
    /// no reference row to read that off.
    ///
    /// Not "since the drag started": the reference row's recorded origin is
    /// advanced by every swap that moves it, so what is left here is scrolling
    /// alone — a reorder that happens to move the reference row is not a scroll.
    fn scrolled_by(&self, reference_rect: Option<RectF>) -> Vector2F {
        match (&self.scroll_reference, reference_rect) {
            (Some(reference), Some(rect)) => rect.origin() - reference.origin,
            _ => Vector2F::zero(),
        }
    }

    /// Account for a swap that has just fired: `slot_shift` is how far it is
    /// expected to move the dragged row's slot, and `neighbor` is the row it
    /// exchanged slots with.
    ///
    /// The anchor is deliberately left open across a swap rather than settled
    /// here. Settling it — freezing it at whatever it is, which for an
    /// unmeasured anchor is zero — loses the collapse correction outright
    /// whenever a swap is decided on a frame delivered before the collapse has
    /// painted, leaving the drag a tab block's height low for the rest of its
    /// life. Leaving it open and accounting for nothing is no better: with
    /// nothing folded away the post-swap slot move is a whole row, and capturing
    /// *that* as the anchor puts the corrected rect past the next neighbour on
    /// every frame. A swap moves the slot by a knowable amount, so it is
    /// subtracted here and whatever is left over is measured as the collapse.
    fn record_swap(&mut self, slot_shift: Vector2F, neighbor: &Path) {
        self.swap_shift += slot_shift;
        // The same reorder moves the scroll reference when the reference is one
        // of the two rows that exchanged slots, and that is not scrolling.
        let reference_shift = match self.scroll_reference_key() {
            Some(key) if key == self.path => slot_shift,
            Some(key) if key == neighbor => -slot_shift,
            _ => return,
        };
        if let Some(reference) = self.scroll_reference.as_mut() {
            reference.origin += reference_shift;
        }
    }
}

/// How far a swap moves the dragged row's slot: the two rows exchange slots, so
/// it is exactly the distance between them.
///
/// Measured rather than assumed to be one row height — R15 does make every row
/// the same height for the duration of a drag, but the two rects are right
/// there. With the dragged row's own slot missing from the position cache the
/// target's height, signed by the direction of travel, is the closest stand-in.
fn swap_slot_shift(slot: Option<RectF>, target: Option<RectF>, forward: bool) -> Vector2F {
    let Some(target) = target else {
        return Vector2F::zero();
    };
    match slot {
        Some(slot) => target.origin() - slot.origin(),
        None => vec2f(
            0.,
            if forward {
                target.height()
            } else {
                -target.height()
            },
        ),
    }
}

#[cfg(test)]
#[path = "repo_mode_model_tests.rs"]
mod tests;
