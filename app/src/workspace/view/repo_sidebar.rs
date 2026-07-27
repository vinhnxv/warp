//! Repositories tree UI for the vertical tabs panel (repo mode).
//!
//! Renders a unified tree: one row per registry entry, with the selected
//! repo's tabs nested directly beneath its row (accordion — selection is
//! expansion). Below a divider, an "Other tabs" section lists tabs not tied to
//! any registry entry and offers a "+ New" button that opens a plain terminal
//! detached from every repo.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use instant::Instant;
use pathfinder_color::ColorU;
use pathfinder_geometry::vector::vec2f;
use repo_mode::RepoEntryKind;
use settings::Setting;
use warp_core::ui::Icon as WarpIcon;
use warp_core::ui::theme::Fill as ThemeFill;
use warp_core::ui::theme::color::internal_colors;
use warpui::elements::{
    ChildAnchor, ConstrainedBox, Container, CrossAxisAlignment, Element, Empty, Expanded, Flex,
    Hoverable, MainAxisAlignment, MainAxisSize, MouseStateHandle, OffsetPositioning, Padding,
    ParentAnchor, ParentElement, ParentOffsetBounds, Shrinkable, Stack, Text,
};
use warpui::ui_components::components::UiComponent;
use warpui::{AppContext, SingletonEntity};

use super::Workspace;
use super::repo_mode_model::{RepoModeEntryBadges, RepoModeListEntry};
use super::vertical_tabs::telemetry::VerticalTabsChipEntrypoint;
use super::vertical_tabs::{
    METADATA_ROW_HEIGHT, VerticalTabsPanelState, render_git_branch_text, render_groups,
    render_passive_terminal_diff_stats_badge, render_terminal_pull_request_badge,
    terminal_pull_request_badge_label,
};
use crate::appearance::Appearance;
use crate::workspace::WorkspaceAction;
use crate::workspace::tab_settings::TabSettings;

const ROW_ICON_SIZE: f32 = 14.;
const CHEVRON_SIZE: f32 = 10.;
const NESTED_TABS_INDENT: f32 = 10.;
/// Left inset of the branch/badges line, aligning it with the name text.
const META_LINE_INDENT: f32 = CHEVRON_SIZE + ROW_ICON_SIZE + 12.;
const BRANCH_CACHE_TTL: Duration = Duration::from_secs(5);

/// Per-row mouse state for the Repositories tree.
#[derive(Clone, Default)]
pub(super) struct RepoSidebarState {
    pub add_button: MouseStateHandle,
    /// Hover state for the "+ New" button on the "Other tabs" section header.
    pub new_terminal_button: MouseStateHandle,
    pub entry_rows: RefCell<HashMap<String, MouseStateHandle>>,
    /// Hover state for the clickable PR badge on each repo row.
    pub pr_badges: RefCell<HashMap<String, MouseStateHandle>>,
    /// Cached branch per repo root, refreshed at most every `BRANCH_CACHE_TTL`.
    pub branch_cache: RefCell<HashMap<String, (Instant, Option<String>)>>,
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
            state.add_button.clone(),
            WorkspaceAction::AddLocalRepositoryOrFolder,
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
            mouse,
            WorkspaceAction::NewRepoModeLooseTab,
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

    // Display partition: which repo row each bound tab renders under; loose
    // tabs always land in the "Other tabs" section.
    let entry_paths: Vec<PathBuf> = entries.iter().map(|e| e.path.clone()).collect();
    let (by_entry, loose) = workspace.repo_mode_tab_partition(&entry_paths, app);

    let mut column = Flex::column()
        .with_main_axis_size(MainAxisSize::Min)
        .with_cross_axis_alignment(CrossAxisAlignment::Stretch);

    if entries.is_empty() {
        column = column.with_child(render_empty_state(appearance));
    }

