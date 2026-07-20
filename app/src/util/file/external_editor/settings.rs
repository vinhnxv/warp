use serde::{Deserialize, Deserializer, Serialize};
use settings::macros::define_settings_group;
use settings::{RespectUserSyncSetting, SupportedPlatforms, SyncToCloud};

pub use crate::util::openable_file_type::EditorLayout;

#[derive(
    Debug,
    Clone,
    Copy,
    Serialize,
    PartialEq,
    Eq,
    schemars::JsonSchema,
    settings_value::SettingsValue,
)]
#[schemars(
    description = "Which editor to use when opening files.",
    rename_all = "snake_case"
)]
pub enum EditorChoice {
    SystemDefault,
    Warp,
    EnvEditor,
    #[schemars(description = "A specific external code editor.")]
    ExternalEditor(super::Editor),
}

// Custom Deserialize implementation to handle backward compatibility
// with the old `Option<Editor>` format
impl<'de> Deserialize<'de> for EditorChoice {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum EditorChoiceCompat {
            // Try new format first
            New(EditorChoiceInner),
            // Fall back to old Option<Editor> format
            Old(Option<super::Editor>),
        }

        #[derive(Deserialize)]
        enum EditorChoiceInner {
            SystemDefault,
            Warp,
            EnvEditor,
            ExternalEditor(super::Editor),
        }

        match EditorChoiceCompat::deserialize(deserializer)? {
            EditorChoiceCompat::New(inner) => match inner {
                EditorChoiceInner::SystemDefault => Ok(EditorChoice::SystemDefault),
                EditorChoiceInner::Warp => Ok(EditorChoice::Warp),
                EditorChoiceInner::EnvEditor => Ok(EditorChoice::EnvEditor),
                EditorChoiceInner::ExternalEditor(editor) => {
                    Ok(EditorChoice::ExternalEditor(editor))
                }
            },
            EditorChoiceCompat::Old(old_value) => match old_value {
                None => Ok(EditorChoice::SystemDefault),
                Some(editor) => Ok(EditorChoice::ExternalEditor(editor)),
            },
        }
    }
}

define_settings_group!(EditorSettings, settings: [
    open_file_editor: OpenFileEditor {
        type: EditorChoice,
        default: EditorChoice::SystemDefault,
        supported_platforms: SupportedPlatforms::ALL,
        sync_to_cloud: SyncToCloud::Never,
        surface: settings::SettingSurfaces::GUI,
        private: false,
        toml_path: "code.editor.open_file_editor",
        max_table_depth: 0,
        description: "The editor used to open files.",
    },
    default_folder_editor: DefaultFolderEditor {
        type: EditorChoice,
        default: EditorChoice::SystemDefault,
        supported_platforms: SupportedPlatforms::ALL,
        sync_to_cloud: SyncToCloud::Never,
        surface: settings::SettingSurfaces::GUI,
        private: false,
        toml_path: "code.editor.default_folder_editor",
        max_table_depth: 0,
        description: "The editor the primary toolbar button uses to open folders.",
    },
    open_code_panels_file_editor: OpenCodePanelsFileEditor {
        type: EditorChoice,
        default: EditorChoice::Warp,
        supported_platforms: SupportedPlatforms::ALL,
        sync_to_cloud: SyncToCloud::Never,
        surface: settings::SettingSurfaces::GUI,
        private: false,
        toml_path: "code.editor.open_code_panels_file_editor",
        max_table_depth: 0,
        description: "The editor used to open files from code panels.",
    },
    open_file_layout: OpenFileLayout {
        type: EditorLayout,
        default: EditorLayout::SplitPane,
        supported_platforms: SupportedPlatforms::ALL,
        sync_to_cloud: SyncToCloud::Globally(RespectUserSyncSetting::Yes),
        surface: settings::SettingSurfaces::GUI,
        private: false,
        toml_path: "code.editor.open_file_layout",
        description: "The layout used when opening files in the editor.",
    },
    prefer_markdown_viewer: PreferMarkdownViewer {
        type: bool,
        default: true,
        supported_platforms: SupportedPlatforms::ALL,
        sync_to_cloud: SyncToCloud::Globally(RespectUserSyncSetting::Yes),
        surface: settings::SettingSurfaces::GUI,
        private: false,
        toml_path: "code.editor.prefer_markdown_viewer",
        description: "Whether to use the Markdown viewer when opening Markdown files.",
    },
    prefer_tabbed_editor_view: PreferTabbedEditorView {
        type: bool,
        default: true,
        supported_platforms: SupportedPlatforms::ALL,
        sync_to_cloud: SyncToCloud::Globally(RespectUserSyncSetting::Yes),
        surface: settings::SettingSurfaces::GUI,
        private: false,
        toml_path: "code.editor.prefer_tabbed_editor_view",
        description: "Whether to prefer opening files in a tabbed editor view.",
    },
    open_conversation_layout_preference: OpenConversationLayoutPreference {
        type: OpenConversationPreference,
        default: OpenConversationPreference::NewTab,
        supported_platforms: SupportedPlatforms::ALL,
        sync_to_cloud: SyncToCloud::Globally(RespectUserSyncSetting::Yes),
        surface: settings::SettingSurfaces::GUI,
        private: false,
        toml_path: "agents.warp_agent.other.open_conversation_layout_preference",
        description: "Whether to open agent conversations in a new tab or a split pane.",
    },
]);

