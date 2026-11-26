//! Session synthesis for replay/load tests: writes
//! `updates.jsonl`/`rewind_points.jsonl` directly, for exact control over ACU
//! redundancy and rewind points.

use std::path::{Path, PathBuf};

use agent_client_protocol::{self as acp};
use echo_agent_workspace::session::file_state::{FileSnapshot, FlexiblePath, RewindPoint};

use crate::session::info::Info;
use crate::session::storage::{JsonlStorageAdapter, StorageAdapter};

fn parse_or<T: std::str::FromStr>(key: &str, found: Option<String>, default: T) -> T {
    let Some(text) = found else {
        return default;
    };
    match text.parse() {
        Ok(value) => value,
        Err(_) => {
            eprintln!("[testkit] ignoring unparseable {key}={text:?}; using default");
            default
        }
    }
}

/// Generation parameters; fields double as the defaults for
/// [`SessionSpec::from_env_prefixed`].
pub struct SessionSpec {
    pub turns: usize,
    /// `available_commands_update`s persisted per turn: the redundant catalog
    /// a real session re-advertises on every skill/subagent boundary.
    pub acu_per_turn: usize,
    pub catalog_commands: usize,
    pub catalog_desc_len: usize,
    pub agent_chunks_per_turn: usize,
    pub agent_chunk_len: usize,
    pub rewind_points: usize,
    pub files_per_rewind: usize,
    pub file_content_len: usize,
}

impl Default for SessionSpec {
    /// Baseline yielding a ~20 MB `updates.jsonl`; callers override the knobs
    /// they scale up.
    fn default() -> Self {
        Self {
            turns: 60,
            acu_per_turn: 15,
            catalog_commands: 64,
            catalog_desc_len: 320,
            agent_chunks_per_turn: 8,
            agent_chunk_len: 2000,
            rewind_points: 20,
            files_per_rewind: 20,
            file_content_len: 4000,
        }
    }
}

impl SessionSpec {
    /// Read `<prefix>_*` env overrides on top of `defaults`, scaling `turns` and
    /// `rewind_points` by `<prefix>_SCALE`.
    pub fn from_env_prefixed(prefix: &str, defaults: Self) -> Self {
        Self::from_lookup(prefix, defaults, |key| std::env::var(key).ok())
    }

    /// [`from_env_prefixed`] with an injectable lookup, so the override and
    /// scale arithmetic is unit-testable without touching process env.
    fn from_lookup(prefix: &str, defaults: Self, get: impl Fn(&str) -> Option<String>) -> Self {
        let val = |name: &str, default| {
            let key = format!("{prefix}_{name}");
            parse_or(&key, get(&key), default)
        };
        let scale = val("SCALE", 1usize).max(1);
        Self {
            turns: val("TURNS", defaults.turns) * scale,
            acu_per_turn: val("ACU_PER_TURN", defaults.acu_per_turn),
            catalog_commands: val("CATALOG_COMMANDS", defaults.catalog_commands),
            catalog_desc_len: val("CATALOG_DESC_LEN", defaults.catalog_desc_len),
            agent_chunks_per_turn: val("AGENT_CHUNKS_PER_TURN", defaults.agent_chunks_per_turn),
            agent_chunk_len: val("AGENT_CHUNK_LEN", defaults.agent_chunk_len),
            rewind_points: val("REWIND_POINTS", defaults.rewind_points) * scale,
            files_per_rewind: val("FILES_PER_REWIND", defaults.files_per_rewind),
            file_content_len: val("FILE_CONTENT_LEN", defaults.file_content_len),
        }
    }
}

fn filler(n: usize) -> String {
    const WORDS: &[&str] = &[
        "alpha", "bravo", "charlie", "delta", "echo", "foxtrot", "golf", "hotel", "india",
        "juliet", "kilo", "lima", "mike", "november", "oscar", "papa", "quebec", "romeo",
    ];
    let mut s = String::with_capacity(n + 8);
    let mut i = 0usize;
    while s.len() < n {
        s.push_str(WORDS[i % WORDS.len()]);
        s.push(' ');
        i += 1;
    }
    s.truncate(n);
    s
}

pub fn sid(session_id: &str) -> acp::SessionId {
    acp::SessionId::new(session_id.to_string())
}

fn text_chunk(text: String) -> acp::ContentChunk {
    acp::ContentChunk::new(acp::ContentBlock::Text(acp::TextContent::new(text)))
}

/// One large `AvailableCommandsUpdate`: the redundant catalog re-persisted
/// thousands of times in the real session.
fn available_commands_update(spec: &SessionSpec) -> acp::SessionUpdate {
    let desc = filler(spec.catalog_desc_len);
    let commands: Vec<acp::AvailableCommand> = (0..spec.catalog_commands)
        .map(|i| {
            acp::AvailableCommand::new(format!("command-number-{i:03}"), desc.clone()).input(Some(
                acp::AvailableCommandInput::Unstructured(acp::UnstructuredCommandInput::new(
                    "[optional arguments here]".to_string(),
                )),
            ))
        })
        .collect();
    acp::SessionUpdate::AvailableCommandsUpdate(acp::AvailableCommandsUpdate::new(commands))
}

