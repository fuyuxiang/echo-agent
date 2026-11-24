//! Bounded, tool-free requests for features that consume model text.
use futures::StreamExt;

const MAX_API_RESPONSE_BYTES: usize = 8 * 1024 * 1024;

async fn response_bytes_bounded(response: reqwest::Response) -> Result<Vec<u8>, String> {
    let mut stream = response.bytes_stream();
    let mut bytes = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| "读取模型响应失败")?;
        if bytes.len().saturating_add(chunk.len()) > MAX_API_RESPONSE_BYTES {
            return Err("模型响应超过大小上限".into());
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

pub(crate) fn visible_completion_text(value: &str) -> &str {
    let mut visible = value.trim();
    while let Some(reasoning) = visible.strip_prefix("<think>") {
        let Some((_, remainder)) = reasoning.split_once("</think>") else {
            return "";
        };
        visible = remainder.trim_start();
    }
    visible.trim()
}

/// Use a configured provider for isolated, tool-free text generation.
pub(crate) async fn complete_text(
    client: &reqwest::Client,
    access: &crate::providers::ConfiguredModelAccess,
    system: &str,
    user: &str,
) -> Result<String, String> {
    let body = match access.api_backend.as_str() {
        "responses" => serde_json::json!({
            "model": access.remote_model_id, "instructions": system, "input": user,
            "max_output_tokens": 4096, "stream": false,
        }),
        "messages" => serde_json::json!({
            "model": access.remote_model_id, "system": system, "max_tokens": 4096,
            "messages": [{ "role": "user", "content": user }], "stream": false,
        }),
        _ => serde_json::json!({
            "model": access.remote_model_id,
            "messages": [
                {"role": "system", "content": system},
                {"role": "user", "content": user}
            ],
            "stream": false,
        }),
    };
    let response = access
        .authorize(client.post(&access.endpoint).json(&body))
        .send()
        .await
        .map_err(|_| "无法连接所选模型")?;
    let status = response.status();
    if !status.is_success() {
        return Err(format!("所选模型返回 HTTP {status}"));
    }
    let bytes = response_bytes_bounded(response).await?;
    let value: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|_| "模型返回了无效响应")?;
    check_response_complete(&value, &access.api_backend)?;
    let text = access.response_text(&value).ok_or("模型没有返回文本")?;
    let visible = visible_completion_text(&text);
    if visible.is_empty() || value.get("error").is_some_and(|error| !error.is_null()) {
        return Err("模型没有返回文本".into());
    }
    if access.api_key.is_empty() {
        Ok(visible.to_owned())
    } else {
        Ok(visible.replace(&access.api_key, "[REDACTED]"))
    }
}

pub(crate) fn check_response_complete(
    value: &serde_json::Value,
    backend: &str,
) -> Result<(), String> {
    let incomplete = match backend {
        "responses" => value["status"]
            .as_str()
            .is_some_and(|status| status != "completed"),
        "messages" => matches!(
            value["stop_reason"].as_str(),
            Some(
                "max_tokens"
                    | "model_context_window_exceeded"
                    | "pause_turn"
                    | "refusal"
                    | "tool_use"
            )
        ),
        _ => matches!(
            value
                .pointer("/choices/0/finish_reason")
                .and_then(serde_json::Value::as_str),
            Some("length" | "content_filter" | "tool_calls" | "function_call")
        ),
    };
    if incomplete {
        return Err("模型未完成输出，请重试或更换模型".into());
    }
    Ok(())
}
