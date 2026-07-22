//! The "Open Folder in IDE" toolbar split-button: target resolution,
//! launch/reveal handling, button state refresh, and dropdown menu
//! construction.
//!
//! The button/menu view handles and the installed-editor cache live as fields
//! on [`Workspace`] (`open_folder_button`, `open_folder_menu`,
//! `show_open_folder_menu`, `installed_editors_cache`); this module owns the
//! behavior around them. The whole module is gated behind the `local_fs`
//! feature at its declaration in `view.rs`.

use std::path::{Path, PathBuf};

use repo_metadata::repositories::DetectedRepositories;
use warp_core::features::FeatureFlag;
use warpui::elements::{Align, Element};
use warpui::{AppContext, SingletonEntity, ViewContext};

use super::Workspace;
use crate::code::buffer_location::LocalOrRemotePath;
use crate::menu::{MenuItem, MenuItemFields};
use crate::ui_components::icons;
use crate::util::file::external_editor::settings::resolve_default_folder_editor_with_installed;
use crate::util::file::external_editor::{installed_editors, Editor};
use crate::view_components::compactible_action_button::RenderCompactibleActionButton;
use crate::workspace::WorkspaceAction;
use crate::{send_telemetry_from_ctx, TelemetryEvent};

impl Workspace {
    /// Resolves the folder the "open folder in IDE" action should open for the
    /// active tab. This is the shared seam consumed by the
    /// open handlers and the toolbar button's enable/disable state.
    ///
    /// Returns `None` when the active session is remote/SSH or its cwd no longer
    /// exists locally; otherwise returns the deepest-ancestor repo root that
    /// owns the cwd, or the cwd itself when no known repo owns it.
    pub(super) fn resolve_open_folder_target(&self, ctx: &AppContext) -> Option<PathBuf> {
        // `canonical_session_pwd_if_local` already returns `None` for remote/SSH
        // sessions and for a cwd that no longer exists locally, so no extra
        // existence check is needed here.
        let cwd = self
            .active_tab_pane_group()
            .as_ref(ctx)
            .active_session_view(ctx)
            .and_then(|tv| tv.as_ref(ctx).canonical_session_pwd_if_local(ctx))
            .map(PathBuf::from);

        resolve_open_folder_target_from(cwd, |path| {
            // `cwd` is already canonicalized, so use the no-I/O lookup.
            DetectedRepositories::as_ref(ctx)
                .get_root_for_canonical_path(&LocalOrRemotePath::Local(path.to_path_buf()))
                .and_then(|root| PathBuf::try_from(root).ok())
        })
    }

    /// Opens the active tab's folder in the default folder IDE (primary click),
    /// falling back to revealing in Finder when no default is set/installed.
    pub(super) fn open_current_folder_in_default_ide(&mut self, ctx: &mut ViewContext<Self>) {
        let installed = self.installed_editors_cached(false, ctx);
        let action = default_open_folder_action(resolve_default_folder_editor_with_installed(
            ctx, &installed,
        ));
        self.open_current_folder(action, false, ctx);
    }

    /// Shared flow for the three "open current folder" actions:
    /// resolve the target folder, then on a hit launch the IDE (or reveal in
    /// Finder) and emit telemetry. Early-returns with no launch and no telemetry
    /// when the folder can't be resolved (remote/SSH or missing cwd).
    ///
    /// No open action ever writes the default-folder-IDE setting; the default
    /// is fixed and managed from Settings (plan R6/KTD3).
    pub(super) fn open_current_folder(
        &mut self,
        action: OpenFolderAction,
        from_dropdown: bool,
        ctx: &mut ViewContext<Self>,
    ) {
        let Some(folder) = self.resolve_open_folder_target(ctx) else {
            // The button was enabled but the session has since gone
            // remote (or lost its cwd) without a tab switch. Re-sync the
            // button so it disables and its tooltip explains why.
            self.refresh_open_folder_button_state(ctx);
            return;
        };

        // Snapshot the telemetry target before consuming `action` in the launch.
        let target = action.telemetry_target();
        match action {
            OpenFolderAction::LaunchEditor(editor) => {
                crate::util::file::open_file_path_with_editor(None, folder, Some(editor), ctx);
            }
            OpenFolderAction::Reveal => {
                ctx.open_file_path_in_explorer(&folder);
            }
        }

        // Record which app the folder opened in and whether it came from the
        // primary click (`false`) or the dropdown (`true`).
        send_telemetry_from_ctx!(
            TelemetryEvent::OpenedFolderInIde {
                target,
                from_dropdown,
            },
            ctx
        );
    }

