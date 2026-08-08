//! Repositories tree UI for the vertical tabs panel (repo mode).
//!
//! Renders a unified tree: one row per registry entry, with the selected
//! repo's tabs nested directly beneath its row (accordion — selection is
//! expansion). Below a divider, an "Other tabs" section lists tabs not tied to
//! any registry entry and offers a "+ New" button that opens a plain terminal
//! detached from every repo.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash as _, Hasher as _};
use std::path::{Path, PathBuf};
use std::time::Duration;

use instant::Instant;
use pathfinder_color::ColorU;
use pathfinder_geometry::vector::{Vector2F, vec2f};
use repo_mode::{RemoteProbeFailure, RemoteProbeState, RepoEntryKind};
use settings::Setting;
use siphasher::sip::SipHasher;
use warp_core::ui::Icon as WarpIcon;
use warp_core::ui::theme::color::internal_colors;
use warp_core::ui::theme::{AnsiColorIdentifier, Fill as ThemeFill};
use warpui::elements::{
    ChildAnchor, ConstrainedBox, Container, CrossAxisAlignment, DragAxis, Draggable,
    DraggableState, Element, Empty, Expanded, Flex, Hoverable, MainAxisAlignment, MainAxisSize,
    MouseStateHandle, OffsetPositioning, Padding, ParentAnchor, ParentElement, ParentOffsetBounds,
    SavePosition, Shrinkable, Stack, Text,
};
use warpui::ui_components::components::UiComponent;
use warpui::{AppContext, SingletonEntity};

use super::Workspace;
use super::repo_mode_model::{
    RemoteListEntry, RepoModeEntryBadges, RepoModeListEntry, RepoRowActivity, fs_probe_ttl,
};
use super::vertical_tabs::telemetry::VerticalTabsChipEntrypoint;
use super::vertical_tabs::{
    METADATA_ROW_HEIGHT, VerticalTabsPanelState, render_git_branch_text, render_groups,
    render_passive_terminal_diff_stats_badge, render_row_title_line,
    render_terminal_pull_request_badge, shows_synced_inputs_indicator,
    terminal_pull_request_badge_label,
};
use crate::appearance::Appearance;
use crate::workspace::tab_settings::TabSettings;
use crate::workspace::{RepoRegistryKey, WorkspaceAction};

const ROW_ICON_SIZE: f32 = 14.;
const CHEVRON_SIZE: f32 = 10.;
const NESTED_TABS_INDENT: f32 = 10.;
/// Left inset of the branch/badges line, aligning it with the name text.
const META_LINE_INDENT: f32 = CHEVRON_SIZE + ROW_ICON_SIZE + 12.;
const BRANCH_CACHE_TTL: Duration = Duration::from_secs(5);

/// Cached branch reads keyed by repo root: when the entry was refreshed, the
/// refresh interval that read earned itself, and the branch it produced
/// (`None` for a root that has no readable `.git/HEAD`).
type BranchCache = RefCell<HashMap<String, (Instant, Duration, Option<String>)>>;

/// Per-row mouse state for the Repositories tree.
#[derive(Clone, Default)]
pub(super) struct RepoSidebarState {
    pub add_button: MouseStateHandle,
    /// Hover state for the "+ New" button on the "Other tabs" section header.
    pub new_terminal_button: MouseStateHandle,
    pub entry_rows: RefCell<HashMap<String, MouseStateHandle>>,
    /// Per-row drag state, keyed by registry path like every other map here.
    ///
    /// Also what R15 reads: the selected repository's tab block is hidden while
    /// any row here reports a drag. `DraggableState` returns to not-dragging on
    /// mouse-up on every path, so deriving visibility from the map means no
    /// terminal path can leave those tabs invisible (KTD11).
    pub entry_drags: RefCell<HashMap<String, DraggableState>>,
    /// Hover state for the clickable PR badge on each repo row.
    pub pr_badges: RefCell<HashMap<String, MouseStateHandle>>,
    /// Hover state for the "Remove" button on each dead repo row.
    pub remove_buttons: RefCell<HashMap<String, MouseStateHandle>>,
    /// Cached branch per repo root. Each entry carries its own refresh
    /// interval: `BRANCH_CACHE_TTL` normally, backed off to
    /// `SLOW_MOUNT_CACHE_TTL` when the read that produced it was slow enough to
    /// mean a stalled mount.
    pub branch_cache: BranchCache,
}

impl RepoSidebarState {
    /// Drops per-entry state for keys that are no longer in the registry.
    ///
    /// These maps are keyed by registry path and only ever grew: removing an
    /// entry, or renaming the directory behind one, left its mouse states and
    /// cached branch behind for the lifetime of the window. The sibling caches
    /// on `Workspace` (`repo_mode_fs_cache`, `repo_mode_remote_probes`) are
    /// already pruned this way.
    fn prune_to(&self, live_keys: &HashSet<String>) {
        self.entry_rows
            .borrow_mut()
            .retain(|key, _| live_keys.contains(key));
        self.entry_drags
            .borrow_mut()
            .retain(|key, _| live_keys.contains(key));
        self.pr_badges
            .borrow_mut()
            .retain(|key, _| live_keys.contains(key));
        self.remove_buttons
            .borrow_mut()
            .retain(|key, _| live_keys.contains(key));
        self.branch_cache
            .borrow_mut()
            .retain(|key, _| live_keys.contains(key));
    }

