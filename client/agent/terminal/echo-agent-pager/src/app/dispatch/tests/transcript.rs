//! Tests for the block viewer and transcript dispatchers.

use super::*;

fn make_test_png(width: u32, height: u32) -> Vec<u8> {
    use image::{ImageBuffer, Rgba};
    let img: ImageBuffer<Rgba<u8>, Vec<u8>> =
        ImageBuffer::from_pixel(width, height, Rgba([128, 64, 32, 255]));
    let mut buf = Vec::new();
    img.write_to(&mut std::io::Cursor::new(&mut buf), image::ImageFormat::Png)
        .unwrap();
    buf
}

fn make_test_jpeg(width: u32, height: u32) -> Vec<u8> {
    use image::{ImageBuffer, Rgb};
    let img: ImageBuffer<Rgb<u8>, Vec<u8>> =
        ImageBuffer::from_pixel(width, height, Rgb([128, 64, 32]));
    let mut buf = Vec::new();
    img.write_to(
        &mut std::io::Cursor::new(&mut buf),
        image::ImageFormat::Jpeg,
    )
    .unwrap();
    buf
}

#[test]
fn open_block_viewer_on_group_header_toggles_group() {
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    {
        let agent = app.agents.get_mut(&id).unwrap();
        let mut appearance = crate::appearance::AppearanceConfig::default();
        appearance.scrollback.display.group_max_visible = 3;
        agent.scrollback.set_appearance(appearance);
        for i in 0..6 {
            agent
                .scrollback
                .push_block(crate::scrollback::block::RenderBlock::tool_call(
                    format!("Tool{i}"),
                    "info",
                    true,
                ));
        }
        agent.scrollback.prepare_layout(80, 40);
        agent.scrollback.set_selected(Some(0));
        assert!(agent.scrollback.is_selected_group_header());
    }

    // Enter on the "N more" header expands the group instead of opening
    // the hidden first entry in the block viewer.
    dispatch(Action::OpenBlockViewer, &mut app);
    {
        let agent = app.agents.get_mut(&id).unwrap();
        assert!(
            agent.block_viewer.is_none(),
            "viewer must not open on a group header"
        );
        assert_eq!(
            agent.scrollback.selected(),
            None,
            "expanding a group clears the selection"
        );
        agent.scrollback.prepare_layout(80, 40);
        agent.scrollback.set_selected(Some(0));
        assert_eq!(
            agent.scrollback.selected_group_header_fold_label(),
            Some("collapse"),
            "entry 0 should now be the expanded group's collapse header"
        );
    }

    // Enter on the collapse header collapses the group back.
    dispatch(Action::OpenBlockViewer, &mut app);
    {
        let agent = app.agents.get_mut(&id).unwrap();
        assert!(agent.block_viewer.is_none());
        agent.scrollback.prepare_layout(80, 40);
        assert_eq!(
            agent.scrollback.selected_group_header_fold_label(),
            Some("expand"),
            "group should be truncated again ('N more' header)"
        );
    }
}

#[test]
fn open_block_viewer_opens_grep_search_block() {
    use crate::scrollback::blocks::{SearchFileMatch, SearchLineMatch};

    let mut app = test_app_with_agent();
    let id = AgentId(0);
    let agent = app.agents.get_mut(&id).unwrap();
    agent.scrollback.push_block(RenderBlock::search(
        "fn main",
        1,
        vec![SearchFileMatch {
            path: "src/main.rs".into(),
            matches: vec![SearchLineMatch {
                line_number: 1,
                content: "fn main() {}".into(),
            }],
        }],
    ));
    agent.scrollback.set_selected(Some(0));

    let entry = agent.scrollback.entry(0).unwrap();
    assert!(entry.block.has_normal_fullscreen_viewer());

    let effects = dispatch(Action::OpenBlockViewer, &mut app);
    assert!(effects.is_empty());
    let agent = app.agents.get(&id).unwrap();
    assert!(agent.block_viewer.is_some());
    assert_eq!(
        agent.block_viewer.as_ref().unwrap().kind,
        crate::views::block_viewer::ViewerKind::Grep
    );
}

#[test]
fn open_block_viewer_opens_list_dir_block() {
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    let agent = app.agents.get_mut(&id).unwrap();
    agent
        .scrollback
        .push_block(RenderBlock::list_dir_with_output("/tmp", "a.txt\nb.txt"));
    agent.scrollback.set_selected(Some(0));

    let entry = agent.scrollback.entry(0).unwrap();
    assert!(entry.block.has_normal_fullscreen_viewer());

    let effects = dispatch(Action::OpenBlockViewer, &mut app);
    assert!(effects.is_empty());
    let agent = app.agents.get(&id).unwrap();
    assert!(agent.block_viewer.is_some());
    assert_eq!(
        agent.block_viewer.as_ref().unwrap().kind,
        crate::views::block_viewer::ViewerKind::PlainText
    );
}