    /// Renders the pre-built open-folder split button. Its
    /// disabled state and tooltip are kept current by
    /// [`Self::refresh_open_folder_button_state`]; here we only lay it out.
    /// Rendered compact (icon + chevron) so it blends into the icon-only
    /// toolbar next to Settings.
    pub(super) fn render_open_folder_button(&self) -> Box<dyn Element> {
        Align::new(self.open_folder_button.render_compact_button()).finish()
    }

    /// Refreshes the open-folder button's disabled state and tooltip from the
    /// active tab. No-op when the feature flag is off so the
    /// installed-editor probe never runs for users without the feature.
    ///
    /// - A `None` resolver result means the active session is remote/SSH (or has
    ///   no local cwd): the button is disabled and the tooltip explains why.
    /// - Otherwise the tooltip names the default IDE the primary click opens, or
    ///   reads the OS reveal label when no default IDE is set/installed (the
    ///   primary then reveals in Finder).
    pub(super) fn refresh_open_folder_button_state(&mut self, ctx: &mut ViewContext<Self>) {
        if !FeatureFlag::OpenFolderInIde.is_enabled() {
            return;
        }
        let is_remote = self.resolve_open_folder_target(ctx).is_none();
        let installed = self.installed_editors_cached(false, ctx);
        let default_editor = resolve_default_folder_editor_with_installed(ctx, &installed);
        let tooltip =
            open_folder_button_tooltip(if is_remote { None } else { default_editor }, is_remote);

        // Show the default IDE's full-color logo on the primary button; fall
        // back to the generic tinted code icon when no default IDE is set.
        self.open_folder_button
            .set_image_icon(default_editor.and_then(|editor| editor.logo_asset()), ctx);
        self.open_folder_button.set_disabled(is_remote, ctx);
        self.open_folder_button.set_tooltip(Some(tooltip), ctx);
    }

    /// Returns the installed-editor list for the open-folder button, scanning
    /// at most once per workspace lifetime unless `rescan` is set. The probe is
    /// expensive on macOS (one uncached LaunchServices lookup per supported
    /// editor), so state refreshes reuse the cache and only rare, user-initiated
    /// events (opening the dropdown, changing editor settings) force a rescan.
    pub(super) fn installed_editors_cached(
        &mut self,
        rescan: bool,
        ctx: &mut ViewContext<Self>,
    ) -> Vec<Editor> {
        if rescan || self.installed_editors_cache.is_none() {
            self.installed_editors_cache = Some(installed_editors(ctx));
        }
        self.installed_editors_cache.clone().unwrap_or_default()
    }

    /// Toggles the open-folder dropdown. Opening rescans the installed
    /// editors once and (re)builds the menu from the result. On macOS the
    /// rescan probes live, so newly-installed editors appear without
    /// restarting; on Windows/Linux the underlying editor metadata is cached
    /// for the process lifetime (`INSTALLED_EDITOR_METADATA`), so a restart is
    /// needed there.
    pub(super) fn toggle_open_folder_menu(&mut self, ctx: &mut ViewContext<Self>) {
        self.show_open_folder_menu = !self.show_open_folder_menu;
        if self.show_open_folder_menu {
            let installed = self.installed_editors_cached(true, ctx);
            let default_editor = resolve_default_folder_editor_with_installed(ctx, &installed);
            let items = open_folder_menu_items(&installed, default_editor);
            self.open_folder_menu.update(ctx, |menu, ctx| {
                menu.set_items(items, ctx);
            });
            ctx.focus(&self.open_folder_menu);
        }
        // Keep the primary's disabled/tooltip state current for the
        // interaction; reuses the cache refreshed above.
        self.refresh_open_folder_button_state(ctx);
        ctx.notify();
    }
}

/// Pure decision for the "open folder in IDE" target.
///
/// Given the active tab's already-resolved local working directory and a
/// repo-root lookup, returns the folder the action should open:
/// - `None` when there is no local cwd. `cwd` is `None` for remote/SSH sessions
///   and for a cwd that no longer exists locally, because the caller sources it
///   from [`TerminalView::canonical_session_pwd_if_local`], which already
///   applies both guards. No filesystem check is repeated here.
/// - the deepest-ancestor repo root that owns the cwd, when one exists.
/// - the cwd itself when no known repo owns it.
///
/// The repo-root lookup is injected as a closure so this decision can be unit
/// tested without an `AppContext`. The ctx-bound adapter
/// [`Workspace::resolve_open_folder_target`] wires it to
/// [`DetectedRepositories::get_root_for_canonical_path`], which performs the
/// actual deepest-ancestor resolution.
fn resolve_open_folder_target_from(
    cwd: Option<PathBuf>,
    repo_root_for_cwd: impl FnOnce(&Path) -> Option<PathBuf>,
) -> Option<PathBuf> {
    let cwd = cwd?;
    Some(repo_root_for_cwd(&cwd).unwrap_or(cwd))
}

