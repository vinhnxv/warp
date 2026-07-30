use std::cell::RefCell;
use std::collections::HashMap;

use settings::{Setting, ToggleableSetting};
use warp_core::features::FeatureFlag;
use warp_errors::report_if_error;
use warpui::elements::{Flex, MouseStateHandle, ParentElement};
use warpui::ui_components::components::UiComponent;
use warpui::ui_components::switch::SwitchStateHandle;
use warpui::{Element, Entity, SingletonEntity, TypedActionView, View, ViewContext, ViewHandle};

use crate::appearance::Appearance;
use crate::send_telemetry_from_ctx;
use crate::server::telemetry::TelemetryEvent;
use crate::settings_view::settings_page::{
    AdditionalInfo, LocalOnlyIconState, ToggleState, render_body_item, render_dropdown_item,
};
use crate::util::file::external_editor::settings::{
    DefaultFolderEditor, EditorChoice, EditorLayout, OpenCodePanelsFileEditor, OpenFileEditor,
    OpenFileLayout, PreferMarkdownViewer, PreferTabbedEditorView,
    resolve_default_folder_editor_with_installed,
};
use crate::util::file::external_editor::{Editor, EditorSettings, installed_editors};
use crate::view_components::{Dropdown, DropdownItem};

const TABBED_FILE_VIEWER_TOGGLE_HEADER: &str = "Group files into single editor pane";
const TABBED_FILE_VIEWER_TOGGLE_DESCRIPTION: &str = "When this setting is on, any files opened in the same tab will be automatically grouped into a single editor pane.";

#[derive(Debug, Clone, PartialEq)]
pub enum ExternalEditorAction {
    SetEditor(EditorChoice),
    SetDefaultFolderEditor(EditorChoice),
    SetCodePanelsEditor(EditorChoice),
    SetLayout(EditorLayout),
    TogglePreferMarkdownViewer,
    ToggleTabbedEditorView,
    OpenUrl(String),
}

pub struct ExternalEditorView {
    editor_dropdown: ViewHandle<Dropdown<ExternalEditorAction>>,
    folder_editor_dropdown: ViewHandle<Dropdown<ExternalEditorAction>>,
    code_panels_editor_dropdown: ViewHandle<Dropdown<ExternalEditorAction>>,
    layout_dropdown: ViewHandle<Dropdown<ExternalEditorAction>>,
    tabbed_editor_view_mouse_state: SwitchStateHandle,
    prefer_markdown_viewer_switch: SwitchStateHandle,
    markdown_viewer_mouse_state: MouseStateHandle,
    local_only_icon_states: RefCell<HashMap<String, MouseStateHandle>>,
}

