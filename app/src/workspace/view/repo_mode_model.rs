//! Registry bridge and selection helpers for repo mode.
//!
//! Selection state lives on [`Workspace`] (`selected_repo_root`). This module
//! owns list/add/remove/select operations against `ProjectManagementModel`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::NaiveDateTime;
use pathfinder_geometry::vector::{vec2f, Vector2F};
use repo_mode::{canonicalize_repo_path, display_name_for_path, is_dead_path, RepoEntryKind};
use warpui::{AppContext, SingletonEntity, UpdateView, ViewContext};

use super::Workspace;
use crate::context_chips::display_chip::GitLineChanges;
use crate::features::FeatureFlag;
use crate::menu::MenuItemFields;
use crate::pane_group::{NewTerminalOptions, PanesLayout};
use crate::projects::ProjectManagementModel;
use crate::workspace::tab_group::{TabGroup, TabGroupId};
use crate::workspace::{TabContextMenuAnchor, WorkspaceAction, WorkspaceRegistry};

/// Snapshot of a registry entry for UI rendering, ordered by recency at launch.
#[derive(Clone, Debug)]
pub struct RepoModeListEntry {
    pub path: PathBuf,
    pub display_name: String,
    pub kind: RepoEntryKind,
    pub is_dead: bool,
    pub last_opened_ts: Option<NaiveDateTime>,
    pub added_ts: NaiveDateTime,
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
        let mut entries: Vec<RepoModeListEntry> =
            ProjectManagementModel::handle(ctx).read(ctx, |projects, _| {
                projects
                    .all_projects()
                    .map(|project| {
                        let path = PathBuf::from(&project.path);
                        let kind = if path.join(".git").exists() {
                            RepoEntryKind::Repo
                        } else {
                            RepoEntryKind::Folder
                        };
                        RepoModeListEntry {
                            display_name: display_name_for_path(&path),
                            is_dead: is_dead_path(&path),
                            path,
                            kind,
                            last_opened_ts: project.last_opened_ts,
                            added_ts: project.added_ts,
                        }
                    })
                    .collect()
            });
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
                let Ok(canonical) = canonicalize_repo_path(Path::new(&path)) else {
                    return;
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

    pub(super) fn remove_repo_mode_entry(&mut self, path: &Path, ctx: &mut ViewContext<Self>) {
        if !Self::repo_mode_enabled() {
            return;
        }
        let path_buf = canonicalize_repo_path(path).unwrap_or_else(|_| path.to_path_buf());
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

    /// Opens a plain terminal detached from every repo entry: clearing the
    /// selection first means the new tab neither joins a repo group (R6) nor
    /// starts at an entry root. The tab starts at the user's home directory
    /// explicitly — the stock new-tab flow can inherit the previous session's
    /// cwd, which would classify the tab under that repo instead of the
    /// "Other tabs" section.
    pub(super) fn new_repo_mode_loose_tab(&mut self, ctx: &mut ViewContext<Self>) {
        if !Self::repo_mode_enabled() {
            return;
        }
        self.selected_repo_root = None;
        self.add_tab_with_pane_layout(
            PanesLayout::SingleTerminal(Box::new(NewTerminalOptions {
                initial_directory: dirs::home_dir(),
                hide_homepage: true,
                ..Default::default()
            })),
            Arc::new(HashMap::new()),
            None,
            ctx,
        );
        ctx.notify();
    }

    pub(super) fn select_repo_mode_entry(&mut self, path: &Path, ctx: &mut ViewContext<Self>) {
        if !Self::repo_mode_enabled() {
            return;
        }
        let path_buf = canonicalize_repo_path(path).unwrap_or_else(|_| path.to_path_buf());
        self.selected_repo_root = Some(path_buf.to_string_lossy().into_owned());

        // R3: record recency for the next launch. The section order is pinned
        // by `repo_mode_launch_order`, so this bump cannot reshuffle it now.
        ProjectManagementModel::handle(ctx).update(ctx, |projects, ctx| {
            projects.upsert_project(path_buf.clone(), ctx);
        });

        // Focus the MRU tab that lives in this repo (by live cwd partition);
        // with no such tab, open a fresh one at the entry root.
        let entry_paths: Vec<PathBuf> = self
            .repo_mode_entries(ctx)
            .into_iter()
            .map(|e| e.path)
            .collect();
        let (mut by_entry, _) = self.repo_mode_tab_partition(&entry_paths, ctx);
        let members = by_entry.remove(&path_buf).unwrap_or_default();
        if members.is_empty() {
            self.create_repo_mode_group_with_tab(&path_buf, ctx);
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
        let mut group = TabGroup::new();
        group.repo_root = Some(path.to_string_lossy().into_owned());
        group.name = Some(display_name_for_path(path));
        let group_id = group.id;
        self.tab_groups.insert(group_id, group);

        self.add_tab_with_pane_layout(
            PanesLayout::SingleTerminal(Box::new(NewTerminalOptions {
                initial_directory: Some(path.to_path_buf()),
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
        let items = vec![MenuItemFields::new("Remove from Repositories")
            .with_on_select_action(WorkspaceAction::RemoveRepoModeEntry(path.to_path_buf()))
            .into_item()];
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
        let mut items = vec![MenuItemFields::new("All")
            .with_on_select_action(WorkspaceAction::SelectRepoModeAll)
            .into_item()];
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

    /// Live partition of tabs across registry entries for display: a tab
    /// belongs to the entry whose path is the deepest ancestor of its focused
    /// terminal's local cwd, so a terminal that cd's between repos follows
    /// reality rather than the group it was opened under. While a terminal
    /// tab's cwd is unknown (sessions still bootstrapping after restore), the
    /// bound group's repo root is used as a fallback. Tabs with no terminal at
    /// all (Settings, notebooks, ...) never have a cwd and always land loose,
    /// as do tabs matching no entry. Group membership itself is not mutated.
    pub(super) fn repo_mode_tab_partition(
        &self,
        entry_paths: &[PathBuf],
        app: &AppContext,
    ) -> (HashMap<PathBuf, Vec<usize>>, Vec<usize>) {
        let mut by_entry: HashMap<PathBuf, Vec<usize>> = HashMap::new();
        let mut loose = Vec::new();
        for (index, tab) in self.tabs.iter().enumerate() {
            let pane_group = tab.pane_group.as_ref(app);
            let terminal_views = pane_group.terminal_views(app);
            let cwd = pane_group
                .active_session_view(app)
                .and_then(|tv| tv.as_ref(app).pwd_if_local(app))
                .or_else(|| {
                    terminal_views
                        .iter()
                        .find_map(|tv| tv.as_ref(app).pwd_if_local(app))
                })
                .map(PathBuf::from);
            let owner = match cwd {
                Some(cwd) => entry_paths
                    .iter()
                    .filter(|p| cwd.starts_with(p))
                    .max_by_key(|p| p.as_os_str().len())
                    .cloned(),
                // No terminal — the tab can never report a cwd, so the group
                // fallback (meant for booting sessions) does not apply.
                None if terminal_views.is_empty() => None,
                None => tab
                    .group_id
                    .and_then(|gid| self.tab_groups.get(&gid))
                    .and_then(|g| g.repo_root.as_deref())
                    .map(PathBuf::from)
                    .filter(|root| entry_paths.iter().any(|p| p == root)),
            };
            match owner {
                Some(path) => by_entry.entry(path).or_default().push(index),
                None => loose.push(index),
            }
        }
        (by_entry, loose)
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
    /// selection / flag off), by the live cwd partition. A selected entry with
    /// no matching tabs yields an empty list — never unrelated tabs (R10).
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
        let (mut by_entry, _) = self.repo_mode_tab_partition(&entry_paths, app);
        Some(by_entry.remove(&selected).unwrap_or_default())
    }
}

#[cfg(test)]
#[path = "repo_mode_model_tests.rs"]
mod tests;