    /// Interaction state for the row keyed by `key`, created on first use.
    ///
    /// Exactly one handle per row, and everything that touches that row's mouse
    /// interaction has to come through here. The row body records a press on it
    /// and the row's drag start clears it; two handles for one row would leave
    /// that press to complete into a click on release and select the repository
    /// the user just dragged (R13).
    fn entry_row_mouse(&self, key: &str) -> MouseStateHandle {
        self.entry_rows
            .borrow_mut()
            .entry(key.to_string())
            .or_default()
            .clone()
    }

    /// Whether any repository row is mid-drag (R15/KTD11).
    fn any_entry_drag_active(&self) -> bool {
        self.entry_drags
            .borrow()
            .values()
            .any(|drag| drag.is_dragging())
    }
}

/// Save-position id for a repository row's full rect, read by the drag to
/// resolve its neighbours' positions and its own laid-out slot.
///
/// Keyed by registry path (KTD4) rather than list index: the index-keyed tab
/// path carries a documented staleness hazard it has to rescan to recover
/// from, and repo mode already uses the path as identity everywhere else.
///
/// The key is hashed rather than spelled out. A remote registry key is a whole
/// SSH connection string — user, host, port, remote path, and the local path to
/// a private key — and an element id is a plain string any diagnostic dump is
/// free to print, so the id must not be the key. The hasher is fixed-key rather
/// than randomly seeded because the drag's writer and every one of its readers
/// resolve their ids through this function: agreement within one process run is
/// the whole requirement, and a per-run `RandomState` would not even give that.
pub(super) fn repo_row_position_id(path: &Path) -> String {
    let mut hasher = SipHasher::new_with_keys(0, 0);
    path.hash(&mut hasher);
    format!("repo_mode:row:{:016x}", hasher.finish())
}

/// Whether a repository row can be picked up.
///
/// R14: a row whose key is still provisional is not draggable, because an order
/// written against it would name a repository that is about to stop existing. A
/// dead row is draggable on the same terms as any other (R12): its key is
/// settled, only its path is unreachable.
///
/// The test is [`RepoModeListEntry::unverified`] and deliberately not the probe
/// state — see that field for why the two are different questions.
pub(super) fn repo_row_is_draggable(entry: &RepoModeListEntry) -> bool {
    !entry.unverified
}

/// Whether the selected repository's tab block renders this frame.
///
/// R15: it folds away for the duration of any repository-row drag, so every row
/// in the list is the same height and the drag's midpoint rule applies
/// unchanged. With the block in place two consecutive rows are separated by an
/// arbitrarily tall region and the dragged row reads as stuck until the cursor
/// has crossed all of it.
pub(super) fn repo_tab_block_visible(is_selected: bool, any_entry_drag_active: bool) -> bool {
    is_selected && !any_entry_drag_active
}

/// Which rolled-up indicators a repository row renders.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct RepoRowIndicators {
    pub synced: bool,
    pub unread: bool,
}

/// Rollup gating for one repository row.
///
/// Keyed on `is_selected` rather than `repo_tab_block_visible`: that predicate
/// also folds every block away for the duration of a repository drag, so
/// keying on it would light indicators up on every row mid-gesture, changing
/// row content while the user is dragging. `is_selected` is the expansion
/// state alone — an expanded row's nested rows carry their own indicators, so
/// it needs no rollup.
///
/// Dead rows show nothing: they keep the row shape they have today, with the
/// trailing edge owned by their "Remove" button.
///
/// The link icon follows the tab-indicator setting because
/// `shows_synced_inputs_indicator` does; the unread dot does not, because
/// `render_row_title_line`'s other callers pass `has_unread_activity` ungated.
/// The rollup mirrors the nested rows rather than inventing its own rule.
pub(super) fn repo_row_indicators(
    is_dead: bool,
    is_selected: bool,
    show_indicators: bool,
    activity: RepoRowActivity,
) -> RepoRowIndicators {
    if is_dead || is_selected {
        return RepoRowIndicators::default();
    }
    RepoRowIndicators {
        // A repository row stands in for terminal rows, which is what the
        // helper's `is_terminal_row` gate is asking about: every tab the
        // rollup walks reaches it through a repo-mode tab group.
        synced: shows_synced_inputs_indicator(true, activity.any_synced, show_indicators),
        unread: activity.any_unread,
    }
}

/// Fixed "Repositories" header (with + Add) rendered above the scrolling tree.
pub(super) fn render_repo_header(state: &RepoSidebarState, app: &AppContext) -> Box<dyn Element> {
    let appearance = Appearance::as_ref(app);
    let header = Flex::row()
        .with_main_axis_size(MainAxisSize::Max)
        .with_main_axis_alignment(MainAxisAlignment::SpaceBetween)
        .with_cross_axis_alignment(CrossAxisAlignment::Center)
        .with_child(
            Text::new("Repositories", appearance.ui_font_family(), 11.)
                .with_color(
                    appearance
                        .theme()
                        .sub_text_color(appearance.theme().background())
                        .into(),
                )
                .finish(),
        )
        .with_child(render_header_button(
            "+ Add",
            HeaderButtonRole::Accent,
            state.add_button.clone(),
            // R1: one control, two destinations — the menu opens at the click.
            |position| WorkspaceAction::ToggleRepoModeAddMenu { position },
            appearance,
        ))
        .finish();
    Container::new(header)
        .with_padding(Padding::uniform(8.).with_bottom(4.))
        .finish()
}