impl ExternalEditorView {
    pub fn new(ctx: &mut ViewContext<Self>) -> Self {
        let settings = EditorSettings::handle(ctx);
        let editor_to_open_files = *settings.as_ref(ctx).open_file_editor;
        let code_panels_editor_to_open_files = *settings.as_ref(ctx).open_code_panels_file_editor;
        let layout_to_open_files = *settings.as_ref(ctx).open_file_layout;
        // One scan for the whole page. Three of the four dropdowns below need
        // the installed-editor list, and each used to probe for it: on macOS
        // that is an uncached LaunchServices lookup per supported editor, so
        // opening Settings cost four sweeps of the same answer.
        let installed = installed_editors(ctx);
        // Only resolve the folder default when the feature is on, so the
        // hidden row costs nothing.
        let folder_editor_to_open_folders = FeatureFlag::OpenFolderInIde
            .is_enabled()
            .then(|| resolve_default_folder_editor_with_installed(ctx, &installed))
            .flatten();

        let editor_dropdown = ctx.add_typed_action_view(|ctx| {
            let mut dropdown = Dropdown::new(ctx);
            Self::init_editor_dropdown(
                &editor_to_open_files,
                &installed,
                &mut dropdown,
                ExternalEditorAction::SetEditor,
                ctx,
            );
            dropdown
        });
        let folder_editor_dropdown = ctx.add_typed_action_view(|ctx| {
            let mut dropdown = Dropdown::new(ctx);
            if FeatureFlag::OpenFolderInIde.is_enabled() {
                Self::init_folder_editor_dropdown(
                    folder_editor_to_open_folders,
                    &installed,
                    &mut dropdown,
                    ctx,
                );
            }
            dropdown
        });
        let code_panels_editor_dropdown = ctx.add_typed_action_view(|ctx| {
            let mut dropdown = Dropdown::new(ctx);
            Self::init_editor_dropdown(
                &code_panels_editor_to_open_files,
                &installed,
                &mut dropdown,
                ExternalEditorAction::SetCodePanelsEditor,
                ctx,
            );
            dropdown
        });
        let layout_dropdown = ctx.add_typed_action_view(|ctx| {
            let mut dropdown = Dropdown::new(ctx);
            Self::init_layout_dropdown(&layout_to_open_files, &mut dropdown, ctx);
            dropdown
        });
        ctx.subscribe_to_model(
            &EditorSettings::handle(ctx),
            |me, editor_settings, _, ctx| {
                // Same one-scan-per-rebuild rule as construction: this fires on
                // every settings change, including the cloud syncs that arrive
                // unprompted.
                let installed = installed_editors(ctx);
                me.editor_dropdown.update(ctx, |dropdown, ctx| {
                    let editor = *editor_settings.as_ref(ctx).open_file_editor;
                    Self::init_editor_dropdown(
                        &editor,
                        &installed,
                        dropdown,
                        ExternalEditorAction::SetEditor,
                        ctx,
                    );
                });
                me.code_panels_editor_dropdown.update(ctx, |dropdown, ctx| {
                    let editor = *editor_settings.as_ref(ctx).open_code_panels_file_editor;
                    Self::init_editor_dropdown(
                        &editor,
                        &installed,
                        dropdown,
                        ExternalEditorAction::SetCodePanelsEditor,
                        ctx,
                    );
                });
                if FeatureFlag::OpenFolderInIde.is_enabled() {
                    let selected = resolve_default_folder_editor_with_installed(ctx, &installed);
                    me.folder_editor_dropdown.update(ctx, |dropdown, ctx| {
                        Self::init_folder_editor_dropdown(selected, &installed, dropdown, ctx);
                    });
                }
                ctx.notify()
            },
        );

        Self {
            editor_dropdown,
            folder_editor_dropdown,
            code_panels_editor_dropdown,
            layout_dropdown,
            tabbed_editor_view_mouse_state: Default::default(),
            prefer_markdown_viewer_switch: Default::default(),
            markdown_viewer_mouse_state: Default::default(),
            local_only_icon_states: Default::default(),
        }
    }

    fn init_layout_dropdown(
        layout_to_open_files: &EditorLayout,
        dropdown: &mut Dropdown<ExternalEditorAction>,
        ctx: &mut ViewContext<Dropdown<ExternalEditorAction>>,
    ) {
        let default_option_text = "Split Pane";
        let default_app = DropdownItem::new(
            default_option_text,
            ExternalEditorAction::SetLayout(EditorLayout::SplitPane),
        );

        let mut items = vec![default_app];
        items.push(DropdownItem::new(
            "New Tab",
            ExternalEditorAction::SetLayout(EditorLayout::NewTab),
        ));

        dropdown.set_items(items, ctx);
        match layout_to_open_files {
            EditorLayout::SplitPane => dropdown.set_selected_by_name(default_option_text, ctx),
            EditorLayout::NewTab => dropdown.set_selected_by_name("New Tab", ctx),
        };
    }

    /// The default option's label, and the selection shown when the setting is
    /// [`EditorChoice::SystemDefault`].
    const DEFAULT_OPTION_TEXT: &'static str = "Default App";