#[derive(
    Debug,
    Clone,
    Copy,
    Serialize,
    Deserialize,
    PartialEq,
    Eq,
    schemars::JsonSchema,
    settings_value::SettingsValue,
)]
#[schemars(
    description = "How to open agent conversations.",
    rename_all = "snake_case"
)]
pub enum OpenConversationPreference {
    NewTab,
    SplitPane,
}

impl OpenConversationPreference {
    pub fn is_new_tab(&self) -> bool {
        matches!(self, Self::NewTab)
    }
}

/// Computes the value to seed `default_folder_editor` with the first time it is
/// read while unset.
///
/// - If the user's existing `open_file_editor` is already an external editor,
///   reuse that editor.
/// - Otherwise fall back to the first installed editor.
/// - If no editor is installed, return `None` so the setting stays unset (the
///   primary toolbar button then falls back to revealing the folder in the
///   system file manager).
///
/// Pure helper (no `AppContext`) so the seeding rules can be unit tested; the
/// caller is responsible for supplying the already-installed-filtered editors.
fn seed_default_folder_editor(
    open_file_editor: EditorChoice,
    installed_editors: &[super::Editor],
) -> Option<super::Editor> {
    if let EditorChoice::ExternalEditor(editor) = open_file_editor {
        return Some(editor);
    }
    installed_editors.first().copied()
}

/// Resolves the external editor the primary toolbar button should open folders
/// in.
///
/// When `default_folder_editor` has been set explicitly it is used directly;
/// when it has never been set the first-run seed (see
/// [`seed_default_folder_editor`]) is applied against the currently installed
/// editors. Returns `None` when no external editor is configured or installed.
pub fn resolve_default_folder_editor(ctx: &mut warpui::AppContext) -> Option<super::Editor> {
    use settings::Setting as _;
    use warpui::SingletonEntity as _;

    let editor_settings = EditorSettings::as_ref(ctx);
    let explicitly_set = editor_settings
        .default_folder_editor
        .is_value_explicitly_set();
    let default_folder_editor = *editor_settings.default_folder_editor;
    let open_file_editor = *editor_settings.open_file_editor;

    if explicitly_set {
        match default_folder_editor {
            EditorChoice::ExternalEditor(editor) if editor.is_installed(ctx) => {
                return Some(editor)
            }
            // The configured IDE is no longer installed: fall through to the seed
            // so the primary reveals in Finder instead of launching a missing app.
            EditorChoice::ExternalEditor(_) => {}
            _ => return None,
        }
    }

    seed_default_folder_editor(open_file_editor, &super::installed_editors(ctx))
}

#[cfg(test)]
#[path = "settings_tests.rs"]
mod tests;