    for entry in entries {
        let key = entry.path.to_string_lossy().into_owned();
        let mouse = sidebar
            .entry_rows
            .borrow_mut()
            .entry(key.clone())
            .or_default()
            .clone();
        let pr_badge_mouse = sidebar
            .pr_badges
            .borrow_mut()
            .entry(key.clone())
            .or_default()
            .clone();
        let is_selected = selected == Some(key.as_str());
        let branch = match entry.kind {
            RepoEntryKind::Repo if !entry.is_dead => {
                repo_branch(&sidebar.branch_cache, &entry.path)
            }
            _ => None,
        };
        let mut badges = if entry.is_dead {
            RepoModeEntryBadges::default()
        } else {
            workspace.repo_mode_entry_badges(&entry.path, app)
        };
        if !show_diff_stats {
            badges.diff_stats = None;
        }
        if !show_pr_link {
            badges.pull_request_url = None;
        }

        let members = is_selected.then(|| by_entry.get(&entry.path).cloned().unwrap_or_default());

        column = column.with_child(render_entry_row(
            entry,
            branch,
            badges,
            mouse,
            pr_badge_mouse,
            is_selected,
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

/// Small accent text button used on section headers ("+ Add", "+ New").
fn render_header_button(
    label: &'static str,
    mouse: MouseStateHandle,
    action: WorkspaceAction,
    app_appearance: &Appearance,
) -> Box<dyn Element> {
    let theme = app_appearance.theme();
    let accent = theme.accent();
    let font = app_appearance.ui_font_family();
    Hoverable::new(mouse, move |hover| {
        let background = if hover.is_hovered() {
            internal_colors::fg_overlay_2(theme)
        } else {
            ThemeFill::Solid(ColorU::transparent_black())
        };
        Container::new(
            Text::new(label, font, 11.)
                .with_color(accent.into())
                .finish(),
        )
        .with_padding(Padding::uniform(4.))
        .with_background(background)
        .finish()
    })
    .on_click(move |ctx, _, _| {
        ctx.dispatch_typed_action(action.clone());
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

fn render_entry_row(
    entry: RepoModeListEntry,
    branch: Option<String>,
    badges: RepoModeEntryBadges,
    mouse: MouseStateHandle,
    pr_badge_mouse: MouseStateHandle,
    is_selected: bool,
    app_appearance: &Appearance,
) -> Box<dyn Element> {
    let theme = app_appearance.theme();
    let font = app_appearance.ui_font_family();
    let ui_builder = app_appearance.ui_builder().clone();
    let primary_color = if entry.is_dead {
        theme.sub_text_color(theme.background())
    } else {
        theme.font_color(theme.background())
    };
    let sub_color = theme.sub_text_color(theme.background());
    let accent = theme.accent();

    let full_path = entry.path.to_string_lossy().into_owned();
    let home_dir = std::env::var("HOME").ok();
    let short_path =
        warp_util::path::user_friendly_path(&full_path, home_dir.as_deref()).into_owned();

    let icon = match entry.kind {
        RepoEntryKind::Repo => WarpIcon::GitBranch,
        RepoEntryKind::Folder => WarpIcon::Folder,
    };

    let is_dead = entry.is_dead;
    let display_name = entry.display_name.clone();
    let path = entry.path.clone();
    let path_for_menu = path.clone();

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
                Text::new(short_path.clone(), font, 10.)
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

        let mut top_row = Flex::row()
            .with_main_axis_size(MainAxisSize::Max)
            .with_cross_axis_alignment(CrossAxisAlignment::Center)
            .with_spacing(6.)
            .with_child(leading.finish());

        if is_dead {
            top_row.add_child(
                Expanded::new(
                    1.,
                    Flex::row().with_main_axis_size(MainAxisSize::Max).finish(),
                )
                .finish(),
            );
            top_row.add_child(
                Text::new("Remove", font, 10.)
                    .with_color(accent.into())
                    .finish(),
            );
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

            if branch.is_some() || has_badges {
                let branch_element: Box<dyn Element> = match branch.clone() {
                    Some(branch) => render_git_branch_text(&branch, sub_color, 10., app_appearance),
                    None => Empty::new().finish(),
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

        // Full path in a hover tooltip; the row shows the shortened form.
        if hover.is_hovered() && short_path != full_path {
            let tooltip = ui_builder.tool_tip(full_path.clone()).build().finish();
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
        if is_dead {
            ctx.dispatch_typed_action(WorkspaceAction::RemoveRepoModeEntry(path.clone()));
        } else if is_selected {
            // Clicking the expanded repo collapses it (deselect).
            ctx.dispatch_typed_action(WorkspaceAction::SelectRepoModeAll);
        } else {
            ctx.dispatch_typed_action(WorkspaceAction::SelectRepoModeEntry(path.clone()));
        }
    })
    .on_right_click(move |ctx, _, position| {
        ctx.dispatch_typed_action(WorkspaceAction::ToggleRepoModeEntryMenu {
            path: path_for_menu.clone(),
            position,
        });
    })
    .finish()
}

/// Current branch (or short detached SHA) for a repo root, via `.git/HEAD`.
/// Cheap enough to poll behind a short-lived cache; supports linked worktrees
/// where `.git` is a file pointing at the real git dir.
fn repo_branch(
    cache: &RefCell<HashMap<String, (Instant, Option<String>)>>,
    root: &Path,
) -> Option<String> {
    let key = root.to_string_lossy().into_owned();
    let now = Instant::now();
    if let Some((refreshed_at, cached)) = cache.borrow().get(&key) {
        if now.duration_since(*refreshed_at) < BRANCH_CACHE_TTL {
            return cached.clone();
        }
    }
    let branch = read_git_head_branch(root);
    cache.borrow_mut().insert(key, (now, branch.clone()));
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
