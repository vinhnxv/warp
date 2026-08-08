use warp_util::path::LineAndColumnArg;

use super::{Editor, OpenFileInEditorMethod, is_warp_bundle};

#[test]
fn is_warp_bundle_recognises_warp_channels() {
    assert!(is_warp_bundle("dev.warp.Warp"));
    assert!(is_warp_bundle("dev.warp.WarpDev"));
    assert!(is_warp_bundle("dev.warp.WarpPreview"));
    assert!(is_warp_bundle("dev.warp.WarpOss"));
}

#[test]
fn is_warp_bundle_rejects_other_apps() {
    assert!(!is_warp_bundle("com.microsoft.VSCode"));
    assert!(!is_warp_bundle("com.apple.TextEdit"));
    assert!(!is_warp_bundle("dev.zed.Zed"));
    assert!(!is_warp_bundle("invalid"));
    assert!(!is_warp_bundle(""));
}

// ---------- Folder-launch behavior ----------

// URL-scheme editors (VS Code, Cursor, Windsurf) open a *file* via a
// `<scheme>://file<path>` URL routed through plain `open`. A *directory*
// cannot be opened reliably that way, so it must be launched through the
// application bundle instead (`open -a <bundle> <dir>`).
#[test]
fn dir_launch_uses_bundle_not_url_scheme_for_url_editors() {
    let tmp = tempfile::tempdir().expect("create temp dir");
    let dir = tmp.path();
    let expected_arg = dir.to_string_lossy().to_string();

    for editor in [
        Editor::VSCode,
        Editor::VSCodeInsiders,
        Editor::Cursor,
        Editor::Windsurf,
    ] {
        let (method, args) = editor.command_executable_and_arguments(None, dir);
        assert!(
            matches!(method, OpenFileInEditorMethod::FromApplicationBundleInfo),
            "expected {editor:?} to launch a directory via the app bundle, got {method:?}"
        );
        assert_eq!(args, vec![expected_arg.clone()]);
        // Must not be a `<scheme>://file<dir>` URL.
        assert!(
            !args[0].contains("://"),
            "directory argument for {editor:?} should be a plain path, got {args:?}"
        );
    }
}

// A JetBrains editor (Binary method + `--line`) and a URL-scheme editor must
// both drop line/column when the target is a directory.
#[test]
fn dir_launch_omits_line_and_column() {
    let tmp = tempfile::tempdir().expect("create temp dir");
    let dir = tmp.path();
    let expected_arg = dir.to_string_lossy().to_string();

    let line_col = Some(LineAndColumnArg {
        line_num: 42,
        column_num: Some(7),
    });

    let (method, args) = Editor::IntelliJ.command_executable_and_arguments(line_col, dir);
    assert!(
        matches!(method, OpenFileInEditorMethod::FromApplicationBundleInfo),
        "expected IntelliJ to launch a directory via the app bundle, got {method:?}"
    );
    assert_eq!(args, vec![expected_arg.clone()]);
    assert!(
        !args.iter().any(|a| a == "--line"),
        "directory launch must not inject --line, got {args:?}"
    );

    let (_, args) = Editor::VSCode.command_executable_and_arguments(line_col, dir);
    assert_eq!(
        args,
        vec![expected_arg],
        "directory launch must not append :line:col to the path"
    );
}

// ---------- JetBrains path conversion ----------

// A path that is not valid UTF-8 must not panic. macOS mounts volumes it does
// not police (exFAT, FAT32, SMB, NFS), so such a path can reach the launcher.
#[test]
fn jetbrains_command_does_not_panic_on_non_utf8_path() {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;
    use std::path::PathBuf;

    let path = PathBuf::from(OsStr::from_bytes(b"/tmp/\xff\xfeinvalid.rs"));

    let (method, args) = Editor::jetbrains_command("idea", None, &path);
    assert!(
        matches!(method, OpenFileInEditorMethod::Binary(_)),
        "expected a binary launch method, got {method:?}"
    );
    assert_eq!(args.len(), 1, "expected just the path, got {args:?}");
    assert!(
        args[0].contains('\u{fffd}'),
        "expected the invalid bytes to be replaced lossily, got {args:?}"
    );
}

#[test]
fn jetbrains_command_orders_line_flag_before_path() {
    use std::path::Path;

    let path = Path::new("/tmp/hello.rs");
    let line_col = Some(LineAndColumnArg {
        line_num: 42,
        column_num: Some(7),
    });

    let (_, args) = Editor::jetbrains_command("idea", line_col, path);
    assert_eq!(
        args,
        vec![
            "--line".to_string(),
            "42".to_string(),
            "/tmp/hello.rs".to_string(),
        ]
    );
}

// A regular file must still go through the URL-scheme path — the folder branch
// must not swallow files.
#[test]
fn file_launch_still_uses_url_scheme_for_url_editors() {
    let tmp = tempfile::tempdir().expect("create temp dir");
    let file = tmp.path().join("hello.rs");
    std::fs::write(&file, "fn main() {}").expect("write temp file");

    let (method, args) = Editor::VSCode.command_executable_and_arguments(None, &file);
    assert!(
        matches!(method, OpenFileInEditorMethod::AppUrl(None)),
        "expected VSCode to open a file via a URL scheme, got {method:?}"
    );
    assert_eq!(args.len(), 1);
    assert!(
        args[0].starts_with("vscode://file"),
        "file argument should be a vscode:// URL, got {args:?}"
    );
}