    /// Builds the `(label, choice)` entries for an open-*file* editor dropdown:
    /// the fixed Warp / system / `$EDITOR` entries, then one per installed
    /// editor.
    ///
    /// Takes the installed list rather than probing per editor. That is what
    /// lets the page detect once and build every dropdown from the one answer —
    /// this used to be an `is_installed` call per supported editor per
    /// dropdown, so opening Settings ran the sweep four times over.
    fn editor_dropdown_items(installed: &[Editor]) -> Vec<(String, EditorChoice)> {
        let mut items = vec![
            (
                Self::DEFAULT_OPTION_TEXT.to_string(),
                EditorChoice::SystemDefault,
            ),
            ("Warp".to_string(), EditorChoice::Warp),
        ];
        if FeatureFlag::AllowOpeningFileLinksUsingEditorEnv.is_enabled() {
            items.push(("$EDITOR".to_string(), EditorChoice::EnvEditor));
        }
        // `installed` is already `SUPPORTED_EDITORS` filtered by installation,
        // and in the same order, so the entries land exactly as they did.
        items.extend(
            installed
                .iter()
                .map(|editor| (format!("{editor}"), EditorChoice::ExternalEditor(*editor))),
        );
        items
    }

    fn init_editor_dropdown(
        editor_to_open_files: &EditorChoice,
        installed: &[Editor],
        dropdown: &mut Dropdown<ExternalEditorAction>,
        mut make_action: impl FnMut(EditorChoice) -> ExternalEditorAction,
        ctx: &mut ViewContext<Dropdown<ExternalEditorAction>>,
    ) {
        let default_option_text = Self::DEFAULT_OPTION_TEXT;
        let items: Vec<DropdownItem<ExternalEditorAction>> = Self::editor_dropdown_items(installed)
            .into_iter()
            .map(|(label, choice)| DropdownItem::new(label, make_action(choice)))
            .collect();

        dropdown.set_items(items, ctx);
        match editor_to_open_files {
            EditorChoice::ExternalEditor(editor) => {
                dropdown.set_selected_by_name(format!("{editor}"), ctx)
            }
            EditorChoice::Warp => dropdown.set_selected_by_name("Warp", ctx),
            EditorChoice::EnvEditor => dropdown.set_selected_by_name("$EDITOR", ctx),
            EditorChoice::SystemDefault => dropdown.set_selected_by_name(default_option_text, ctx),
        };
    }

    /// Builds the `(label, choice)` entries for the default-folder-editor
    /// dropdown: one per installed editor. Unlike [`Self::init_editor_dropdown`]
    /// there are no "Default App" / "Warp" / "$EDITOR" entries, because the
    /// `default_folder_editor` setting only ever holds an
    /// [`EditorChoice::ExternalEditor`].
    fn folder_editor_dropdown_items(installed_editors: &[Editor]) -> Vec<(String, EditorChoice)> {
        installed_editors
            .iter()
            .map(|editor| (format!("{editor}"), EditorChoice::ExternalEditor(*editor)))
            .collect()
    }

    fn init_folder_editor_dropdown(
        selected_editor: Option<Editor>,
        installed: &[Editor],
        dropdown: &mut Dropdown<ExternalEditorAction>,
        ctx: &mut ViewContext<Dropdown<ExternalEditorAction>>,
    ) {
        let items: Vec<DropdownItem<ExternalEditorAction>> =
            Self::folder_editor_dropdown_items(installed)
                .into_iter()
                .map(|(label, choice)| {
                    DropdownItem::new(label, ExternalEditorAction::SetDefaultFolderEditor(choice))
                })
                .collect();

        dropdown.set_items(items, ctx);
        if let Some(editor) = selected_editor {
            dropdown.set_selected_by_name(format!("{editor}"), ctx);
        }
    }

    /// Handles [`ExternalEditorAction::SetEditor`] by updating the external editor settings.
    fn set_editor(&mut self, editor: &EditorChoice, ctx: &mut ViewContext<Self>) {
        EditorSettings::handle(ctx).update(ctx, |settings, ctx| {
            report_if_error!(settings.open_file_editor.set_value(*editor, ctx));
        });

        send_telemetry_from_ctx!(
            TelemetryEvent::FeaturesPageAction {
                action: "SetEditor".to_string(),
                value: format!("{editor:?}")
            },
            ctx
        );
    }

    /// Handles [`ExternalEditorAction::SetDefaultFolderEditor`] by updating the
    /// default folder editor setting.
    fn set_default_folder_editor(&mut self, editor: &EditorChoice, ctx: &mut ViewContext<Self>) {
        EditorSettings::handle(ctx).update(ctx, |settings, ctx| {
            report_if_error!(settings.default_folder_editor.set_value(*editor, ctx));
        });

        send_telemetry_from_ctx!(
            TelemetryEvent::FeaturesPageAction {
                action: "SetDefaultFolderEditor".to_string(),
                value: format!("{editor:?}")
            },
            ctx
        );
    }