/// "Other tabs" section header with a "+ New" button that opens a plain
/// terminal detached from every repo entry.
fn render_other_tabs_header(
    mouse: MouseStateHandle,
    app_appearance: &Appearance,
) -> Box<dyn Element> {
    let theme = app_appearance.theme();
    let header = Flex::row()
        .with_main_axis_size(MainAxisSize::Max)
        .with_main_axis_alignment(MainAxisAlignment::SpaceBetween)
        .with_cross_axis_alignment(CrossAxisAlignment::Center)
        .with_child(
            Text::new("Other tabs", app_appearance.ui_font_family(), 11.)
                .with_color(theme.sub_text_color(theme.background()).into())
                .finish(),
        )
        .with_child(render_header_button(
            "+ New",
            HeaderButtonRole::Accent,
            mouse,
            |_| WorkspaceAction::NewRepoModeLooseTab,
            app_appearance,
        ))
        .finish();
    Container::new(header)
        .with_padding(Padding::uniform(8.).with_top(0.).with_bottom(4.))
        .finish()
}

/// Scrollable tree body: repo rows, the selected repo's tabs nested under its
/// row, then loose (non-repo) tabs below a divider.
pub(super) fn render_repo_tree(
    state: &VerticalTabsPanelState,
    workspace: &Workspace,
    app: &AppContext,
) -> Box<dyn Element> {
    let appearance = Appearance::as_ref(app);
    let sidebar = &state.repo_sidebar;
    let entries = workspace.repo_mode_entries(app);
    let selected = workspace.selected_repo_root.as_deref();
    let show_diff_stats = *TabSettings::as_ref(app)
        .vertical_tabs_show_diff_stats
        .value();
    let show_pr_link = *TabSettings::as_ref(app).vertical_tabs_show_pr_link.value();
    let show_indicators = *TabSettings::as_ref(app).show_indicators.value();

    // Display partition: which repo row each bound tab renders under; loose
    // tabs always land in the "Other tabs" section.
    let entry_paths: Vec<PathBuf> = entries.iter().map(|e| e.path.clone()).collect();
    let (by_entry, loose) = workspace.repo_mode_tab_partition(&entry_paths);

    // Badges come from terminals whose *local* repo path matches an entry, so a
    // remote or dead entry never has any and is left out of the sweep. One sweep
    // for the whole sidebar: asking per row walked every tab and pane again for
    // each entry. With both badge settings off nothing reads the result, so the
    // sweep does not run at all.
    let badge_paths: Vec<PathBuf> = if show_diff_stats || show_pr_link {
        entries
            .iter()
            .filter(|entry| !entry.is_dead && entry.remote.is_none())
            .map(|entry| entry.path.clone())
            .collect()
    } else {
        Vec::new()
    };
    let badges_by_entry = workspace.repo_mode_badges_by_entry(&badge_paths, app);

    let activity_by_entry = workspace.repo_mode_activity_by_entry(&by_entry, app);

    let mut column = Flex::column()
        .with_main_axis_size(MainAxisSize::Min)
        .with_cross_axis_alignment(CrossAxisAlignment::Stretch);

    if entries.is_empty() {
        column = column.with_child(render_empty_state(appearance));
    }

    // Registry keys seen this frame, so per-entry state for removed entries can
    // be dropped rather than accumulating for the window's lifetime.
    let mut live_keys: HashSet<String> = HashSet::new();
    // Read once for the whole tree: every row's tab block answers to the same
    // "is a repository being dragged" question (R15).
    let entry_drag_active = sidebar.any_entry_drag_active();

    for entry in entries {
        let key = entry.path.to_string_lossy().into_owned();
        let mouse = sidebar.entry_row_mouse(&key);
        let pr_badge_mouse = sidebar
            .pr_badges
            .borrow_mut()
            .entry(key.clone())
            .or_default()
            .clone();
        let remove_mouse = sidebar
            .remove_buttons
            .borrow_mut()
            .entry(key.clone())
            .or_default()
            .clone();
        let drag_state = sidebar
            .entry_drags
            .borrow_mut()
            .entry(key.clone())
            .or_default()
            .clone();
        live_keys.insert(key.clone());
        let is_selected = selected == Some(key.as_str());
        let remote_state = entry
            .remote
            .as_ref()
            .map(|remote| remote_row_state(remote, SECONDARY_LABEL_BUDGET));
        let branch = match &remote_state {
            // R11: a remote branch comes from the last probe. Reading
            // `.git/HEAD` here would answer about the local filesystem, which
            // knows nothing about this entry.
            Some(state) => state.branch.clone(),
            None => match entry.kind {
                RepoEntryKind::Repo if !entry.is_dead => {
                    repo_branch(&sidebar.branch_cache, &entry.path)
                }
                _ => None,
            },
        };
        let mut badges = badges_by_entry
            .get(&entry.path)
            .cloned()
            .unwrap_or_default();
        if !show_diff_stats {
            badges.diff_stats = None;
        }
        if !show_pr_link {
            badges.pull_request_url = None;
        }

        let members = repo_tab_block_visible(is_selected, entry_drag_active)
            .then(|| by_entry.get(&entry.path).cloned().unwrap_or_default());

        let is_draggable = repo_row_is_draggable(&entry);
        let path = entry.path.clone();
        // Gated here rather than inside the row, mirroring how the badge
        // settings are folded into `badges` above: the row renders what it is
        // handed.
        let indicators = repo_row_indicators(
            entry.is_dead,
            is_selected,
            show_indicators,
            activity_by_entry
                .get(&entry.path)
                .copied()
                .unwrap_or_default(),
        );
        let row = render_entry_row(
            entry,
            remote_state,
            branch,
            badges,
            indicators,
            mouse,
            drag_state.clone(),
            pr_badge_mouse,
            remove_mouse,
            is_selected,
            appearance,
        );
        column = column.with_child(wrap_entry_row_for_drag(
            row,
            &path,
            is_draggable,
            drag_state,
            sidebar,
            appearance,
        ));

        // Accordion: the selected repo's tabs render right under its row,
        // flattened (the row already names the context) and lightly indented.
        if let Some(members) = members {
            column = column.with_child(
                Container::new(render_groups(
                    state,
                    workspace,
                    Some(members),
                    true,
                    true,
                    app,
                ))
                .with_padding(Padding::uniform(0.).with_left(NESTED_TABS_INDENT))
                .finish(),
            );
        }
    }

    sidebar.prune_to(&live_keys);

    // "Other tabs" section: loose tabs (cwd outside every registry entry) stay
    // visible below the tree regardless of selection, as plain terminal rows —
    // repo-bound group chrome is stripped (a repo group's tab that cd'd away
    // still carries its group binding), while user-created groups keep theirs.
    // The header is always shown so its "+ New" button remains the way to open
    // a terminal detached from every repo.
    column = column.with_child(render_divider(appearance));
    column = column.with_child(render_other_tabs_header(
        sidebar.new_terminal_button.clone(),
        appearance,
    ));
    if !loose.is_empty() {
        column = column.with_child(render_groups(
            state,
            workspace,
            Some(loose),
            true,
            false,
            app,
        ));
    }

    Container::new(column.finish())
        .with_padding(Padding::uniform(0.).with_bottom(4.))
        .finish()
}

