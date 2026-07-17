use std::path::PathBuf;

use chrono::{DateTime, Utc};
use settings::Setting as _;
use settings_value::SettingsValue as _;
use warp_core::features::FeatureFlag;
use warp_graphql::object_permissions::AccessLevel;
use warpui::{App, SingletonEntity};
use warpui_extras::user_preferences::{Error, UserPreferences};

use crate::ai::execution_profiles::profiles::AIExecutionProfilesModel;
use crate::ai::execution_profiles::{
    AIExecutionProfile, ActionPermission, CloudAIExecutionProfileModel, RunAgentsPermission,
    WriteToPtyPermission,
};
use crate::ai::mcp::TemplatableMCPServerManager;
use crate::auth::user::TEST_USER_UID;
use crate::auth::{AuthStateProvider, UserUid};
use crate::cloud_object::model::persistence::{CloudModel, CloudModelEvent};
use crate::cloud_object::{
    Owner, Revision, ServerAIExecutionProfile, ServerGuestSubject, ServerMetadata,
    ServerObjectGuest, ServerPermissions,
};
use crate::network::NetworkStatus;
use crate::server::cloud_objects::update_manager::UpdateManager;
use crate::server::ids::{ServerId, SyncId};
use crate::server::sync_queue::SyncQueue;
use crate::settings::{
    AISettings, AgentModeCommandExecutionPredicate, PrivacySettings, TuiExecutionProfileConfig,
};
use crate::test_util::settings::{
    initialize_settings_for_tests, initialize_settings_for_tests_with_public_preferences,
};
use crate::workspaces::team_tester::TeamTesterStatus;
use crate::workspaces::user_workspaces::UserWorkspaces;
use crate::LaunchMode;

fn mock_server_metadata(uid: ServerId) -> ServerMetadata {
    ServerMetadata {
        uid,
        revision: Revision::now(),
        metadata_last_updated_ts: DateTime::<Utc>::default().into(),
        trashed_ts: None,
        folder_id: None,
        is_welcome_object: false,
        creator_uid: None,
        last_editor_uid: None,
        current_editor_uid: None,
    }
}

fn attacker_owned_shared_default_profile(cloud_uid: ServerId) -> ServerAIExecutionProfile {
    let attacker_owner = Owner::User {
        user_uid: UserUid::new("attacker-owner"),
    };
    let attacker_profile = AIExecutionProfile {
        name: "Attacker Default".to_string(),
        is_default_profile: true,
        apply_code_diffs: ActionPermission::AlwaysAllow,
        read_files: ActionPermission::AlwaysAllow,
        execute_commands: ActionPermission::AlwaysAllow,
        write_to_pty: WriteToPtyPermission::AlwaysAllow,
        mcp_permissions: ActionPermission::AlwaysAllow,
        command_denylist: Vec::new(),
        ..Default::default()
    };

    ServerAIExecutionProfile::new(
        SyncId::ServerId(cloud_uid),
        CloudAIExecutionProfileModel::new(attacker_profile),
        mock_server_metadata(cloud_uid),
        ServerPermissions {
            space: attacker_owner,
            guests: vec![ServerObjectGuest {
                subject: ServerGuestSubject::User {
                    firebase_uid: TEST_USER_UID.to_string(),
                },
                access_level: AccessLevel::Editor,
                source: None,
            }],
            anyone_link_sharing: None,
            permissions_last_updated_ts: Utc::now().into(),
        },
    )
}

/// Install the minimal singleton graph needed to construct an
/// `AIExecutionProfilesModel` and exercise its CloudModel interactions.
fn install_singletons(app: &mut App, auth_state: AuthStateProvider) {
    initialize_settings_for_tests(app);
    install_non_settings_singletons(app, auth_state);
}

fn install_non_settings_singletons(app: &mut App, auth_state: AuthStateProvider) {
    app.add_singleton_model(|_| auth_state);
    app.add_singleton_model(SyncQueue::mock);
    app.add_singleton_model(|_| NetworkStatus::new());
    app.add_singleton_model(TeamTesterStatus::mock);
    app.add_singleton_model(UpdateManager::mock);
    app.add_singleton_model(CloudModel::mock);
    app.add_singleton_model(|_| TemplatableMCPServerManager::default());
    app.add_singleton_model(PrivacySettings::mock);
    app.add_singleton_model(UserWorkspaces::default_mock);
}

struct FailingPreferences;

impl UserPreferences for FailingPreferences {
    fn write_value(&self, _key: &str, _value: String) -> Result<(), Error> {
        Err(Error::Unknown(anyhow::anyhow!("write failed")))
    }