/// The concrete side effect an "open current folder" action resolves to once
/// the target folder is known and (for the default action) the default IDE has
/// been looked up. Factored out of the ctx-bound handler so the launch-vs-reveal
/// branch and the telemetry payload it produces are unit-testable without an
/// `AppContext`.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum OpenFolderAction {
    /// Launch the folder in this IDE (primary default or dropdown pick).
    LaunchEditor(Editor),
    /// Reveal the folder in Finder / Explorer / the OS file manager, with no
    /// IDE (dropdown pick, and the fallback when no default IDE is set/installed).
    Reveal,
}

impl OpenFolderAction {
    /// The telemetry `target`: the IDE's display name, or the literal
    /// `"finder"` for a reveal. Never contains the folder path, so the payload
    /// stays UGC-free.
    fn telemetry_target(&self) -> String {
        match self {
            OpenFolderAction::LaunchEditor(editor) => format!("{editor}"),
            OpenFolderAction::Reveal => "finder".to_string(),
        }
    }
}

/// Decides what the *default* open action (`OpenCurrentFolderInDefaultIde`)
/// does given the resolved default folder IDE: launch that IDE when one is
/// set/installed, otherwise fall back to revealing the folder in Finder.
/// Pure so the fallback branch is testable without an `AppContext`.
fn default_open_folder_action(default_editor: Option<Editor>) -> OpenFolderAction {
    match default_editor {
        Some(editor) => OpenFolderAction::LaunchEditor(editor),
        None => OpenFolderAction::Reveal,
    }
}

/// OS-aware label for revealing a folder in the system file manager. Mirrors
/// the label the code view's context menu uses (`app/src/code/view.rs`). Pure
/// so the platform branch is unit-testable.
fn os_reveal_label() -> &'static str {
    if cfg!(target_os = "macos") {
        "Reveal in Finder"
    } else if cfg!(target_os = "windows") {
        "Reveal in Explorer"
    } else {
        "Reveal in file manager"
    }
}

/// Pure tooltip decision for the open-folder toolbar button:
/// - remote/disabled tab -> explains why the button is unavailable.
/// - default IDE set -> names the IDE the primary click opens.
/// - no default IDE set/installed -> the primary reveals in Finder, so the
///   tooltip reads the OS reveal label.
fn open_folder_button_tooltip(default_editor: Option<Editor>, is_remote: bool) -> String {
    if is_remote {
        "Not available for remote sessions".to_string()
    } else if let Some(editor) = default_editor {
        format!("Open folder in {editor}")
    } else {
        os_reveal_label().to_string()
    }
}

/// Pure builder for the open-folder dropdown's items: one row per
/// installed IDE (each opens the folder one-off in that IDE, never changing
/// the default), a visual separator, then the OS-aware Reveal item. Rows show
/// the IDE's full-color logo, and the current default IDE is marked with a
/// trailing check. With zero installed IDEs the menu is just the Reveal item
/// -- no IDE rows and no separator, since the primary already carries the
/// Finder fallback.
fn open_folder_menu_items(
    installed_editors: &[Editor],
    default_editor: Option<Editor>,
) -> Vec<MenuItem<WorkspaceAction>> {
    let mut items: Vec<MenuItem<WorkspaceAction>> = installed_editors
        .iter()
        .map(|editor| {
            let mut fields = MenuItemFields::new(format!("{editor}"))
                .with_on_select_action(WorkspaceAction::OpenCurrentFolderIn(*editor));
            if let Some(logo) = editor.logo_asset() {
                fields = fields.with_image_icon(logo);
            }
            if default_editor == Some(*editor) {
                fields = fields.with_right_side_icon(icons::Icon::Check);
            }
            fields.into_item()
        })
        .collect();

    if !items.is_empty() {
        items.push(MenuItem::Separator);
    }

    items.push(
        MenuItemFields::new(os_reveal_label())
            .with_on_select_action(WorkspaceAction::RevealCurrentFolder)
            .into_item(),
    );

    items
}

#[cfg(test)]
#[path = "open_folder_tests.rs"]
mod tests;