fn render_divider(app_appearance: &Appearance) -> Box<dyn Element> {
    let theme = app_appearance.theme();
    Container::new(
        ConstrainedBox::new(
            Container::new(Empty::new().finish())
                .with_background(internal_colors::fg_overlay_2(theme))
                .finish(),
        )
        .with_height(1.)
        .finish(),
    )
    .with_padding(
        Padding::uniform(0.)
            .with_top(4.)
            .with_bottom(4.)
            .with_left(8.)
            .with_right(8.),
    )
    .finish()
}

/// Which color role a header button takes.
enum HeaderButtonRole {
    /// Additive actions — the affirmative accent.
    Accent,
    /// Permanently deletes something. Mirrors `DangerNakedTheme` so a
    /// destructive control never wears the same color as an additive one.
    Danger,
}

/// Small text button used on section headers.
///
/// `action` receives the click position so a button can open a menu anchored
/// where the user clicked, not just fire a fixed action.
fn render_header_button(
    label: &'static str,
    role: HeaderButtonRole,
    mouse: MouseStateHandle,
    action: impl Fn(Vector2F) -> WorkspaceAction + 'static,
    app_appearance: &Appearance,
) -> Box<dyn Element> {
    let theme = app_appearance.theme();
    // Kept in sync with `NakedTheme`/`DangerNakedTheme` in `action_button.rs`.
    let (text_color, hover_background) = match role {
        HeaderButtonRole::Accent => (theme.accent().into(), internal_colors::fg_overlay_2(theme)),
        HeaderButtonRole::Danger => (
            theme.ansi_fg_red(),
            ThemeFill::Solid(theme.ansi_overlay_1(
                AnsiColorIdentifier::Red.to_ansi_color(&theme.terminal_colors().normal),
            )),
        ),
    };
    let font = app_appearance.ui_font_family();
    Hoverable::new(mouse, move |hover| {
        let background = if hover.is_hovered() {
            hover_background
        } else {
            ThemeFill::Solid(ColorU::transparent_black())
        };
        Container::new(Text::new(label, font, 11.).with_color(text_color).finish())
            .with_padding(Padding::uniform(4.))
            .with_background(background)
            .finish()
    })
    .on_click(move |ctx, _, position| {
        ctx.dispatch_typed_action(action(position));
    })
    .finish()
}

fn render_empty_state(app_appearance: &Appearance) -> Box<dyn Element> {
    let theme = app_appearance.theme();
    Container::new(
        Text::new(
            "No repositories yet — add one with + Add",
            app_appearance.ui_font_family(),
            11.,
        )
        .with_color(theme.sub_text_color(theme.background()).into())
        .finish(),
    )
    .with_padding(Padding::uniform(8.).with_top(4.).with_bottom(4.))
    .finish()
}