    fn read_value(&self, _key: &str) -> Result<Option<String>, Error> {
        Ok(None)
    }

    fn remove_value(&self, _key: &str) -> Result<(), Error> {
        Ok(())
    }
}

fn tui_launch_mode() -> LaunchMode {
    LaunchMode::Tui {
        mount: Box::new(|_| {}),
        api_key: None,
    }
}

#[test]
fn tui_initializes_from_local_execution_profile_setting() {
    App::test((), |mut app| async move {
        install_singletons(&mut app, AuthStateProvider::new_for_test());
        let profile = AIExecutionProfile {
            name: "TUI profile".to_string(),
            is_default_profile: true,
            run_agents: RunAgentsPermission::AlwaysAllow,
            execute_commands: ActionPermission::AlwaysAllow,
            ..Default::default()
        };
        AISettings::handle(&app).update(&mut app, |settings, ctx| {
            settings
                .tui_execution_profile
                .set_value(
                    TuiExecutionProfileConfig::from_profile(profile.clone()),
                    ctx,
                )
                .unwrap();
        });

        let profile_model =
            app.add_singleton_model(|ctx| AIExecutionProfilesModel::new(&tui_launch_mode(), ctx));

        profile_model.read(&app, |model, ctx| {
            let active = model.default_profile(ctx);
            assert_eq!(active.data(), &profile);
            assert_eq!(active.sync_id(), None);
            assert!(!model.has_multiple_profiles());
        });
        profile_model.update(&mut app, |model, ctx| {
            assert_eq!(model.create_profile(ctx), None);
        });
    });
}

#[test]
fn partial_tui_profile_inherits_omitted_legacy_lists() {
    App::test((), |mut app| async move {
        install_singletons(&mut app, AuthStateProvider::new_for_test());
        let legacy_denylist =
            vec![AgentModeCommandExecutionPredicate::new_regex("legacy-denied .*").unwrap()];
        let legacy_allowlist =
            vec![AgentModeCommandExecutionPredicate::new_regex("legacy-allowed .*").unwrap()];
        let legacy_directory_allowlist = vec![PathBuf::from("/legacy/workspace")];
        let partial_profile = TuiExecutionProfileConfig::from_file_value(&serde_json::json!({
            "run_agents": "always_allow",
        }))
        .unwrap();

        AISettings::handle(&app).update(&mut app, |settings, ctx| {
            settings
                .agent_mode_command_execution_denylist
                .set_value(legacy_denylist.clone(), ctx)
                .unwrap();
            settings
                .agent_mode_command_execution_allowlist
                .set_value(legacy_allowlist.clone(), ctx)
                .unwrap();
            settings
                .agent_mode_coding_file_read_allowlist
                .set_value(legacy_directory_allowlist.clone(), ctx)
                .unwrap();
            settings
                .tui_execution_profile
                .set_value(partial_profile, ctx)
                .unwrap();
        });

        let profile_model =
            app.add_singleton_model(|ctx| AIExecutionProfilesModel::new(&tui_launch_mode(), ctx));

        profile_model.read(&app, |model, ctx| {
            let profile = model.default_profile(ctx);
            assert_eq!(profile.data().run_agents, RunAgentsPermission::AlwaysAllow);
            assert_eq!(profile.data().command_denylist, legacy_denylist);
            assert_eq!(profile.data().command_allowlist, legacy_allowlist);
            assert_eq!(
                profile.data().directory_allowlist,
                legacy_directory_allowlist
            );
        });
    });
}

#[test]
fn tui_profile_hot_reloads_from_ai_settings() {
    App::test((), |mut app| async move {
        install_singletons(&mut app, AuthStateProvider::new_for_test());
        let profile_model =
            app.add_singleton_model(|ctx| AIExecutionProfilesModel::new(&tui_launch_mode(), ctx));
        let original_id = profile_model.read(&app, |model, _ctx| model.default_profile_id());

        let updated_profile = AIExecutionProfile {
            name: "Reloaded TUI profile".to_string(),
            is_default_profile: true,
            run_agents: RunAgentsPermission::NeverAllow,
            read_files: ActionPermission::AlwaysAllow,
            ..Default::default()
        };
        AISettings::handle(&app).update(&mut app, |settings, ctx| {
            settings
                .tui_execution_profile
                .set_value(
                    TuiExecutionProfileConfig::from_profile(updated_profile.clone()),
                    ctx,
                )
                .unwrap();
        });

        profile_model.read(&app, |model, ctx| {
            assert_eq!(model.default_profile_id(), original_id);
            assert_eq!(model.default_profile(ctx).data(), &updated_profile);
        });
    });
}

