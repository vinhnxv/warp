use serde::{Deserialize, Deserializer, Serialize};
use settings::macros::define_settings_group;
use settings::{RespectUserSyncSetting, SupportedPlatforms, SyncToCloud};
use warp_errors::report_if_error;

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
/// - If the user's existing `open_file_editor` is already an external editor
///   *and it is installed*, reuse that editor.
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
        if installed_editors.contains(&editor) {
            return Some(editor);
        }
    }
    installed_editors.first().copied()
}

/// Resolves the external editor the primary toolbar button should open folders
/// in.
///
/// When `default_folder_editor` has been set it is used directly; when it has
/// never been set the first-run seed (see [`seed_default_folder_editor`]) is
/// applied against the currently installed editors and written down. Returns
/// `None` when the user opted out of an IDE, when the one they chose is no
/// longer installed, or when nothing is installed to suggest — in every case
/// the caller reveals the folder in the file manager instead.
pub fn resolve_default_folder_editor(ctx: &mut warpui::AppContext) -> Option<super::Editor> {
    let installed_editors = super::installed_editors(ctx);
    resolve_default_folder_editor_with_installed(ctx, &installed_editors)
}

/// Like [`resolve_default_folder_editor`], but resolves against a
/// caller-supplied installed-editor list instead of probing each editor's
/// installation itself. Hot paths (tab switch, toolbar refresh, menu open)
/// pass a cached scan here, because on macOS every `is_installed` probe is an
/// uncached LaunchServices lookup.
///
/// The user's choice is authoritative in both directions: a chosen editor that
/// is no longer installed resolves to `None` (reveal in the file manager)
/// rather than to some other IDE, and once the first-run seed has been written
/// it is never recomputed, so installing an application cannot change a default
/// the user is already relying on.
pub fn resolve_default_folder_editor_with_installed(
    ctx: &mut warpui::AppContext,
    installed_editors: &[super::Editor],
) -> Option<super::Editor> {
    use settings::Setting as _;
    use warpui::SingletonEntity as _;

    let editor_settings = EditorSettings::as_ref(ctx);
    let explicitly_set = editor_settings
        .default_folder_editor
        .is_value_explicitly_set();
    let default_folder_editor = *editor_settings.default_folder_editor;
    let open_file_editor = *editor_settings.open_file_editor;

    if explicitly_set {
        return match default_folder_editor {
            EditorChoice::ExternalEditor(editor) if installed_editors.contains(&editor) => {
                Some(editor)
            }
            // The chosen IDE is no longer installed, so the folder is revealed
            // in the file manager. This used to fall through to the seed, which
            // reads `installed_editors.first()` — so uninstalling your IDE
            // silently promoted an unrelated one to be your default.
            EditorChoice::ExternalEditor(_) => None,
            // The Settings row that opts out of an IDE entirely.
            EditorChoice::Warp | EditorChoice::EnvEditor | EditorChoice::SystemDefault => None,
        };
    }

    let seed = seed_default_folder_editor(open_file_editor, installed_editors)?;

    // Write the seed through as the user's default the first time it resolves.
    // Left unpersisted it was recomputed on every read against the live
    // installed list, so installing an editor that sorts earlier than the
    // seeded one silently changed which IDE the button opened. Nothing is
    // written when no editor is installed: there is no suggestion to make yet,
    // and the button reveals in the file manager until there is.
    EditorSettings::handle(ctx).update(ctx, |settings, ctx| {
        report_if_error!(
            settings
                .default_folder_editor
                .set_value(EditorChoice::ExternalEditor(seed), ctx)
        );
    });

    Some(seed)
}

#[cfg(test)]
#[path = "settings_tests.rs"]
mod tests;
