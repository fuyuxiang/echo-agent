//! Keeps `echo.agent/task_completed` lines short enough for a client to read, both
//! when this build sends one and when replay reaches one an earlier build
//! wrote. Bounding the output field alone does not bound the line: the
//! wrapper and the JSON encoding go on top of it.

use echo_agent_tools::types::TaskSnapshot;
use serde_json::Value;
use serde_json::value::RawValue;

use crate::extensions::notification::{SessionNotification, SessionUpdate};

/// Half the 64 KiB a Python `asyncio` stream reader allows, the limit that
/// reported this bug ("Separator is not found, and chunk exceed the limit").
pub(crate) const FRAME_MAX_BYTES: usize = 32 * 1024;

/// The method the bridge sends; the budget is derived from it.
pub(crate) const METHOD: &str = "echo.agent/task_completed";

/// The JSON-RPC wrapper, the `_` an extension method carries, and the newline.
const WRAPPER_BYTES: usize = r#"{"jsonrpc":"2.0","method":"_","params":}"#.len() + 1;

/// Measured after JSON encoding.
const FIELD_MAX_BYTES: usize = 1024;

/// The replay copy of [`compact`]'s field list. The path to the log is
/// missing on purpose: a truncated pointer is worse than less output, so it
/// is cut only as a last resort.
const COMPACTED_FIELDS: [&str; 4] = ["command", "display_command", "description", "cwd"];

fn body_budget() -> usize {
    FRAME_MAX_BYTES.saturating_sub(WRAPPER_BYTES + METHOD.len())
}

/// A message body proven to fit the frame budget. [`within`] is the only
/// constructor, so a return path that skips the measurement does not compile.
pub(crate) struct FittedFrame(Box<RawValue>);

impl FittedFrame {
    pub(crate) fn into_inner(self) -> Box<RawValue> {
        self.0
    }
}

impl std::ops::Deref for FittedFrame {
    type Target = RawValue;

    fn deref(&self) -> &RawValue {
        &self.0
    }
}

/// Builds the message within [`FRAME_MAX_BYTES`], rewriting `notification` to
/// match so the caller can persist what it sends. A message with no room for
/// output still reports the task finished and points at the log. `None`
/// means nothing fit, and sending anyway is what closes the connection.
pub(crate) fn encode(notification: &mut SessionNotification) -> Option<FittedFrame> {
    let budget = body_budget();
    let Some(snapshot) = task_snapshot(notification) else {
        return within(
            serde_json::value::to_raw_value(&*notification).ok()?,
            budget,
        );
    };

    let output = std::mem::take(&mut snapshot.output);
    if let Some(params) = fit_into(notification, &output, budget) {
        return Some(params);
    }

    // Nothing this task controls is small enough; keep the ids and the status.
    let snapshot = task_snapshot(notification)?;
    snapshot.output = String::new();
    snapshot.truncated = true;
    compact_hard(snapshot);

    let params = within(
        serde_json::value::to_raw_value(&*notification).ok()?,
        budget,
    );
    if params.is_none() {
        tracing::warn!("task_completed message is too long to send even with no output");
    }
    params
}

fn fit_into(
    notification: &mut SessionNotification,
    output: &str,
    budget: usize,
) -> Option<FittedFrame> {
    // The output is the field worth keeping, so when it does not fit, the
    // other fields a task can grow give up their room to it first.
    let mut room = room_for_output(notification, budget)?;
    if encoded_len(output) > room {
        compact(task_snapshot(notification)?);
        room = room_for_output(notification, budget)?;
    }

    let snapshot = task_snapshot(notification)?;
    let (fitted, cut) = fit_output(output, &snapshot.output_file, room);
    snapshot.truncated |= cut;
    snapshot.output = fitted;

    within(
        serde_json::value::to_raw_value(&*notification).ok()?,
        budget,
    )
}

fn room_for_output(notification: &SessionNotification, budget: usize) -> Option<usize> {
    let rest = serde_json::value::to_raw_value(notification).ok()?;
    Some(budget.saturating_sub(rest.get().len()))
}

/// As much of `output` as fits `room` once encoded, then the path to the log.
/// The flag reports what was left out; the length cannot, since the footer can
/// make the result longer than the output it replaces.
fn fit_output(output: &str, output_file: &std::path::Path, room: usize) -> (String, bool) {
    if encoded_len(output) <= room {
        return (output.to_string(), false);
    }
    let footer = format!(
        "\n\n... (output truncated; full output at {})",
        output_file.display()
    );
    let footer_room = encoded_len(&footer);
    if footer_room > room {
        return (String::new(), true);
    }
    let kept = prefix_within_encoded_len(output, room - footer_room);
    (format!("{kept}{footer}"), true)
}