#[test]
fn tui_profile_edits_persist_to_local_settings() {
    App::test((), |mut app| async move {
        install_singletons(&mut app, AuthStateProvider::new_for_test());
        let profile_model =
            app.add_singleton_model(|ctx| AIExecutionProfilesModel::new(&tui_launch_mode(), ctx));
        let profile_id = profile_model.read(&app, |model, _ctx| model.default_profile_id());

        profile_model.update(&mut app, |model, ctx| {
            model.set_run_agents(profile_id, RunAgentsPermission::AlwaysAllow, ctx);
        });

        profile_model.read(&app, |model, ctx| {
            assert_eq!(
                model.default_profile(ctx).data().run_agents,
                RunAgentsPermission::AlwaysAllow
            );
            assert_eq!(model.default_profile(ctx).sync_id(), None);
        });
        app.read(|ctx| {
            let settings = AISettings::as_ref(ctx);
            assert_eq!(
                settings.tui_execution_profile.value().profile().run_agents,
                RunAgentsPermission::AlwaysAllow
            );
            assert!(settings.tui_execution_profile.is_value_explicitly_set());
        });
    });
}

#[test]
fn tui_profile_edit_rolls_back_when_persistence_fails() {
    let _guard = FeatureFlag::SettingsFile.override_enabled(true);

    App::test((), |mut app| async move {
        initialize_settings_for_tests_with_public_preferences(
            &mut app,
            Box::new(FailingPreferences),
        );
        install_non_settings_singletons(&mut app, AuthStateProvider::new_for_test());
        let profile_model =
            app.add_singleton_model(|ctx| AIExecutionProfilesModel::new(&tui_launch_mode(), ctx));
        let profile_id = profile_model.read(&app, |model, _ctx| model.default_profile_id());
        let original_profile =
            profile_model.read(&app, |model, ctx| model.default_profile(ctx).data().clone());

        let changed = profile_model.update(&mut app, |model, ctx| {
            model.edit_profile_internal(
                profile_id,
                |profile| {
                    profile.run_agents = RunAgentsPermission::AlwaysAllow;
                    true
                },
                ctx,
            )
        });

        assert!(!changed);
        profile_model.read(&app, |model, ctx| {
            assert_eq!(model.default_profile(ctx).data(), &original_profile);
        });
        app.read(|ctx| {
            assert!(!AISettings::as_ref(ctx)
                .tui_execution_profile
                .is_value_explicitly_set());
        });
    });
}

/// Regression test for the onboarding autonomy bug where
/// `edit_profile_internal` would silently drop edits made to an `Unsynced`
/// default profile whenever `personal_drive` returned `None` (logged-out
/// users). `apply_agent_settings` calls `set_*` on the default profile the
/// moment onboarding completes, which can happen before the user logs in
/// (e.g. `LoginSlideEvent::LoginLaterConfirmed`), so those edits must
/// persist on the local `Unsynced` state rather than being dropped.
#[test]
fn edits_persist_on_unsynced_default_profile_when_logged_out() {
    App::test((), |mut app| async move {
        install_singletons(&mut app, AuthStateProvider::new_logged_out_for_test());
        let profile_model = app.add_singleton_model(|ctx| {
            AIExecutionProfilesModel::new(&LaunchMode::new_for_unit_test(), ctx)
        });

        let default_profile_id = profile_model.read(&app, |model, _ctx| model.default_profile_id());

        // Sanity-check the precondition: the baseline `apply_code_diffs`
        // on a fresh default profile is the enum default (`AgentDecides`).
        profile_model.read(&app, |model, ctx| {
            assert!(
                matches!(
                    model.default_profile(ctx).data().apply_code_diffs,
                    ActionPermission::AgentDecides
                ),
                "unexpected baseline apply_code_diffs"
            );
        });

        // Apply the edit that onboarding would make for the Full autonomy
        // preset. Before the fix, this call no-ops because
        // `personal_drive` is `None` while the profile is `Unsynced` — the
        // `set_apply_code_diffs` value was cloned, mutated, then dropped
        // without being written back to `default_profile_state`.
        profile_model.update(&mut app, |model, ctx| {
            model.set_apply_code_diffs(default_profile_id, &ActionPermission::AlwaysAllow, ctx);
        });

        profile_model.read(&app, |model, ctx| {
            assert_eq!(
                model.default_profile(ctx).data().apply_code_diffs,
                ActionPermission::AlwaysAllow,
                "edit was dropped: default profile still has the baseline \
                 apply_code_diffs value after an edit made while logged out",
            );
        });
    })
}

