use std::path::PathBuf;

use super::EditorMetadata;

// ---------- Folder-launch behavior ----------

// A directory is opened by launching the editor executable (resolved from the
// registry `executable_path`) directly with the folder as its sole argument —
// never through a `<scheme>://file<dir>` URL, and without any line/column.
//
// The async `Command` does not expose its program or args, so we assert against
// its `Debug` representation. Forward-slash paths are used so the paths appear
// unescaped in the `Debug` output.
#[test]
fn folder_command_launches_executable_with_folder_arg() {
    let metadata = EditorMetadata {
        executable_path: PathBuf::from("C:/Program Files/Microsoft VS Code/bin/code.exe"),
    };
    let folder = PathBuf::from("C:/Users/me/my-project");

    let command = metadata.folder_command(&folder);
    let debug = format!("{command:?}");

    assert!(
        debug.contains("code.exe"),
        "folder command should launch the editor executable, got: {debug}"
    );
    assert!(
        debug.contains("my-project"),
        "folder command should target the directory, got: {debug}"
    );
    // Must not be a `<scheme>://file<dir>` URL.
    assert!(
        !debug.contains("://"),
        "folder command must not use a URL scheme, got: {debug}"
    );
}
