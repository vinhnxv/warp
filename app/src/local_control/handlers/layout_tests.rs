use ::local_control::protocol::TargetSelector;
use ::local_control::{ErrorCode, InstanceId};
use warpui::App;

use super::{create_tab, tab_create_action};
use crate::local_control::LocalControlBridge;
use crate::workspace::view::tests::{initialize_app, mock_workspace};
use crate::workspace::{RepoModeAutoConnect, WorkspaceAction};

/// The bridge has no action that runs a command, so a tab it creates must not
/// bring one with it: with a remote repository selected, an auto-connecting tab
/// would put an `ssh` on the wire for a caller that cannot otherwise execute
/// anything.
#[test]
fn tab_create_terminal_tab_does_not_connect_itself() {
    let action =
        tab_create_action(&serde_json::json!({})).expect("a terminal tab.create builds an action");
    assert!(
        matches!(
            action,
            WorkspaceAction::AddTerminalTab {
                auto_connect: RepoModeAutoConnect::Suppress,
                ..
            }
        ),
        "expected a suppressed tab, got {action:?}"
    );
}

#[test]
fn tab_create_handler_adds_and_activates_terminal_tab() {
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        let workspace = mock_workspace(&mut app);
        let previous_count = workspace.read(&app, |workspace, _| workspace.tab_count());
        let bridge = app.add_singleton_model(LocalControlBridge::new);
        let instance_id = InstanceId("inst_test".to_owned());

        let response = bridge.update(&mut app, |bridge, ctx| {
            bridge.set_instance_id(instance_id.clone());
            create_tab(
                &Some(instance_id.clone()),
                &serde_json::json!({}),
                &TargetSelector::default(),
                ctx,
            )
            .expect("tab.create handler succeeds")
        });

        workspace.read(&app, |workspace, _| {
            assert_eq!(workspace.tab_count(), previous_count + 1);
            assert_eq!(workspace.active_tab_index(), previous_count);
        });
        assert_eq!(response["action"], "tab.create");
        assert_eq!(response["created"], true);
        assert_eq!(response["instance_id"], "inst_test");
        assert_eq!(response["tab"]["previous_count"], previous_count);
        assert_eq!(response["tab"]["count"], previous_count + 1);
        assert_eq!(response["tab"]["active_index"], previous_count);
        assert!(response["tab"]["id"].is_string());
    });
}

#[test]
fn tab_create_rejects_shell_parameter() {
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        let _workspace = mock_workspace(&mut app);
        let bridge = app.add_singleton_model(LocalControlBridge::new);
        let instance_id = InstanceId("inst_test".to_owned());

        let err = bridge.update(&mut app, |bridge, ctx| {
            bridge.set_instance_id(instance_id.clone());
            create_tab(
                &Some(instance_id.clone()),
                &serde_json::json!({ "shell": "zsh" }),
                &TargetSelector::default(),
                ctx,
            )
            .expect_err("shell parameter must be rejected")
        });

        assert_eq!(err.code, ErrorCode::InvalidParams);
    });
}