#[test]
fn open_block_viewer_prefers_markdown_viewer_over_image_refs() {
    use crate::terminal::image::{GraphicsProtocol, set_protocol_for_test};

    let mut app = test_app_with_agent();
    let id = AgentId(0);
    let dir = tempfile::tempdir().unwrap();
    let image_path = dir.path().join("referenced.png");
    std::fs::write(&image_path, make_test_png(20, 10)).unwrap();

    let agent = app.agents.get_mut(&id).unwrap();
    agent
        .scrollback
        .push_block(RenderBlock::agent_message(format!(
            "Here is an image: ![ref]({})",
            image_path.display()
        )));
    agent.scrollback.set_selected(Some(0));

    // Need a graphics protocol so the top-level media guard doesn't
    // short-circuit before reaching the block viewer.
    let _guard = set_protocol_for_test(GraphicsProtocol::Kitty);
    let effects = dispatch(Action::OpenBlockViewer, &mut app);

    assert!(effects.is_empty());
    let agent = app.agents.get(&id).unwrap();
    assert!(agent.block_viewer.is_some());
    assert!(agent.image_viewer.is_none());
}

#[test]
fn open_block_viewer_uses_markdown_viewer_for_agent_message_with_image_ref() {
    use crate::terminal::image::{GraphicsProtocol, set_protocol_for_test};

    let mut app = test_app_with_agent();
    let id = AgentId(0);
    let dir = tempfile::tempdir().unwrap();
    let jpg_path = dir.path().join("generated.jpg");
    std::fs::write(&jpg_path, make_test_jpeg(20, 10)).unwrap();

    let agent = app.agents.get_mut(&id).unwrap();
    agent
        .scrollback
        .push_block(RenderBlock::agent_message(format!(
            "![generated]({})",
            jpg_path.display()
        )));
    agent.scrollback.set_selected(Some(0));

    let _guard = set_protocol_for_test(GraphicsProtocol::Kitty);
    let effects = dispatch(Action::OpenBlockViewer, &mut app);

    assert!(effects.is_empty());
    let agent = app.agents.get(&id).unwrap();
    // Agent messages with image refs now open the normal markdown viewer
    // (inline media rendering moved to the tool call block).
    assert!(agent.block_viewer.is_some());
}

#[test]
fn open_block_viewer_opens_image_only_blocks_natively() {
    use crate::terminal::image::{GraphicsProtocol, set_protocol_for_test};

    let mut app = test_app_with_agent();
    let id = AgentId(0);
    let dir = tempfile::tempdir().unwrap();
    let image_path = dir.path().join("referenced.png");
    std::fs::write(&image_path, make_test_png(20, 10)).unwrap();

    let agent = app.agents.get_mut(&id).unwrap();
    agent
        .scrollback
        .push_block(RenderBlock::ToolCall(ToolCallBlock::Other(
            crate::scrollback::blocks::OtherToolCallBlock::new("image_tool", "saved image")
                .with_output(format!("Saved image: {}", image_path.display())),
        )));
    agent.scrollback.set_selected(Some(0));

    let entry = agent.scrollback.entry(0).unwrap();
    assert!(entry.block.supports_fullscreen());
    assert!(!entry.block.has_normal_fullscreen_viewer());

    // Pretend the host terminal speaks Kitty graphics so the media
    // short-circuit guard (`guard_image_support`) doesn't fire and the
    // dispatch reaches the image branch, which opens the file natively
    // rather than an in-app viewer.
    let _guard = set_protocol_for_test(GraphicsProtocol::Kitty);
    let effects = dispatch(Action::OpenBlockViewer, &mut app);

    // Generated media now opens in the OS-native viewer (fire-and-forget),
    // so neither the in-app block viewer nor image viewer is shown.
    assert!(effects.is_empty());
    let agent = app.agents.get(&id).unwrap();
    assert!(agent.block_viewer.is_none());
    assert!(agent.image_viewer.is_none());
}

// -- Plugins tab: group-collapse seeding on PluginsListLoaded --------------

