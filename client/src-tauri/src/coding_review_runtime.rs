//! Native capture of an explicitly requested read-only review. Only actual
//! Runtime chunks for the registered session/run become review evidence.
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use serde::Deserialize;
use serde_json::Value;
use tauri::Emitter;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewRequest {
    pub task_id: String,
    pub kind: crate::coding::review::ReviewKind,
    pub content_revision: String,
}
struct PendingReview {
    root: PathBuf,
    request: ReviewRequest,
    summary: String,
    overflow: bool,
}
static REVIEWS: OnceLock<Mutex<HashMap<(String, String), PendingReview>>> = OnceLock::new();
fn reviews() -> &'static Mutex<HashMap<(String, String), PendingReview>> {
    REVIEWS.get_or_init(Default::default)
}

pub fn begin(
    root: &Path,
    session: &str,
    run: &str,
    mode: Option<&str>,
    request: ReviewRequest,
) -> Result<(), String> {
    if !matches!(mode, Some("ask" | "plan")) {
        return Err("代码审查必须使用只读模式".into());
    }
    crate::coding::store::validate_task_id(&request.task_id)?;
    let task = crate::coding::task::load(root, &request.task_id).ok_or("审查任务不存在")?;
    if task.session_id.as_deref() != Some(session) {
        return Err("审查会话与任务归属不一致".into());
    }
    let set = crate::coding::changeset::load(root, &request.task_id);
    crate::coding::changeset::ensure_changes_current(root, &set)?;
    if set.content_revision() != request.content_revision {
        return Err("审查内容已变化，请刷新差异后重试".into());
    }
    let mut pending = reviews().lock().map_err(|_| "审查状态不可用")?;
    if pending.len() >= 16 || pending.contains_key(&(session.into(), run.into())) {
        return Err("审查请求已存在或数量超限".into());
    }
    pending.insert(
        (session.into(), run.into()),
        PendingReview {
            root: root.into(),
            request,
            summary: String::new(),
            overflow: false,
        },
    );
    Ok(())
}

pub fn observe(session: &str, update: &Value, meta: Option<&Value>) {
    if update.get("sessionUpdate").and_then(Value::as_str) != Some("agent_message_chunk") {
        return;
    }
    let update_meta = update.get("_meta");
    let run = meta
        .and_then(|value| value.get("promptId"))
        .or_else(|| update_meta.and_then(|value| value.get("promptId")))
        .and_then(Value::as_str);
    let text = update.pointer("/content/text").and_then(Value::as_str);
    let (Some(run), Some(text)) = (run, text) else {
        return;
    };
    if meta
        .and_then(|value| value.get("isReplay"))
        .and_then(Value::as_bool)
        == Some(true)
    {
        return;
    }
    if let Ok(mut pending) = reviews().lock() {
        if let Some(review) = pending.get_mut(&(session.into(), run.into())) {
            if review.summary.len().saturating_add(text.len()) > 256 * 1024 {
                review.overflow = true;
            } else if !review.overflow {
                review.summary.push_str(text);
            }
        }
    }
}

pub fn discard(session: &str, run: &str) {
    if let Ok(mut pending) = reviews().lock() {
        pending.remove(&(session.into(), run.into()));
    }
}

pub fn complete(
    app: &tauri::AppHandle,
    complete: &crate::bridge::CompleteEvent,
) -> Result<(), String> {
    let pending = reviews()
        .lock()
        .map_err(|_| "审查状态不可用")?
        .remove(&(complete.session_id.clone(), complete.prompt_id.clone()));
    let Some(review) = pending else { return Ok(()) };
    if crate::bridge::completion_disposition(complete)
        != crate::bridge::CompletionDisposition::Success
    {
        return Ok(());
    }
    if review.overflow || review.summary.trim().is_empty() {
        return Err("实际模型审查结果为空或超过上限，未生成审查证据".into());
    }
    let record = crate::coding::review::record_runtime_review(
        &review.root,
        &review.request.task_id,
        review.request.kind,
        &complete.session_id,
        &complete.prompt_id,
        &review.request.content_revision,
        &review.summary,
    )?;
    let _ = app.emit("coding://review", record);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unrelated_or_replayed_chunks_do_not_create_review_evidence() {
        observe(
            "unregistered",
            &serde_json::json!({"sessionUpdate":"agent_message_chunk","content":{"text":"not evidence"},"_meta":{"promptId":"run"}}),
            None,
        );
        assert!(reviews().lock().unwrap().is_empty());
    }
}