/// The tab path's wrapping order, minus the drop target (KTD10): the row's
/// `SavePosition` outermost, then the placeholder that fills the hole the drag
/// overlay leaves behind, then the `Draggable` itself.
///
/// `SavePosition` stays outside the `Draggable` so it keeps publishing the
/// row's laid-out *slot* while the overlay follows the cursor — that slot is
/// what the neighbour lookup and the anchor correction both read. It is
/// per-frame (KTD8b) because the indefinite cache is never cleared, so a row
/// that stops being painted mid-drag would go on answering the lookup at a rect
/// nothing occupies.
///
/// A row whose key the registry has not accepted yet — `unverified`, which is
/// none of the other things "pending" names in this subsystem: not
/// `RemoteProbeState::Pending`, and not `repo_mode_pending_remote_key` — skips
/// only the `Draggable` (R14). It keeps its `SavePosition`: a row with no
/// published rect is invisible to the neighbour lookup, so an unwrapped one
/// would clamp every drag that has to cross it.
///
/// Repository rows get no `DropTarget` at all. Nothing here reads drop-target
/// data — the swap is resolved from the row rect — and reusing the vertical
/// tabs' pane data to fill the parameter would make every repository row a
/// valid pane-header drop target.
///
/// The row body's own interaction state is cleared the moment the drag starts,
/// and that — not `repo_row_click_action`'s `is_dragging` parameter — is what
/// makes R13 hold. `Draggable` stores `DragState::None` before any click can be
/// delivered, so the row's click handler can never observe "a drag is in
/// flight"; the press the row recorded at mouse-down would otherwise complete
/// into a click on release and select the repository the user just moved.
/// Resetting the state is the same remedy the vertical tabs sidecar uses when an
/// element stops receiving its own follow-up mouse events.
///
/// The state is looked up here rather than passed in so it is necessarily the
/// handle the row body records its press on. That much is asserted; the reset
/// itself is not, and cannot be from a unit test — `MouseState` offers no way to
/// arm interaction state from outside `warpui_core`, and `EventContext` has no
/// public constructor, so there is no way to press a row and watch the press go
/// away.
fn wrap_entry_row_for_drag(
    row: Box<dyn Element>,
    path: &Path,
    is_draggable: bool,
    drag_state: DraggableState,
    sidebar: &RepoSidebarState,
    app_appearance: &Appearance,
) -> Box<dyn Element> {
    let mouse = sidebar.entry_row_mouse(&path.to_string_lossy());
    let row = if is_draggable {
        let start_path = path.to_path_buf();
        let drag_path = path.to_path_buf();
        let drop_path = path.to_path_buf();
        Draggable::new(drag_state.clone(), row)
            .with_drag_axis(DragAxis::VerticalOnly)
            .on_drag_start(move |ctx, _, row_position| {
                if let Ok(mut mouse_state) = mouse.lock() {
                    mouse_state.reset_interaction_state();
                }
                ctx.dispatch_typed_action(WorkspaceAction::StartRepoModeEntryDrag {
                    path: RepoRegistryKey(start_path.clone()),
                    row_position,
                });
            })
            .on_drag(move |ctx, _, row_position, _| {
                ctx.dispatch_typed_action(WorkspaceAction::DragRepoModeEntry {
                    path: RepoRegistryKey(drag_path.clone()),
                    row_position,
                });
            })
            .on_drop(move |ctx, _, _, _| {
                ctx.dispatch_typed_action(WorkspaceAction::DropRepoModeEntry(RepoRegistryKey(
                    drop_path.clone(),
                )));
            })
            .finish()
    } else {
        row
    };

    let row = if drag_state.is_dragging() {
        Container::new(row)
            .with_background(internal_colors::fg_overlay_1(app_appearance.theme()))
            .finish()
    } else {
        row
    };

    SavePosition::new(row, &repo_row_position_id(path))
        .for_single_frame()
        .finish()
}

/// What a left-click on a repo row's body does, or `None` for no action.
///
/// A pure function so the two properties that matter can be asserted directly.
///
/// **The row body is never destructive.** A dead row dispatches nothing, and
/// removal is reachable only through the row's own "Remove" button. Dispatching
/// `RemoveRepoModeEntry` from the body would make one unmodified left-click drop
/// a registry entry with no confirmation and no undo — and a repo on a briefly
/// unavailable network mount reads as dead, so that click would land on healthy
/// repositories.
///
/// **A drag never selects the repository it moves (R13).** Selecting spawns a
/// terminal, and for a remote entry an SSH session, so a row that crossed the
/// drag threshold must dispatch nothing. What holds that at runtime is the
/// mouse-state reset in [`wrap_entry_row_for_drag`], which clears the row body's
/// recorded press the moment a drag starts;
/// `a_repository_rows_body_and_its_drag_share_one_interaction_state` is what
/// fails if the two sides stop addressing the same handle. `is_dragging` is
/// never `true` here — `Draggable` suppresses every child event for the life of
/// a drag and has stored `DragState::None` by the time a click could be
/// delivered — so the parameter is the rule written down as a total function:
/// the answer for a dragging row is pinned by a test rather than left to depend
/// on that reset never regressing.
fn repo_row_click_action(
    is_dragging: bool,
    is_dead: bool,
    is_selected: bool,
    path: &Path,
) -> Option<WorkspaceAction> {
    if is_dragging {
        None
    } else if is_dead {
        // A dead entry cannot be opened, and must not be removed from here.
        // Right-click still opens the row's context menu.
        None
    } else if is_selected {
        // Clicking the expanded repo collapses it (deselect).
        Some(WorkspaceAction::SelectRepoModeAll)
    } else {
        Some(WorkspaceAction::SelectRepoModeEntry(RepoRegistryKey(
            path.to_path_buf(),
        )))
    }
}

