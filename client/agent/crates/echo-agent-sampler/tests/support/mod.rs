//! Sampler-specific helpers for the shared-HTTP-client integration binaries:
//! config + request drivers for real `SamplingClient`s. The generic
//! connection-counting server lives in `echo_agent_test_support`.

use std::sync::Arc;

use echo_agent_sampler::{SamplerConfig, SamplingClient};
use echo_agent_sampling_types::{ContentPart, ConversationItem, ConversationRequest, UserItem};

pub fn test_config(base_url: &str, api_key: &str) -> SamplerConfig {
    SamplerConfig {
        api_key: Some(api_key.to_string()),
        base_url: base_url.to_string(),
        model: "test-model".to_string(),
        ..SamplerConfig::default()
    }
}

/// Drive one POST through the client; the canned `{}` body is not a valid
/// completion, but only the wire-level request matters here.
pub async fn send_one(client: &SamplingClient) {
    let request = ConversationRequest {
        items: vec![ConversationItem::User(UserItem {
            content: vec![ContentPart::Text {
                text: Arc::<str>::from("hi"),
            }],
            ..Default::default()
        })],
        ..Default::default()
    };
    let _ = client.conversation(request).await;
}
