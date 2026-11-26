//! Todo types — re-exported from `echo-agent-tools` with ACP conversion helpers.
//!
//! Types are canonical in `echo-agent-tools`. This module adds ACP ↔ TodoItem
//! conversions since `echo-agent-tools` is protocol-agnostic.

pub use echo_agent_tools::implementations::echo_agent_build::todo::TodoId;
pub use echo_agent_tools::implementations::echo_agent_build::todo::TodoItem;
pub use echo_agent_tools::implementations::echo_agent_build::todo::TodoPriority;
pub use echo_agent_tools::implementations::echo_agent_build::todo::TodoState;
pub use echo_agent_tools::implementations::echo_agent_build::todo::TodoStatus;

use agent_client_protocol as acp;

/// Convert an ACP `PlanEntry` to a `TodoItem`.
///
/// Handles the cancelled state: ACP has no `Cancelled` status, so cancelled
/// items are stored as `Completed` with `{"cancelled": true}` in meta.
pub fn todo_item_from_plan_entry(entry: acp::PlanEntry) -> TodoItem {
    let status = match entry.status {
        acp::PlanEntryStatus::Pending => TodoStatus::Pending,
        acp::PlanEntryStatus::InProgress => TodoStatus::InProgress,
        acp::PlanEntryStatus::Completed => {
            // Check if this is actually a cancelled item
            if entry
                .meta
                .as_ref()
                .and_then(|m| m.get("cancelled"))
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
            {
                TodoStatus::Cancelled
            } else {
                TodoStatus::Completed
            }
        }
        // TODO(acp-0.10): `PlanEntryStatus` is #[non_exhaustive].
        _ => TodoStatus::Pending,
    };
    TodoItem {
        content: entry.content,
        priority: match entry.priority {
            acp::PlanEntryPriority::High => TodoPriority::High,
            acp::PlanEntryPriority::Medium => TodoPriority::Medium,
            acp::PlanEntryPriority::Low => TodoPriority::Low,
            // TODO(acp-0.10): `PlanEntryPriority` is #[non_exhaustive].
            _ => TodoPriority::Medium,
        },
        status,
        meta: entry.meta.map(serde_json::Value::Object),
    }
}

/// Convert a `TodoItem` to an ACP `PlanEntry`.
///
/// Cancelled items become `Completed` with `{"cancelled": true}` in meta.
pub(crate) fn plan_entry_from_todo_item(item: TodoItem) -> acp::PlanEntry {
    let status = match item.status {
        TodoStatus::Pending => acp::PlanEntryStatus::Pending,
        TodoStatus::InProgress => acp::PlanEntryStatus::InProgress,
        TodoStatus::Completed => acp::PlanEntryStatus::Completed,
        TodoStatus::Cancelled => acp::PlanEntryStatus::Completed,
    };
    let mut meta = item.meta;
    if item.status == TodoStatus::Cancelled {
        let mut m = meta.unwrap_or_else(|| serde_json::json!({}));
        if let Some(obj) = m.as_object_mut() {
            obj.insert("cancelled".into(), true.into());
        }
        meta = Some(m);
    }
    acp::PlanEntry::new(
        item.content,
        match item.priority {
            TodoPriority::High => acp::PlanEntryPriority::High,
            TodoPriority::Medium => acp::PlanEntryPriority::Medium,
            TodoPriority::Low => acp::PlanEntryPriority::Low,
        },
        status,
    )
    .meta(meta.and_then(|v| v.as_object().cloned()))
}