#[allow(clippy::too_many_arguments)]
fn render_entry_row(
    entry: RepoModeListEntry,
    remote_state: Option<RemoteRowState>,
    branch: Option<String>,
    badges: RepoModeEntryBadges,
    indicators: RepoRowIndicators,
    mouse: MouseStateHandle,
    drag_state: DraggableState,
    pr_badge_mouse: MouseStateHandle,
    remove_mouse: MouseStateHandle,
    is_selected: bool,
    app_appearance: &Appearance,
) -> Box<dyn Element> {
    let theme = app_appearance.theme();
    let font = app_appearance.ui_font_family();
    let ui_builder = app_appearance.ui_builder().clone();
    let unreachable = matches!(
        remote_state.as_ref().map(|state| state.status),
        Some(RemoteRowStatus::Unreachable(_))
    );
    let primary_color = if entry.is_dead || unreachable {
        theme.sub_text_color(theme.background())
    } else {
        theme.font_color(theme.background())
    };
    let sub_color = theme.sub_text_color(theme.background());

    let full_path = entry.path.to_string_lossy().into_owned();
    // R10: a remote row names its machine where a local row shows its path,
    // and its hover text carries the untruncated value plus any probe reason.
    let (secondary, hover_text) = match &remote_state {
        Some(state) => (state.secondary.clone(), state.tooltip.clone()),
        None => {
            let home_dir = std::env::var("HOME").ok();
            (
                warp_util::path::user_friendly_path(&full_path, home_dir.as_deref()).into_owned(),
                full_path.clone(),
            )
        }
    };
    let labels = row_labels(&entry.display_name, secondary, hover_text);

    // R10: the cloud icon is what distinguishes a remote row at a glance, and
    // it goes offline when the last probe failed.
    let icon = match remote_state.as_ref().map(|state| state.status) {
        Some(RemoteRowStatus::Unreachable(_)) => WarpIcon::CloudOffline,
        Some(_) => WarpIcon::Cloud,
        None => match entry.kind {
            RepoEntryKind::Repo => WarpIcon::GitBranch,
            RepoEntryKind::Folder => WarpIcon::Folder,
        },
    };
    // Short inline note for the states a branch cannot describe (R9/R11); the
    // full reason lives in the tooltip.
    let remote_note = match remote_state.as_ref().map(|state| state.status) {
        Some(RemoteRowStatus::Pending) => Some("Connecting…"),
        Some(RemoteRowStatus::Unreachable(reason)) => Some(reason.short_label()),
        _ => None,
    };

    let is_dead = entry.is_dead;
    let RowLabels {
        name: display_name,
        secondary,
        hover: hover_text,
    } = labels;
    let path = entry.path.clone();
    let path_for_menu = path.clone();
    let remove_path = path.clone();

    Hoverable::new(mouse, move |hover| {
        let background = if is_selected {
            internal_colors::fg_overlay_3(theme)
        } else if hover.is_hovered() {
            internal_colors::fg_overlay_1(theme)
        } else {
            ThemeFill::Solid(ColorU::transparent_black())
        };

        let icon_element = ConstrainedBox::new(
            icon.to_warpui_icon(if is_dead { sub_color } else { primary_color })
                .finish(),
        )
        .with_width(ROW_ICON_SIZE)
        .with_height(ROW_ICON_SIZE)
        .finish();

        let text_column = Flex::column()
            .with_main_axis_size(MainAxisSize::Min)
            .with_cross_axis_alignment(CrossAxisAlignment::Start)
            .with_child(
                Text::new(display_name.clone(), font, 12.)
                    .with_color(primary_color.into())
                    .finish(),
            )
            .with_child(
                Text::new(secondary.clone(), font, 10.)
                    .with_color(sub_color.into())
                    .finish(),
            )
            .finish();

        let mut leading = Flex::row()
            .with_main_axis_size(MainAxisSize::Min)
            .with_cross_axis_alignment(CrossAxisAlignment::Center)
            .with_spacing(6.);
        // Expansion affordance: live rows toggle open/closed on click.
        if !is_dead {
            let chevron = if is_selected {
                WarpIcon::ChevronDown
            } else {
                WarpIcon::ChevronRight
            };
            leading.add_child(
                ConstrainedBox::new(chevron.to_warpui_icon(sub_color).finish())
                    .with_width(CHEVRON_SIZE)
                    .with_height(CHEVRON_SIZE)
                    .finish(),
            );
        }
        leading.add_child(icon_element);
        leading.add_child(text_column);

        // Dead rows keep the shape they have today: their trailing edge belongs
        // to "Remove".
        let title_line = if is_dead {
            leading.finish()
        } else {
            // `render_row_title_line` lays its indicators out with
            // `SpaceBetween`, which only separates anything if the line owns
            // the row's full width.
            Expanded::new(
                1.,
                render_row_title_line(
                    leading.finish(),
                    indicators.synced,
                    indicators.unread,
                    theme,
                ),
            )
            .finish()
        };

        let mut top_row = Flex::row()
            .with_main_axis_size(MainAxisSize::Max)
            .with_cross_axis_alignment(CrossAxisAlignment::Center)
            .with_spacing(6.)
            .with_child(title_line);

        if is_dead {
            top_row.add_child(
                Expanded::new(
                    1.,
                    Flex::row().with_main_axis_size(MainAxisSize::Max).finish(),
                )
                .finish(),
            );
            // "Remove" is its own hit target, and the only way to delete a
            // registry entry from the sidebar. Deletion has no confirmation and
            // no undo, so it must not be reachable from anywhere a click can
            // land by accident — and a repo on a briefly unavailable network
            // mount reads as dead, so the rows this button appears on are not
            // reliably rows the user has finished with.
            top_row.add_child(render_header_button(
                "Remove",
                HeaderButtonRole::Danger,
                remove_mouse.clone(),
                {
                    let path = remove_path.clone();
                    move |_| WorkspaceAction::RemoveRepoModeEntry(RepoRegistryKey(path.clone()))
                },
                app_appearance,
            ));
        }

        let mut lines = Flex::column()
            .with_main_axis_size(MainAxisSize::Min)
            .with_cross_axis_alignment(CrossAxisAlignment::Stretch)
            .with_spacing(2.)
            .with_child(top_row.finish());

        // Second line, mirroring the terminal rows' branch line: branch on the
        // left (ellipsized first when tight), diff stats + PR chip on the
        // right. Indented to align with the name text above.
        if !is_dead {
            let mut meta_badges = Flex::row()
                .with_cross_axis_alignment(CrossAxisAlignment::Center)
                .with_spacing(4.);
            let mut has_badges = false;
            if let Some(diff_stats) = badges.diff_stats.as_ref() {
                meta_badges.add_child(render_passive_terminal_diff_stats_badge(
                    diff_stats,
                    app_appearance,
                ));
                has_badges = true;
            }
            if let Some(pull_request_url) = badges.pull_request_url.clone() {
                let label = terminal_pull_request_badge_label(&pull_request_url);
                meta_badges.add_child(render_terminal_pull_request_badge(
                    label,
                    pull_request_url,
                    VerticalTabsChipEntrypoint::RepoSidebar,
                    pr_badge_mouse.clone(),
                    app_appearance,
                ));
                has_badges = true;
            }

            if branch.is_some() || remote_note.is_some() || has_badges {
                // A remote row that has no branch to show still owes the user a
                // word about why (R9/R11); a local row leaves the slot empty.
                let branch_element: Box<dyn Element> = match (branch.clone(), remote_note) {
                    (Some(branch), _) => {
                        render_git_branch_text(&branch, sub_color, 10., app_appearance)
                    }
                    (None, Some(note)) => Text::new(note, font, 10.)
                        .with_color(sub_color.into())
                        .finish(),
                    (None, None) => Empty::new().finish(),
                };
                let mut meta = Flex::row()
                    .with_main_axis_size(MainAxisSize::Max)
                    .with_main_axis_alignment(MainAxisAlignment::SpaceBetween)
                    .with_cross_axis_alignment(CrossAxisAlignment::Center)
                    .with_child(Shrinkable::new(1., branch_element).finish());
                if has_badges {
                    meta.add_child(
                        Container::new(meta_badges.finish())
                            .with_padding_left(4.)
                            .finish(),
                    );
                }
                lines.add_child(
                    Container::new(
                        ConstrainedBox::new(meta.finish())
                            .with_height(METADATA_ROW_HEIGHT)
                            .finish(),
                    )
                    .with_padding(Padding::uniform(0.).with_left(META_LINE_INDENT))
                    .finish(),
                );
            }
        }

        let row_container = Container::new(lines.finish())
            .with_padding(Padding::uniform(8.).with_top(4.).with_bottom(4.))
            .with_background(background)
            .finish();

        // Hover carries whatever the row had to shorten or omit: the full local
        // path, or the untruncated host plus the probe reason.
        if hover.is_hovered() && hover_text != secondary {
            let tooltip = ui_builder.tool_tip(hover_text.clone()).build().finish();
            let mut stack = Stack::new().with_child(row_container);
            stack.add_positioned_overlay_child(
                tooltip,
                OffsetPositioning::offset_from_parent(
                    vec2f(0., 4.),
                    ParentOffsetBounds::WindowByPosition,
                    ParentAnchor::BottomMiddle,
                    ChildAnchor::TopMiddle,
                ),
            );
            stack.finish()
        } else {
            row_container
        }
    })
    .on_click(move |ctx, _, _| {
        if let Some(action) =
            repo_row_click_action(drag_state.is_dragging(), is_dead, is_selected, &path)
        {
            ctx.dispatch_typed_action(action);
        }
    })
    .on_right_click(move |ctx, _, position| {
        ctx.dispatch_typed_action(WorkspaceAction::ToggleRepoModeEntryMenu {
            path: RepoRegistryKey(path_for_menu.clone()),
            position,
        });
    })
    .finish()
}