/// Keep in step with [`COMPACTED_FIELDS`], the replay path's copy of this
/// list.
fn compact(snapshot: &mut TaskSnapshot) {
    snapshot.command = prefix_within_encoded_len(&snapshot.command, FIELD_MAX_BYTES).to_string();
    snapshot.display_command = snapshot
        .display_command
        .as_deref()
        .map(|command| prefix_within_encoded_len(command, FIELD_MAX_BYTES).to_string());
    snapshot.description = snapshot
        .description
        .as_deref()
        .map(|description| prefix_within_encoded_len(description, FIELD_MAX_BYTES).to_string());
    snapshot.cwd = prefix_within_encoded_len(&snapshot.cwd, FIELD_MAX_BYTES).to_string();
}

/// Cuts the path too, for a message that will not fit any other way.
fn compact_hard(snapshot: &mut TaskSnapshot) {
    compact(snapshot);
    let path = snapshot.output_file.display().to_string();
    snapshot.output_file = prefix_within_encoded_len(&path, FIELD_MAX_BYTES).into();
}

fn task_snapshot(notification: &mut SessionNotification) -> Option<&mut TaskSnapshot> {
    let SessionUpdate::TaskCompleted { task_snapshot, .. } = &mut notification.update else {
        return None;
    };
    Some(task_snapshot)
}

/// `None` when it is still over the limit, so the caller can try smaller.
fn within(params: Box<RawValue>, budget: usize) -> Option<FittedFrame> {
    (params.get().len() <= budget).then_some(FittedFrame(params))
}

/// Bytes this text costs inside a JSON string, never underestimated.
fn encoded_len(text: &str) -> usize {
    text.chars().map(encoded_char_len).sum()
}

fn prefix_within_encoded_len(text: &str, max: usize) -> &str {
    let mut used = 0;
    for (index, character) in text.char_indices() {
        used += encoded_char_len(character);
        if used > max {
            return &text[..index];
        }
    }
    text
}

fn encoded_char_len(character: char) -> usize {
    match character {
        '"' | '\\' | '\n' | '\r' | '\t' | '\u{8}' | '\u{c}' => 2,
        control if (control as u32) < 0x20 => 6,
        other => other.len_utf8(),
    }
}

/// What to do with a recorded message on its way back out. `Unfittable` has to
/// be dropped: sending it would close the connection.
pub(crate) enum Refit {
    Unchanged,
    Fitted(FittedFrame),
    Unfittable,
}

/// Shrinks a recorded task completion that is too long to send. Edited in
/// place because an earlier build may have written fields this one does not
/// model, and replay must not drop them.
pub(crate) fn refit_recorded(params: &RawValue) -> Refit {
    let budget = body_budget();
    if params.get().len() <= budget {
        return Refit::Unchanged;
    }

    let Some(mut record) = serde_json::from_str::<Value>(params.get()).ok() else {
        return Refit::Unchanged;
    };
    if !is_task_completion(&record) {
        return Refit::Unchanged;
    }
    match shrink_record(&mut record, budget) {
        Some(refit) => Refit::Fitted(refit),
        None => Refit::Unfittable,
    }
}

fn is_task_completion(record: &Value) -> bool {
    record
        .get("update")
        .and_then(|update| update.get("sessionUpdate"))
        .and_then(Value::as_str)
        == Some("task_completed")
}

fn shrink_record(record: &mut Value, budget: usize) -> Option<FittedFrame> {
    let snapshot = record.get_mut("update")?.get_mut("task_snapshot")?;
    let output = snapshot.get("output")?.as_str()?.to_owned();
    let output_file = snapshot
        .get("output_file")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();

    snapshot["output"] = Value::String(String::new());
    for field in COMPACTED_FIELDS {
        if let Some(text) = snapshot.get(field).and_then(Value::as_str) {
            let capped = prefix_within_encoded_len(text, FIELD_MAX_BYTES).to_owned();
            snapshot[field] = Value::String(capped);
        }
    }

    let room = budget.saturating_sub(serde_json::to_string(&record).ok()?.len());
    let (fitted, cut) = fit_output(&output, std::path::Path::new(&output_file), room);

    let snapshot = record.get_mut("update")?.get_mut("task_snapshot")?;
    snapshot["output"] = Value::String(fitted);
    if cut {
        snapshot["truncated"] = Value::Bool(true);
    }

    if let Some(refit) = within(serde_json::value::to_raw_value(&record).ok()?, budget) {
        return Some(refit);
    }

    // The same last resort as `encode`: no output, and the path capped too.
    let snapshot = record.get_mut("update")?.get_mut("task_snapshot")?;
    snapshot["output"] = Value::String(String::new());
    snapshot["truncated"] = Value::Bool(true);
    if let Some(path) = snapshot.get("output_file").and_then(Value::as_str) {
        let capped = prefix_within_encoded_len(path, FIELD_MAX_BYTES).to_owned();
        snapshot["output_file"] = Value::String(capped);
    }
    within(serde_json::value::to_raw_value(&record).ok()?, budget)
}

#[cfg(test)]
#[path = "task_completed_frame_tests.rs"]
mod tests;