    fn set_code_panels_editor(&mut self, editor: &EditorChoice, ctx: &mut ViewContext<Self>) {
        EditorSettings::handle(ctx).update(ctx, |settings, ctx| {
            report_if_error!(
                settings
                    .open_code_panels_file_editor
                    .set_value(*editor, ctx)
            );
        });

        send_telemetry_from_ctx!(
            TelemetryEvent::FeaturesPageAction {
                action: "SetCodePanelsEditor".to_string(),
                value: format!("{editor:?}")
            },
            ctx
        );
    }

    // Handles [`ExternalEditorAction::SetLayout`] by updating the external editor layout settings.
    fn set_layout(&mut self, layout: &EditorLayout, ctx: &mut ViewContext<Self>) {
        EditorSettings::handle(ctx).update(ctx, |settings, ctx| {
            report_if_error!(settings.open_file_layout.set_value(*layout, ctx));
        });

        send_telemetry_from_ctx!(
            TelemetryEvent::FeaturesPageAction {
                action: "SetLayout".to_string(),
                value: format!("{layout:?}")
            },
            ctx
        );
    }

    /// Handles [`ExternalEditorAction::TogglePreferMarkdownViewer`]
    /// preference.
    fn toggle_prefer_markdown_viewer(&mut self, ctx: &mut ViewContext<Self>) {
        let new_value = EditorSettings::handle(ctx).update(ctx, |settings, ctx| {
            let new_value = settings.prefer_markdown_viewer.toggle_and_save_value(ctx);
            report_if_error!(new_value);
            new_value.unwrap_or(PreferMarkdownViewer::default_value())
        });

        send_telemetry_from_ctx!(
            TelemetryEvent::FeaturesPageAction {
                action: "TogglePreferMarkdownViewer".to_string(),
                value: new_value.to_string()
            },
            ctx
        );
    }

    /// Handles [`ExternalEditorAction::TogglePreferTabbedEditorView`] by updating the tabbed file viewer preference.
    fn toggle_prefer_tabbed_editor_view(&mut self, ctx: &mut ViewContext<Self>) {
        let new_value = EditorSettings::handle(ctx).update(ctx, |settings, ctx| {
            let new_value = settings
                .prefer_tabbed_editor_view
                .toggle_and_save_value(ctx);
            report_if_error!(new_value);
            new_value.unwrap_or(PreferTabbedEditorView::default_value())
        });

        send_telemetry_from_ctx!(
            TelemetryEvent::FeaturesPageAction {
                action: "ToggleTabbedEditorView".to_string(),
                value: new_value.to_string()
            },
            ctx
        );
    }
}

impl Entity for ExternalEditorView {
    type Event = ();
}