/// Character budget for the secondary line — `user@host` on a remote row, the
/// path on a local one — before it is ellipsized. The sidebar is narrow, and a
/// long host, user, or path would otherwise push the row wider than the panel.
const SECONDARY_LABEL_BUDGET: usize = 28;

/// Character budget for the primary name line. Smaller than the secondary
/// budget because the name is set two points larger, so fewer characters fit
/// across the same panel width.
const NAME_BUDGET: usize = 23;

/// What a remote row is doing, from its last probe alone (R11 — nothing
/// rechecks in the background).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum RemoteRowStatus {
    /// Registered but never probed, or a probe is in flight (R9).
    Pending,
    Ready,
    Unreachable(RemoteProbeFailure),
}

/// Everything a remote row renders that a local row does not.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct RemoteRowState {
    /// Secondary line under the name: `user@host`, ellipsized to fit (R10).
    pub secondary: String,
    /// Branch for the meta line, from the probe rather than `.git/HEAD` (R8).
    pub branch: Option<String>,
    pub status: RemoteRowStatus,
    /// Hover text: the full `user@host`, plus the mapped reason when the last
    /// probe failed, so a dimmed row is diagnosable without reopening the form.
    pub tooltip: String,
}

pub(super) fn remote_row_state(remote: &RemoteListEntry, budget: usize) -> RemoteRowState {
    let full = remote.target.user_host();
    let (branch, status) = match &remote.probe {
        RemoteProbeState::Pending => (None, RemoteRowStatus::Pending),
        RemoteProbeState::Resolved { kind, branch } => {
            let branch = match kind {
                RepoEntryKind::Repo => branch.clone(),
                RepoEntryKind::Folder => None,
            };
            (branch, RemoteRowStatus::Ready)
        }
        RemoteProbeState::Failed { reason } => (None, RemoteRowStatus::Unreachable(*reason)),
    };
    let tooltip = match status {
        RemoteRowStatus::Pending => format!("{full} — connecting…"),
        RemoteRowStatus::Ready => full.clone(),
        RemoteRowStatus::Unreachable(reason) => format!("{full} — {}", reason.message()),
    };
    RemoteRowState {
        secondary: truncate_label(&full, budget),
        branch,
        status,
        tooltip,
    }
}