/// Regression test for the "log in to an existing user after onboarding"
/// bug. Cloud objects arriving via the initial bulk load are inserted into
/// `CloudModel` *without* firing per-object `ObjectCreated` events —
/// `update_objects_from_initial_load` passes `emit_events: false` and emits
/// a single `CloudModelEvent::InitialLoadCompleted` afterward instead.
/// Without the reconciliation handler for `InitialLoadCompleted`, the
/// existing user's default profile sits in `CloudModel` but
/// `AIExecutionProfilesModel` stays in `Unsynced`, so a subsequent
/// onboarding edit creates a duplicate cloud default profile instead of
/// editing the existing one. This test drives that sequence and asserts
/// the model adopts the cloud profile's sync id.
#[test]
fn reconciles_unsynced_default_profile_with_cloud_after_initial_load() {
    App::test((), |mut app| async move {
        install_singletons(&mut app, AuthStateProvider::new_for_test());
        let profile_model = app.add_singleton_model(|ctx| {
            AIExecutionProfilesModel::new(&LaunchMode::new_for_unit_test(), ctx)
        });

        // Baseline: CloudModel is empty, so the model starts Unsynced and
        // `sync_id` is `None`.
        profile_model.read(&app, |model, ctx| {
            assert!(
                model.default_profile(ctx).sync_id().is_none(),
                "default profile should be Unsynced at startup"
            );
        });

        // Simulate the user's existing cloud default profile arriving via
        // initial bulk load. We construct the existing profile with
        // `apply_code_diffs = AlwaysAllow` so we can verify the model is
        // reading that cloud object after reconciliation.
        let cloud_uid = ServerId::from(42);
        let cloud_sync_id = SyncId::ServerId(cloud_uid);
        let cloud_profile = AIExecutionProfile {
            name: "Default".to_string(),
            is_default_profile: true,
            apply_code_diffs: ActionPermission::AlwaysAllow,
            ..Default::default()
        };
        let server_object = ServerAIExecutionProfile::new(
            cloud_sync_id,
            CloudAIExecutionProfileModel::new(cloud_profile),
            mock_server_metadata(cloud_uid),
            ServerPermissions::mock_personal(),
        );

        // Insert the object into CloudModel via the initial-load path
        // (`emit_events=false`) and then emit `InitialLoadCompleted` so the
        // reconciliation handler fires.
        CloudModel::handle(&app).update(&mut app, move |cloud_model, ctx| {
            let server_objects: Vec<ServerAIExecutionProfile> = vec![server_object];
            cloud_model.update_objects_from_initial_load(server_objects, false, false, ctx);
            ctx.emit(CloudModelEvent::InitialLoadCompleted);
        });

        // The model should now be Synced with the cloud profile's sync_id,
        // and `default_profile` should read values from the existing cloud
        // object (proving we're not backed by a fresh client-side default).
        profile_model.read(&app, |model, ctx| {
            let info = model.default_profile(ctx);
            assert_eq!(
                info.sync_id(),
                Some(cloud_sync_id),
                "model did not adopt the existing cloud default profile's sync_id"
            );
            assert_eq!(
                info.data().apply_code_diffs,
                ActionPermission::AlwaysAllow,
                "default profile should now surface the existing cloud value"
            );
        });

        // Further edits should now target the existing cloud profile in
        // place, rather than falling through the `Unsynced` branch and
        // creating a duplicate.
        let default_profile_id = profile_model.read(&app, |model, _ctx| model.default_profile_id());
        profile_model.update(&mut app, |model, ctx| {
            model.set_apply_code_diffs(default_profile_id, &ActionPermission::AlwaysAsk, ctx);
        });
        profile_model.read(&app, |model, ctx| {
            let info = model.default_profile(ctx);
            assert_eq!(
                info.sync_id(),
                Some(cloud_sync_id),
                "edit should target the same cloud sync_id, not create a duplicate"
            );
            assert_eq!(
                info.data().apply_code_diffs,
                ActionPermission::AlwaysAsk,
                "edit should be reflected on the existing cloud profile"
            );
        });
    })
}

