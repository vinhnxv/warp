//! Repositories section UI for the vertical tabs panel (repo mode).

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use pathfinder_color::ColorU;
use pathfinder_geometry::vector::vec2f;
use repo_mode::RepoEntryKind;
use warp_core::ui::theme::color::internal_colors;
use warp_core::ui::theme::Fill as ThemeFill;
use warp_core::ui::Icon as WarpIcon;
use warpui::elements::{
    ChildAnchor, ConstrainedBox, Container, CrossAxisAlignment, Element, Empty, Flex, Hoverable,
    MainAxisAlignment, MainAxisSize, MouseStateHandle, OffsetPositioning, Padding, ParentAnchor,
    ParentElement, ParentOffsetBounds, Stack, Text,
};
use warpui::ui_components::components::UiComponent;
use warpui::{AppContext, SingletonEntity};

use super::repo_mode_model::RepoModeListEntry;
use super::Workspace;
use crate::appearance::Appearance;
use crate::workspace::WorkspaceAction;

const ROW_ICON_SIZE: f32 = 14.;
const BRANCH_CACHE_TTL: Duration = Duration::from_secs(5);

/// Per-row mouse state for the Repositories section.
#[derive(Clone, Default)]
pub(super) struct RepoSidebarState {
    pub add_button: MouseStateHandle,
    pub all_row: MouseStateHandle,
    pub entry_rows: RefCell<HashMap<String, MouseStateHandle>>,
    /// Cached branch per repo root, refreshed at most every `BRANCH_CACHE_TTL`.
    pub branch_cache: RefCell<HashMap<String, (Instant, Option<String>)>>,
}

/// Renders the fixed Repositories block above the tab scroller.
pub(super) fn render_repo_sidebar(
    state: &RepoSidebarState,
    workspace: &Workspace,
    app: &AppContext,
) -> Box<dyn Element> {
    if !Workspace::repo_mode_enabled() {
        return Flex::column().finish();
    }

    let appearance = Appearance::as_ref(app);
    let entries = workspace.repo_mode_entries(app);
    let selected = workspace.selected_repo_root.as_deref();

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
        .with_child(render_add_button(state.add_button.clone(), appearance))
        .finish();

    let mut column = Flex::column()
        .with_main_axis_size(MainAxisSize::Min)
        .with_cross_axis_alignment(CrossAxisAlignment::Stretch)
        .with_child(
            Container::new(header)
                .with_padding(Padding::uniform(8.).with_bottom(4.))
                .finish(),
        )
        .with_child(render_all_row(
            state.all_row.clone(),
            selected.is_none(),
            appearance,
        ));

    if entries.is_empty() {
        column = column.with_child(render_empty_state(appearance));
    }

    for entry in entries {
        let key = entry.path.to_string_lossy().into_owned();
        let mouse = state
            .entry_rows
            .borrow_mut()
            .entry(key.clone())
            .or_default()
            .clone();
        let is_selected = selected == Some(key.as_str());
        let branch = match entry.kind {
            RepoEntryKind::Repo if !entry.is_dead => {
                repo_branch(&state.branch_cache, &entry.path)
            }
            _ => None,
        };
        column = column.with_child(render_entry_row(
            entry,
            branch,
            mouse,
            is_selected,
            appearance,
        ));
    }

    // Divider separating the fixed section from the terminal tab scroller (R5).
    let theme = appearance.theme();
    let divider = Container::new(
        ConstrainedBox::new(
            Container::new(Empty::new().finish())
                .with_background(internal_colors::fg_overlay_2(theme))
                .finish(),
        )
        .with_height(1.)
        .finish(),
    )
    .with_padding(Padding::uniform(0.).with_top(4.).with_left(8.).with_right(8.))
    .finish();
    column = column.with_child(divider);

    Container::new(column.finish())
        .with_padding(Padding::uniform(0.).with_bottom(4.))
        .finish()
}

fn render_add_button(mouse: MouseStateHandle, app_appearance: &Appearance) -> Box<dyn Element> {
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
            Text::new("+ Add", font, 11.)
                .with_color(accent.into())
                .finish(),
        )
        .with_padding(Padding::uniform(4.))
        .with_background(background)
        .finish()
    })
    .on_click(|ctx, _, _| {
        ctx.dispatch_typed_action(WorkspaceAction::AddLocalRepositoryOrFolder);
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

fn render_all_row(
    mouse: MouseStateHandle,
    is_selected: bool,
    app_appearance: &Appearance,
) -> Box<dyn Element> {
    let theme = app_appearance.theme();
    let font = app_appearance.ui_font_family();
    let color = theme.font_color(theme.background());
    Hoverable::new(mouse, move |hover| {
        let background = if is_selected {
            internal_colors::fg_overlay_3(theme)
        } else if hover.is_hovered() {
            internal_colors::fg_overlay_1(theme)
        } else {
            ThemeFill::Solid(ColorU::transparent_black())
        };
        Container::new(
            Text::new("All", font, 12.)
                .with_color(color.into())
                .finish(),
        )
        .with_padding(Padding::uniform(8.).with_top(4.).with_bottom(4.))
        .with_background(background)
        .finish()
    })
    .on_click(|ctx, _, _| {
        ctx.dispatch_typed_action(WorkspaceAction::SelectRepoModeAll);
    })
    .finish()
}

fn render_entry_row(
    entry: RepoModeListEntry,
    branch: Option<String>,
    mouse: MouseStateHandle,
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
            icon.to_warpui_icon(if is_dead {
                sub_color.clone()
            } else {
                primary_color.clone()
            })
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
                    .with_color(primary_color.clone().into())
                    .finish(),
            )
            .with_child(
                Text::new(short_path.clone(), font, 10.)
                    .with_color(sub_color.clone().into())
                    .finish(),
            )
            .finish();

        // Trailing slot: branch name for live repos, inline Remove for dead
        // paths (R4), nothing otherwise.
        let trailing: Option<Box<dyn Element>> = if is_dead {
            Some(
                Text::new("Remove", font, 10.)
                    .with_color(accent.into())
                    .finish(),
            )
        } else {
            branch.clone().map(|branch| {
                Text::new(branch, font, 10.)
                    .with_color(sub_color.clone().into())
                    .finish()
            })
        };

        let mut row = Flex::row()
            .with_main_axis_size(MainAxisSize::Max)
            .with_main_axis_alignment(MainAxisAlignment::SpaceBetween)
            .with_cross_axis_alignment(CrossAxisAlignment::Center)
            .with_spacing(6.)
            .with_child(
                Flex::row()
                    .with_main_axis_size(MainAxisSize::Min)
                    .with_cross_axis_alignment(CrossAxisAlignment::Center)
                    .with_spacing(6.)
                    .with_child(icon_element)
                    .with_child(text_column)
                    .finish(),
            );
        if let Some(trailing) = trailing {
            row = row.with_child(trailing);
        }

        let row_container = Container::new(row.finish())
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