/// The three strings a repo row shows, after clipping.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct RowLabels {
    pub name: String,
    pub secondary: String,
    /// Hover text, which must still carry whatever clipping removed.
    pub hover: String,
}

/// Clip a row's name and secondary line to the panel width, keeping the clipped
/// text recoverable on hover.
///
/// Every string on the row goes through here, not just the remote `user@host`.
/// The sidebar is fixed-width and does not scroll horizontally, so anything left
/// unclipped — a long repo name, a deep local path — is cut off by the panel
/// edge with nothing to say it had been.
///
/// The secondary line is idempotent under this: a remote row arrives already
/// clipped to the same budget by `remote_row_state`, so re-clipping it is a
/// no-op rather than a second ellipsis.
pub(super) fn row_labels(display_name: &str, secondary: String, hover: String) -> RowLabels {
    let name = truncate_label(display_name, NAME_BUDGET);
    // A clipped name has to stay recoverable somewhere. A local row's hover text
    // is its full path, which already ends in the name; a remote row's is
    // `user@host`, which does not.
    let hover = if name != display_name && !hover.contains(display_name) {
        format!("{display_name} — {hover}")
    } else {
        hover
    };
    RowLabels {
        name,
        secondary: truncate_label(&secondary, SECONDARY_LABEL_BUDGET),
        hover,
    }
}

/// Middle-ellipsize `label` to `budget` characters. Both ends stay readable
/// because the user at the front and the host's tail at the back are what
/// identify the machine. Returns the label unchanged when it already fits, or
/// when the budget is too small to ellipsize into anything readable.
pub(super) fn truncate_label(label: &str, budget: usize) -> String {
    let chars: Vec<char> = label.chars().collect();
    if chars.len() <= budget || budget < 4 {
        return label.to_string();
    }
    let keep = budget - 1;
    let head = keep.div_ceil(2);
    let tail = keep - head;
    let mut truncated: String = chars[..head].iter().collect();
    truncated.push('…');
    truncated.extend(chars[chars.len() - tail..].iter());
    truncated
}

/// Current branch (or short detached SHA) for a repo root, via `.git/HEAD`.
/// Cheap enough to poll behind a short-lived cache; supports linked worktrees
/// where `.git` is a file pointing at the real git dir.
fn repo_branch(cache: &BranchCache, root: &Path) -> Option<String> {
    let key = root.to_string_lossy().into_owned();
    let now = Instant::now();
    if let Some((refreshed_at, ttl, cached)) = cache.borrow().get(&key)
        && now.duration_since(*refreshed_at) < *ttl
    {
        return cached.clone();
    }
    let branch = read_git_head_branch(root);
    // Stamped from when the read *finished*: on a stalled mount `now` is already
    // the full timeout in the past, so timing the TTL from it would expire the
    // entry the moment it was written and re-read on the very next frame.
    let refreshed_at = Instant::now();
    let ttl = fs_probe_ttl(refreshed_at.duration_since(now), BRANCH_CACHE_TTL);
    cache
        .borrow_mut()
        .insert(key, (refreshed_at, ttl, branch.clone()));
    branch
}

fn read_git_head_branch(root: &Path) -> Option<String> {
    let dot_git = root.join(".git");
    let git_dir = if dot_git.is_file() {
        let content = std::fs::read_to_string(&dot_git).ok()?;
        let gitdir = content.trim().strip_prefix("gitdir: ")?;
        let gitdir = PathBuf::from(gitdir);
        if gitdir.is_absolute() {
            gitdir
        } else {
            root.join(gitdir)
        }
    } else {
        dot_git
    };
    let head = std::fs::read_to_string(git_dir.join("HEAD")).ok()?;
    let head = head.trim();
    if let Some(branch) = head.strip_prefix("ref: refs/heads/") {
        Some(branch.to_string())
    } else {
        Some(head.chars().take(8).collect())
    }
}

#[cfg(test)]
#[path = "repo_sidebar_tests.rs"]
mod tests;