impl View for ExternalEditorView {
    fn ui_name() -> &'static str {
        "ExternalEditorView"
    }

    fn render(&self, app: &warpui::AppContext) -> Box<dyn warpui::Element> {
        let appearance = Appearance::as_ref(app);

        let default_editor = render_dropdown_item(
            appearance,
            "Choose an editor to open file links",
            None,
            None,
            LocalOnlyIconState::for_setting(
                OpenFileEditor::storage_key(),
                OpenFileEditor::sync_to_cloud(),
                &mut self.local_only_icon_states.borrow_mut(),
                app,
            ),
            None,
            &self.editor_dropdown,
        );

        let code_panels_editor = render_dropdown_item(
            appearance,
            "Choose an editor to open files from the code review panel, project explorer, and global search",
            None,
            None,
            LocalOnlyIconState::for_setting(
                OpenCodePanelsFileEditor::storage_key(),
                OpenCodePanelsFileEditor::sync_to_cloud(),
                &mut self.local_only_icon_states.borrow_mut(),
                app,
            ),
            None,
            &self.code_panels_editor_dropdown,
        );

        let default_layout = render_dropdown_item(
            appearance,
            "Choose a layout to open files in Warp",
            None,
            None,
            LocalOnlyIconState::for_setting(
                OpenFileLayout::storage_key(),
                OpenFileLayout::sync_to_cloud(),
                &mut self.local_only_icon_states.borrow_mut(),
                app,
            ),
            None,
            &self.layout_dropdown,
        );

        let mut column = Flex::column()
            .with_child(default_editor)
            .with_child(code_panels_editor)
            .with_child(default_layout);

        if FeatureFlag::OpenFolderInIde.is_enabled() {
            column.add_child(render_dropdown_item(
                appearance,
                "Choose an editor to open folders in from the toolbar",
                None,
                None,
                LocalOnlyIconState::for_setting(
                    DefaultFolderEditor::storage_key(),
                    DefaultFolderEditor::sync_to_cloud(),
                    &mut self.local_only_icon_states.borrow_mut(),
                    app,
                ),
                None,
                &self.folder_editor_dropdown,
            ));
        }

        if FeatureFlag::TabbedEditorView.is_enabled() {
            column.add_child(render_body_item::<ExternalEditorAction>(
                TABBED_FILE_VIEWER_TOGGLE_HEADER.into(),
                None,
                LocalOnlyIconState::for_setting(
                    PreferTabbedEditorView::storage_key(),
                    PreferTabbedEditorView::sync_to_cloud(),
                    &mut self.local_only_icon_states.borrow_mut(),
                    app,
                ),
                ToggleState::Enabled,
                appearance,
                appearance
                    .ui_builder()
                    .switch(self.tabbed_editor_view_mouse_state.clone())
                    .check(
                        *EditorSettings::as_ref(app)
                            .prefer_tabbed_editor_view
                            .value(),
                    )
                    .build()
                    .on_click(|ctx, _, _| {
                        ctx.dispatch_typed_action(ExternalEditorAction::ToggleTabbedEditorView);
                    })
                    .finish(),
                Some(TABBED_FILE_VIEWER_TOGGLE_DESCRIPTION.into()),
            ));
        }

        column.add_child(render_body_item::<ExternalEditorAction>(
            "Open Markdown files in Warp's Markdown Viewer by default".to_string(),
            Some(AdditionalInfo {
                mouse_state: self.markdown_viewer_mouse_state.clone(),
                on_click_action: Some(ExternalEditorAction::OpenUrl(
                    "https://docs.warp.dev/terminal/more-features/markdown-viewer".to_string(),
                )),
                secondary_text: None,
                tooltip_override_text: None,
            }),
            LocalOnlyIconState::for_setting(
                PreferMarkdownViewer::storage_key(),
                PreferMarkdownViewer::sync_to_cloud(),
                &mut self.local_only_icon_states.borrow_mut(),
                app,
            ),
            ToggleState::Enabled,
            appearance,
            appearance
                .ui_builder()
                .switch(self.prefer_markdown_viewer_switch.clone())
                .check(*EditorSettings::as_ref(app).prefer_markdown_viewer.value())
                .build()
                .on_click(|ctx, _, _| {
                    ctx.dispatch_typed_action(ExternalEditorAction::TogglePreferMarkdownViewer);
                })
                .finish(),
            None,
        ));

        column.finish()
    }
}

impl TypedActionView for ExternalEditorView {
    type Action = ExternalEditorAction;

    fn handle_action(&mut self, action: &Self::Action, ctx: &mut ViewContext<Self>) {
        match action {
            ExternalEditorAction::SetEditor(editor) => self.set_editor(editor, ctx),
            ExternalEditorAction::SetDefaultFolderEditor(editor) => {
                self.set_default_folder_editor(editor, ctx)
            }
            ExternalEditorAction::SetCodePanelsEditor(editor) => {
                self.set_code_panels_editor(editor, ctx)
            }
            ExternalEditorAction::SetLayout(layout) => self.set_layout(layout, ctx),
            ExternalEditorAction::TogglePreferMarkdownViewer => {
                self.toggle_prefer_markdown_viewer(ctx)
            }
            ExternalEditorAction::ToggleTabbedEditorView => {
                self.toggle_prefer_tabbed_editor_view(ctx);
            }
            ExternalEditorAction::OpenUrl(url) => {
                ctx.open_url(url.as_str());
            }
        }
    }
}

#[cfg(test)]
#[path = "external_editor_tests.rs"]
mod tests;
