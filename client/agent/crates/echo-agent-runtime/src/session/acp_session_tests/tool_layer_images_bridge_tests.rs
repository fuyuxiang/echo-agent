//! Wiring tests for MCP tool-layer images through `handle_bridge_tool_success`.
use super::support::*;
use super::*;
use base64::Engine as _;
use echo_agent_sampling_types::{ContentPart, ConversationItem};
use echo_agent_tools::types::output::{MCPOutput, ToolOutput, ToolRunResult};
use echo_agent_tools::util::base64_images::{ExtractedImage, IMAGE_CONTENT_PLACEHOLDER};
/// 32×32 solid PNG — above vision min side/area so normalize keeps it.
fn vision_ok_png_b64() -> String {
    use image::{ImageBuffer, Rgba};
    let img: ImageBuffer<Rgba<u8>, Vec<u8>> =
        ImageBuffer::from_pixel(32, 32, Rgba([128, 64, 32, 255]));
    let mut buf = Vec::new();
    img.write_to(&mut std::io::Cursor::new(&mut buf), image::ImageFormat::Png)
        .expect("encode png");
    base64::engine::general_purpose::STANDARD.encode(buf)
}
fn mcp_screenshot_result(payload_b64: &str) -> ToolRunResult {
    let mut mcp = MCPOutput::okay_output(
        "browser_screenshot".into(),
        "browser-use".into(),
        IMAGE_CONTENT_PLACEHOLDER.into(),
    );
    mcp.extracted_images = vec![ExtractedImage {
        data: payload_b64.to_owned(),
        mime_type: "image/png".into(),
    }];
    ToolRunResult {
        output: ToolOutput::MCP(mcp),
        prompt_text: IMAGE_CONTENT_PLACEHOLDER.into(),
        effective_tool_name: None,
    }
}
fn tool_result_text(item: &ConversationItem) -> &str {
    match item {
        ConversationItem::ToolResult(tr) => tr.content.as_ref(),
        other => panic!("expected ToolResult, got {other:?}"),
    }
}
fn followup_has_data_image(followups: &[ConversationItem]) -> bool {
    followups.iter().any(|item| match item {
        ConversationItem::User(u) => u
            .content
            .iter()
            .any(|p| matches!(p, ContentPart::Image { url } if url.starts_with("data:image/"))),
        _ => false,
    })
}
/// Multimodal: drained MCP image becomes a deferred vision follow-up; tool text keeps placeholder.
#[tokio::test(flavor = "current_thread")]
async fn handle_bridge_tool_success_multimodal_mcp_image_deferred_followup() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _) = tokio::sync::mpsc::unbounded_channel::<
                echo_agent_acp::AcpClientMessage,
            >();
            let (persistence_tx, _) = tokio::sync::mpsc::unbounded_channel::<
                PersistenceMsg,
            >();
            let actor = create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx)
                .await;
            assert!(!actor.is_cursor_harness());
            let payload = vision_ok_png_b64();
            let parsed_args = serde_json::json!({});
            let followups = actor
                .handle_bridge_tool_success(
                    &acp::ToolCallId::new("tc-mcp-img"),
                    "tc-mcp-img",
                    "browser_screenshot",
                    "browser_screenshot",
                    DrainedToolSuccess::new(mcp_screenshot_result(&payload)),
                    0,
                    "test-model",
                    &parsed_args,
                )
                .await
                .expect("bridge success");
            assert!(
                followup_has_data_image(&followups),
                "multimodal must attach drained MCP image as deferred vision follow-up: {followups:?}"
            );
            assert!(
                followups.iter().any(|item| matches!(
                    item,
                    ConversationItem::User(u) if u
                        .content
                        .iter()
                        .any(|p| matches!(p, ContentPart::Text { text } if text.contains("Image extracted from tool result")))
                )),
                "expected extracted-image caption: {followups:?}"
            );
            let conv = actor.chat_state_handle.get_conversation().await;
            let tool = conv
                .iter()
                .rev()
                .find(|i| matches!(i, ConversationItem::ToolResult(_)))
                .expect("tool result pushed");
            let text = tool_result_text(tool);
            assert!(
                text.contains(IMAGE_CONTENT_PLACEHOLDER),
                "placeholder stays in tool text: {text}"
            );
            assert!(
                !text.contains("data:image"),
                "tool text must not reinject data URI: {text}"
            );
            assert!(
                !text.contains("image omitted"),
                "no budget-omit copy: {text}"
            );
        })
        .await;
}