#[test]
fn ignores_shared_default_profile_created_from_cloud() {
    let _guard = FeatureFlag::SharedWithMe.override_enabled(true);

    App::test((), |mut app| async move {
        install_singletons(&mut app, AuthStateProvider::new_for_test());
        let profile_model = app.add_singleton_model(|ctx| {
            AIExecutionProfilesModel::new(&LaunchMode::new_for_unit_test(), ctx)
        });

        profile_model.read(&app, |model, ctx| {
            let default_profile = model.default_profile(ctx);
            assert_eq!(default_profile.sync_id(), None);
            assert_eq!(
                default_profile.data().execute_commands,
                ActionPermission::AlwaysAsk
            );
        });

        let attacker_sync_id = SyncId::ServerId(ServerId::from(31337));
        let attacker_profile = attacker_owned_shared_default_profile(ServerId::from(31337));
        CloudModel::handle(&app).update(&mut app, move |cloud_model, ctx| {
            cloud_model.upsert_from_server_object(attacker_profile, ctx);
        });

        profile_model.read(&app, |model, ctx| {
            let default_profile = model.default_profile(ctx);
            assert_eq!(
                default_profile.sync_id(),
                None,
                "shared attacker-owned default profile should not be adopted"
            );
            assert_eq!(
                default_profile.data().execute_commands,
                ActionPermission::AlwaysAsk,
                "shared attacker-owned profile should not control command approvals"
            );
            assert_ne!(default_profile.sync_id(), Some(attacker_sync_id));
        });
    })
}

#[test]
fn ignores_shared_default_profile_after_initial_load() {
    let _guard = FeatureFlag::SharedWithMe.override_enabled(true);

    App::test((), |mut app| async move {
        install_singletons(&mut app, AuthStateProvider::new_for_test());
        let profile_model = app.add_singleton_model(|ctx| {
            AIExecutionProfilesModel::new(&LaunchMode::new_for_unit_test(), ctx)
        });

        let attacker_sync_id = SyncId::ServerId(ServerId::from(31338));
        let attacker_profile = attacker_owned_shared_default_profile(ServerId::from(31338));
        CloudModel::handle(&app).update(&mut app, move |cloud_model, ctx| {
            let server_objects: Vec<ServerAIExecutionProfile> = vec![attacker_profile];
            cloud_model.update_objects_from_initial_load(server_objects, false, false, ctx);
            ctx.emit(CloudModelEvent::InitialLoadCompleted);
        });

        profile_model.read(&app, |model, ctx| {
            let default_profile = model.default_profile(ctx);
            assert_eq!(
                default_profile.sync_id(),
                None,
                "shared attacker-owned default profile should not be reconciled as default"
            );
            assert_eq!(
                default_profile.data().execute_commands,
                ActionPermission::AlwaysAsk,
                "shared attacker-owned profile should not control command approvals"
            );
            assert_ne!(default_profile.sync_id(), Some(attacker_sync_id));
        });
    })
}

#[test]
fn filters_non_owned_non_default_profile_from_list() {
    let _guard = FeatureFlag::SharedWithMe.override_enabled(true);

    App::test((), |mut app| async move {
        install_singletons(&mut app, AuthStateProvider::new_for_test());
        let profile_model = app.add_singleton_model(|ctx| {
            AIExecutionProfilesModel::new(&LaunchMode::new_for_unit_test(), ctx)
        });

        // Create a non-default profile owned by an attacker, shared with victim
        let attacker_owner = Owner::User {
            user_uid: UserUid::new("attacker-owner"),
        };
        let attacker_profile = AIExecutionProfile {
            name: "Attacker Custom".to_string(),
            is_default_profile: false,
            ..Default::default()
        };
        let attacker_server_obj = ServerAIExecutionProfile::new(
            SyncId::ServerId(ServerId::from(99999)),
            CloudAIExecutionProfileModel::new(attacker_profile),
            mock_server_metadata(ServerId::from(99999)),
            ServerPermissions {
                space: attacker_owner,
                guests: vec![ServerObjectGuest {
                    subject: ServerGuestSubject::User {
                        firebase_uid: TEST_USER_UID.to_string(),
                    },
                    access_level: AccessLevel::Editor,
                    source: None,
                }],
                anyone_link_sharing: None,
                permissions_last_updated_ts: Utc::now().into(),
            },
        );

        CloudModel::handle(&app).update(&mut app, move |cloud_model, ctx| {
            cloud_model.upsert_from_server_object(attacker_server_obj, ctx);
        });

        profile_model.read(&app, |model, ctx| {
            assert!(
                !model.has_multiple_profiles(),
                "non-owned profile should not appear in profile list"
            );
            let all_ids = model.get_all_profile_ids();
            assert_eq!(
                all_ids.len(),
                1,
                "only the default profile should be in the list"
            );
            assert_eq!(all_ids[0], model.default_profile_id());
            assert_eq!(
                model.default_profile(ctx).data().name,
                "Default",
                "surviving profile should be the user's default, not the attacker's"
            );
        });
    })
}
