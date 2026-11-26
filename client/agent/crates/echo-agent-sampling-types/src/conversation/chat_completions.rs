//! Chat Completions wire format.

use super::*;

impl From<ChatRequestMessage> for ConversationItem {
    fn from(msg: ChatRequestMessage) -> Self {
        match msg.role {
            Role::System => ConversationItem::System(SystemItem {
                content: Arc::<str>::from(msg.text_content()),
            }),
            Role::User => {
                let parts = msg
                    .content
                    .blocks()
                    .into_iter()
                    .map(|block| match block {
                        ChatContentBlock::Text { text } => ContentPart::Text {
                            text: Arc::<str>::from(text),
                        },
                        ChatContentBlock::ImageUrl { image_url } => ContentPart::Image {
                            url: Arc::<str>::from(image_url.url),
                        },
                    })
                    .collect();
                ConversationItem::User(UserItem {
                    content: parts,
                    synthetic_reason: None,
                    ..Default::default()
                })
            }
            Role::Assistant => {
                // Reasoning is a sibling item, which a single-item conversion
                // cannot emit, so it is dropped here.
                let content = msg.text_content();
                let model_id = msg.model_id;

                let tool_calls: Vec<ToolCall> = msg
                    .tool_calls
                    .into_iter()
                    .map(|tc| ToolCall {
                        id: Arc::<str>::from(tc.id.unwrap_or_default()),
                        name: tc.function.name,
                        arguments: Arc::<str>::from(tc.function.arguments),
                    })
                    .collect();

                ConversationItem::Assistant(AssistantItem {
                    content: Arc::<str>::from(content),
                    tool_calls,
                    model_id,
                    model_fingerprint: None,
                    reasoning_effort: None,
                })
            }
            Role::Tool => {
                let content = msg.text_content();
                ConversationItem::ToolResult(ToolResultItem {
                    tool_call_id: msg.tool_call_id.unwrap_or_default(),
                    content: Arc::<str>::from(content),
                    images: Vec::new(),
                })
            }
        }
    }
}