fn envelope_line(session_id: &str, update: acp::SessionUpdate) -> String {
    let update_val = serde_json::to_value(&update).expect("serialize update");
    let params = serde_json::json!({
        "sessionId": session_id,
        "update": update_val,
    });
    let envelope = serde_json::json!({
        "timestamp": 0u64,
        "method": "session/update",
        "params": params,
    });
    serde_json::to_string(&envelope).expect("serialize envelope")
}

fn write_updates_jsonl(path: &Path, session_id: &str, spec: &SessionSpec) {
    let mut out = String::new();
    for turn in 0..spec.turns {
        out.push_str(&envelope_line(
            session_id,
            acp::SessionUpdate::UserMessageChunk(text_chunk(format!(
                "user prompt for turn {turn}"
            ))),
        ));
        out.push('\n');
        for _ in 0..spec.acu_per_turn {
            out.push_str(&envelope_line(session_id, available_commands_update(spec)));
            out.push('\n');
        }
        for _ in 0..spec.agent_chunks_per_turn {
            out.push_str(&envelope_line(
                session_id,
                acp::SessionUpdate::AgentMessageChunk(text_chunk(filler(spec.agent_chunk_len))),
            ));
            out.push('\n');
        }
    }
    std::fs::write(path, out).expect("write updates.jsonl");
}

/// Replay keeps the per-turn user and agent chunks and drops ACUs; keep in sync
/// with `write_updates_jsonl`.
pub fn expected_replay_lines(spec: &SessionSpec) -> usize {
    spec.turns * (1 + spec.agent_chunks_per_turn)
}

pub fn write_rewind_jsonl(path: &Path, spec: &SessionSpec) {
    let mut out = String::new();
    for p in 0..spec.rewind_points {
        let mut rp = RewindPoint::new(p);
        for f in 0..spec.files_per_rewind {
            let fp =
                FlexiblePath::Absolute(PathBuf::from(format!("/repo/src/module_{p}/file_{f}.rs")));
            rp.add_snapshot(FileSnapshot::new_flexible(
                fp.clone(),
                Some(filler(spec.file_content_len)),
            ));
            rp.set_after_snapshot(FileSnapshot::new_flexible(
                fp,
                Some(filler(spec.file_content_len + 64)),
            ));
        }
        out.push_str(&serde_json::to_string(&rp).expect("serialize rewind point"));
        out.push('\n');
    }
    std::fs::write(path, out).expect("write rewind_points.jsonl");
}

/// Locate `<root>/sessions/<encoded cwd>/<id>` without the crate-internal cwd encoder.
pub fn locate_session_dir(root: &Path, id: &str) -> PathBuf {
    let sessions = root.join("sessions");
    for entry in std::fs::read_dir(&sessions).expect("read sessions dir") {
        let entry = entry.expect("read sessions dir entry");
        let candidate = entry.path().join(id);
        if candidate.is_dir() {
            return candidate;
        }
    }
    panic!(
        "could not locate session dir for {id} under {}",
        sessions.display()
    );
}

/// Synthesize a session on disk under `root` for working dir `cwd`: the summary
/// through the production storage adapter, then `updates.jsonl` and
/// `rewind_points.jsonl` written directly. Returns the `Info` and its directory.
pub async fn prepare_session(root: &Path, cwd: &Path, spec: &SessionSpec) -> (Info, PathBuf) {
    let adapter = JsonlStorageAdapter::with_root(root.to_path_buf());
    let id = uuid::Uuid::new_v4().to_string();
    let info = Info {
        id: sid(&id),
        cwd: cwd.to_string_lossy().to_string(),
    };
    adapter
        .init_session(&info, acp::ModelId::new("test-model"))
        .await
        .expect("init_session");
    let dir = locate_session_dir(root, &id);
    write_updates_jsonl(&dir.join("updates.jsonl"), &id, spec);
    write_rewind_jsonl(&dir.join("rewind_points.jsonl"), spec);
    (info, dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filler_is_exactly_n_bytes() {
        assert_eq!(filler(0).len(), 0);
        assert_eq!(filler(100).len(), 100);
        assert_eq!(filler(4096).len(), 4096);
    }

    #[test]
    fn from_lookup_falls_back_to_defaults_when_absent() {
        let d = SessionSpec::default();
        let spec = SessionSpec::from_lookup("P", SessionSpec::default(), |_| None);
        assert_eq!(spec.turns, d.turns);
        assert_eq!(spec.rewind_points, d.rewind_points);
        assert_eq!(spec.agent_chunks_per_turn, d.agent_chunks_per_turn);
    }

    #[test]
    fn from_lookup_applies_overrides_and_scale() {
        let env = std::collections::HashMap::from([
            ("P_TURNS".to_string(), "10".to_string()),
            ("P_SCALE".to_string(), "3".to_string()),
            ("P_FILES_PER_REWIND".to_string(), "not_a_number".to_string()),
        ]);
        let d = SessionSpec::default();
        let spec = SessionSpec::from_lookup("P", SessionSpec::default(), |k| env.get(k).cloned());
        assert_eq!(spec.turns, 10 * 3, "override is multiplied by SCALE");
        assert_eq!(
            spec.rewind_points,
            d.rewind_points * 3,
            "SCALE also scales rewind_points"
        );
        assert_eq!(
            spec.acu_per_turn, d.acu_per_turn,
            "an unscaled field keeps its default"
        );
        assert_eq!(
            spec.files_per_rewind, d.files_per_rewind,
            "an unparseable override falls back to the (unscaled) default"
        );
    }
}