fn plugins_list_response() -> echo_agent_hooks_plugins_types::PluginsListResponse {
    use crate::views::extensions_modal::test_plugin_info;
    echo_agent_hooks_plugins_types::PluginsListResponse {
        plugins: vec![
            test_plugin_info(
                "user-tool",
                Some(echo_agent_hooks_plugins_types::PluginOrigin::UserEchoAgent),
            ),
            test_plugin_info(
                "claude-tool",
                Some(echo_agent_hooks_plugins_types::PluginOrigin::UserClaude),
            ),
        ],
    }
}

fn open_plugins_modal(app: &mut AppView, id: AgentId) {
    app.agents.get_mut(&id).unwrap().extensions_modal =
        Some(crate::views::extensions_modal::ExtensionsModalState::new(
            crate::views::extensions_modal::ExtensionsTab::Plugins,
        ));
}

fn deliver_plugins_list(app: &mut AppView, id: AgentId) {
    dispatch(
        Action::TaskComplete(TaskResult::PluginsListLoaded {
            agent_id: id,
            result: Ok(plugins_list_response()),
        }),
        app,
    );
}

fn plugins_collapsed_keys(app: &AppView, id: AgentId) -> Vec<String> {
    let modal = app.agents[&id].extensions_modal.as_ref().unwrap();
    let mut keys: Vec<String> = modal.plugins_collapsed_groups.iter().cloned().collect();
    keys.sort();
    keys
}

#[test]
fn plugins_list_loaded_seeds_all_groups_collapsed_on_first_load() {
    use crate::views::extensions_modal::TabDataState;

    let mut app = test_app_with_agent();
    let id = AgentId(0);
    open_plugins_modal(&mut app, id);

    deliver_plugins_list(&mut app, id);

    assert_eq!(
        plugins_collapsed_keys(&app, id),
        vec!["origin:user".to_string(), "origin:user-claude".to_string()]
    );
    let modal = app.agents[&id].extensions_modal.as_ref().unwrap();
    match &modal.plugins_data {
        TabDataState::Loaded(response) => assert_eq!(response.plugins.len(), 2),
        other => panic!("expected Loaded plugins data, got {other:?}"),
    }
}

#[test]
fn plugins_list_delivery_seeds_once_then_always_preserves() {
    use crate::views::extensions_modal::TabDataState;

    let mut app = test_app_with_agent();
    let id = AgentId(0);
    open_plugins_modal(&mut app, id);
    deliver_plugins_list(&mut app, id);

    // User expands a group, then the post-action refetch arrives.
    app.agents
        .get_mut(&id)
        .unwrap()
        .extensions_modal
        .as_mut()
        .unwrap()
        .plugins_collapsed_groups
        .remove("origin:user");
    deliver_plugins_list(&mut app, id);

    assert_eq!(
        plugins_collapsed_keys(&app, id),
        vec!["origin:user-claude".to_string()],
        "post-action refetch must not re-collapse an expanded group"
    );

    // Reload sets Loading, but seeding is once-per-modal: still preserves.
    app.agents
        .get_mut(&id)
        .unwrap()
        .extensions_modal
        .as_mut()
        .unwrap()
        .plugins_data = TabDataState::Loading;
    deliver_plugins_list(&mut app, id);

    assert_eq!(
        plugins_collapsed_keys(&app, id),
        vec!["origin:user-claude".to_string()],
        "reload must not re-collapse groups the user expanded"
    );
}

#[test]
fn open_block_viewer_skips_image_viewer_when_no_graphics() {
    use crate::terminal::image::{GraphicsProtocol, set_protocol_for_test};

    let mut app = test_app_with_agent();
    let id = AgentId(0);
    let dir = tempfile::tempdir().unwrap();
    let image_path = dir.path().join("referenced.png");
    std::fs::write(&image_path, make_test_png(20, 10)).unwrap();

    let agent = app.agents.get_mut(&id).unwrap();
    agent
        .scrollback
        .push_block(RenderBlock::ToolCall(ToolCallBlock::Other(
            crate::scrollback::blocks::OtherToolCallBlock::new("image_tool", "saved image")
                .with_output(format!("Saved image: {}", image_path.display())),
        )));
    agent.scrollback.set_selected(Some(0));

    // Terminal has no inline-image protocol (e.g. Windows / ConPTY).
    // The dispatch should refuse to open the image-viewer modal and
    // surface the situation via a toast instead.
    let _guard = set_protocol_for_test(GraphicsProtocol::None);
    let effects = dispatch(Action::OpenBlockViewer, &mut app);

    assert!(effects.is_empty());
    let agent = app.agents.get(&id).unwrap();
    assert!(agent.block_viewer.is_none());
    assert!(
        agent.image_viewer.is_none(),
        "image_viewer modal should not open on terminals without a graphics protocol"
    );
}