/// Convert a single non-`Reasoning` [`ConversationItem`]. The wire format
/// carries `reasoning_content` on the *following* assistant message, which a
/// single item cannot see, so use [`conversation_to_chat_messages`] instead
/// when reasoning must survive.
pub fn conversation_item_to_chat_message(item: ConversationItem) -> ChatRequestMessage {
    match item {
        ConversationItem::System(s) => ChatRequestMessage::system(s.content.as_ref()),
        ConversationItem::User(u) => {
            let has_images = u
                .content
                .iter()
                .any(|p| matches!(p, ContentPart::Image { .. }));
            // Collapse to a single text block when there are no images, as
            // the pre-blocks behavior did.
            let content = if !has_images {
                let text = u
                    .content
                    .iter()
                    .filter_map(|p| match p {
                        ContentPart::Text { text } => Some(text.as_ref()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                MessageContent::Text(text)
            } else {
                let blocks: Vec<ChatContentBlock> = u
                    .content
                    .into_iter()
                    .map(|part| match part {
                        ContentPart::Text { text } => ChatContentBlock::Text {
                            text: text.as_ref().to_owned(),
                        },
                        ContentPart::Image { url } => ChatContentBlock::ImageUrl {
                            image_url: ImageUrl {
                                url: url.as_ref().to_owned(),
                            },
                        },
                    })
                    .collect();
                MessageContent::Blocks(blocks)
            };
            ChatRequestMessage {
                role: Role::User,
                content,
                name: None,
                tool_calls: Vec::new(),
                tool_call_id: None,
                model_id: None,
                reasoning_content: None,
            }
        }
        ConversationItem::Assistant(a) => {
            let tool_calls: Vec<ToolCallRequest> = a
                .tool_calls
                .into_iter()
                .map(|tc| {
                    let arguments = sanitize_tool_arguments(&tc.id, &tc.name, tc.arguments.clone());
                    ToolCallRequest::function(tc.name, arguments.as_ref().to_owned())
                        .with_id(tc.id.as_ref().to_owned())
                })
                .collect();

            ChatRequestMessage {
                role: Role::Assistant,
                content: MessageContent::Text(a.content.as_ref().to_owned()),
                name: None,
                tool_calls,
                tool_call_id: None,
                model_id: a.model_id,
                reasoning_content: None,
            }
        }
        ConversationItem::ToolResult(t) => {
            if t.images.is_empty() {
                ChatRequestMessage::tool(t.tool_call_id, t.content.as_ref().to_owned())
            } else {
                let mut blocks = vec![ChatContentBlock::Text {
                    text: t.content.as_ref().to_owned(),
                }];
                for img in t.images {
                    if let ContentPart::Image { url } = img {
                        blocks.push(ChatContentBlock::ImageUrl {
                            image_url: ImageUrl {
                                url: url.as_ref().to_owned(),
                            },
                        });
                    }
                }
                ChatRequestMessage {
                    role: Role::Tool,
                    content: MessageContent::Blocks(blocks),
                    name: None,
                    tool_calls: Vec::new(),
                    tool_call_id: Some(t.tool_call_id),
                    model_id: None,
                    reasoning_content: None,
                }
            }
        }
        // Backend tool calls have no Chat Completions equivalent.
        // Emit a synthetic assistant message so the model sees context
        // about what was searched, without breaking the message sequence.
        ConversationItem::BackendToolCall(b) => ChatRequestMessage {
            role: Role::Assistant,
            content: MessageContent::Text(b.text_summary()),
            name: None,
            tool_calls: Vec::new(),
            tool_call_id: None,
            model_id: None,
            reasoning_content: None,
        },
        // The only caller folds `Reasoning` into the following assistant.
        ConversationItem::Reasoning(_) => unreachable!(
            "conversation_to_chat_messages folds Reasoning siblings; \
                 conversation_item_to_chat_message is never called with one"
        ),
    }
}

/// The canonical conversion. Each run of `Reasoning` siblings folds into the
/// `reasoning_content` of the following `Assistant`; a `BackendToolCall` in
/// between does not break the fold, any other item clears it, and reasoning
/// with no following assistant is dropped.
pub fn conversation_to_chat_messages(items: Vec<ConversationItem>) -> Vec<ChatRequestMessage> {
    let mut out: Vec<ChatRequestMessage> = Vec::with_capacity(items.len());
    let mut pending_reasoning: Vec<String> = Vec::new();

    for item in items {
        match item {
            ConversationItem::Reasoning(r) => {
                let text = reasoning_item_text(&r);
                if !text.is_empty() {
                    pending_reasoning.push(text);
                }
            }
            ConversationItem::Assistant(_) => {
                let mut msg = conversation_item_to_chat_message(item);
                if !pending_reasoning.is_empty() {
                    msg.reasoning_content = Some(pending_reasoning.join("\n"));
                    pending_reasoning.clear();
                }
                out.push(msg);
            }
            ConversationItem::BackendToolCall(_) => {
                // Keep `pending_reasoning` so it still folds onto the
                // following assistant, as the Responses path does.
                out.push(conversation_item_to_chat_message(item));
            }
            other => {
                pending_reasoning.clear();
                out.push(conversation_item_to_chat_message(other));
            }
        }
    }

    out
}

impl From<ChatResponseMessage> for ConversationItem {
    fn from(msg: ChatResponseMessage) -> Self {
        // Reasoning is dropped: the streaming consumer synthesizes the
        // sibling item instead.
        let content = msg.content.unwrap_or_default();

        let tool_calls: Vec<ToolCall> = msg
            .tool_calls
            .into_iter()
            .map(|tc| ToolCall {
                id: Arc::<str>::from(tc.id),
                name: tc.function.name,
                arguments: Arc::<str>::from(tc.function.arguments),
            })
            .collect();

        ConversationItem::Assistant(AssistantItem {
            content: Arc::<str>::from(content),
            tool_calls,
            model_id: None,
            model_fingerprint: None,
            reasoning_effort: None,
        })
    }
}

impl From<ConversationRequest> for ChatCompletionRequest {
    fn from(req: ConversationRequest) -> Self {
        let messages: Vec<ChatRequestMessage> = conversation_to_chat_messages(req.items);

        let tools_is_empty = req.tools.is_empty();
        let tools: Option<Vec<ToolDefinition>> = if tools_is_empty {
            None
        } else {
            Some(
                req.tools
                    .into_iter()
                    .map(|t| ToolDefinition::function(t.name, t.description, t.parameters))
                    .collect(),
            )
        };

        // only set `tool_choice` when there are `tools` to avoid OpenAI client errors
        let tool_choice = req
            .tool_choice
            .filter(|_| !tools_is_empty)
            .map(|tc| match tc {
                ConversationToolChoice::Auto => ToolChoice::auto(),
                ConversationToolChoice::None => ToolChoice::none(),
                ConversationToolChoice::Required => ToolChoice::required(),
                ConversationToolChoice::Function(name) => ToolChoice::function(name),
            });

        let response_format = req
            .json_schema
            .map(|schema| rs::ResponseFormat::JsonSchema {
                json_schema: rs::ResponseFormatJsonSchema {
                    description: None,
                    name: STRUCTURED_OUTPUT_SCHEMA_NAME.to_string(),
                    schema: Some(schema),
                    strict: Some(true),
                },
            });

        ChatCompletionRequest {
            model: req.model,
            messages,
            temperature: req.temperature,
            max_tokens: req.max_output_tokens,
            top_p: req.top_p,
            frequency_penalty: None,
            presence_penalty: None,
            user: None,
            tools,
            tool_choice,
            parallel_tool_calls: None,
            search_parameters: None,
            response_format,
            reasoning_effort: req.reasoning_effort,
            x_echo_agent_conv_id: req.x_echo_agent_conv_id,
            x_echo_agent_req_id: req.x_echo_agent_req_id,
            x_echo_agent_session_id: req.x_echo_agent_session_id,
            x_echo_agent_turn_idx: req.x_echo_agent_turn_idx,
            x_echo_agent_agent_id: req.x_echo_agent_agent_id,
            x_echo_agent_deployment_id: req.x_echo_agent_deployment_id,
            x_echo_agent_user_id: req.x_echo_agent_user_id,
            trace: None,
        }
    }
}
