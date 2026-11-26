#![cfg_attr(rustfmt, rustfmt::skip)]
    use super::*;

    #[test]
    fn plugins_changed_update_refreshes_data_without_reseeding_collapse_state() {
        use crate::views::extensions_modal::{ExtensionsModalState, ExtensionsTab, TabDataState};

        let mut app = make_app_with_agent("sess-plugins");
        let mut modal = ExtensionsModalState::new(ExtensionsTab::Plugins);
        modal.plugins_data =
            TabDataState::Loaded(echo_agent_hooks_plugins_types::PluginsListResponse { plugins: vec![] });
        modal.plugins_groups_seeded = true;
        modal
            .plugins_collapsed_groups
            .insert("origin:user-claude".into());
        app.agents.get_mut(&AgentId(0)).unwrap().extensions_modal = Some(modal);

        let handled = handle(
            make_ext_session_notification(
                "sess-plugins",
                EchoAgentSessionUpdate::PluginsChanged {
                    plugins: vec![crate::views::extensions_modal::test_plugin_info(
                        "user-tool",
                        Some(echo_agent_hooks_plugins_types::PluginOrigin::UserEchoAgent),
                    )],
                },
            ),
            &mut app,
        );
        assert!(handled);

        let modal = app.agents[&AgentId(0)].extensions_modal.as_ref().unwrap();
        match &modal.plugins_data {
            TabDataState::Loaded(response) => {
                assert_eq!(response.plugins.len(), 1);
                assert_eq!(response.plugins[0].name, "user-tool");
            }
            other => panic!("expected Loaded plugins data, got {other:?}"),
        }
        assert_eq!(
            modal.plugins_collapsed_groups,
            std::collections::HashSet::from(["origin:user-claude".to_string()]),
            "live PluginsChanged refresh must not touch collapse state"
        );
    }

    #[test]
    fn plugins_changed_seeds_collapse_when_it_wins_the_first_load_race() {
        use crate::views::extensions_modal::{ExtensionsModalState, ExtensionsTab, TabDataState};

        // Fresh modal: the push lands before the initial list fetch returns.
        let mut app = make_app_with_agent("sess-plugins");
        app.agents.get_mut(&AgentId(0)).unwrap().extensions_modal =
            Some(ExtensionsModalState::new(ExtensionsTab::Plugins));

        let handled = handle(
            make_ext_session_notification(
                "sess-plugins",
                EchoAgentSessionUpdate::PluginsChanged {
                    plugins: vec![
                        crate::views::extensions_modal::test_plugin_info(
                            "user-tool",
                            Some(echo_agent_hooks_plugins_types::PluginOrigin::UserEchoAgent),
                        ),
                        crate::views::extensions_modal::test_plugin_info(
                            "claude-tool",
                            Some(echo_agent_hooks_plugins_types::PluginOrigin::UserClaude),
                        ),
                    ],
                },
            ),
            &mut app,
        );
        assert!(handled);

        let modal = app.agents[&AgentId(0)].extensions_modal.as_ref().unwrap();
        assert_eq!(
            modal.plugins_collapsed_groups,
            std::collections::HashSet::from([
                "origin:user".to_string(),
                "origin:user-claude".to_string()
            ]),
            "push winning the first-load race must seed the collapsed default"
        );
        match &modal.plugins_data {
            TabDataState::Loaded(response) => assert_eq!(response.plugins.len(), 2),
            other => panic!("expected Loaded plugins data, got {other:?}"),
        }

        // The push counts as the one seeding: expand a group, deliver again,
        // and the expansion must survive.
        app.agents
            .get_mut(&AgentId(0))
            .unwrap()
            .extensions_modal
            .as_mut()
            .unwrap()
            .plugins_collapsed_groups
            .remove("origin:user");
        let handled = handle(
            make_ext_session_notification(
                "sess-plugins",
                EchoAgentSessionUpdate::PluginsChanged {
                    plugins: vec![crate::views::extensions_modal::test_plugin_info(
                        "user-tool",
                        Some(echo_agent_hooks_plugins_types::PluginOrigin::UserEchoAgent),
                    )],
                },
            ),
            &mut app,
        );
        assert!(handled);
        let modal = app.agents[&AgentId(0)].extensions_modal.as_ref().unwrap();
        assert_eq!(
            modal.plugins_collapsed_groups,
            std::collections::HashSet::from(["origin:user-claude".to_string()]),
            "deliveries after the push-seed must preserve expand state"
        );
    }

