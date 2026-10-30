//! Platform-independent, bounded menu data. No transcripts or workspace paths cross this API.
use serde::{Deserialize, Serialize};

/// Native exit protection reads live Runtime actors, independently of menu
/// visibility, archive metadata and the renderer's availability.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ExitActivity {
    pub running: bool,
    pub waiting: bool,
}

impl ExitActivity {
    pub fn has_tasks(&self) -> bool {
        self.running || self.waiting
    }

    pub fn description(&self) -> &'static str {
        match (self.running, self.waiting) {
            (true, true) => "仍有任务正在运行或等待确认",
            (true, false) => "仍有任务正在运行",
            (false, true) => "仍有任务等待确认",
            (false, false) => "没有正在运行的任务",
        }
    }
}

#[derive(Debug, Default, Deserialize)]
pub struct RuntimeExitSnapshot {
    // Required: an invalid/unsupported response must not mean "idle".
    pub sessions: Vec<RuntimeExitSession>,
}

#[derive(Debug, Deserialize)]
pub struct RuntimeExitSession {
    pub resident: bool,
    pub activity: RuntimeActivity,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeActivity {
    Working,
    NeedsInput,
    Idle,
    Dormant,
    Completed,
    Dead,
}

impl RuntimeExitSnapshot {
    pub fn activity(&self) -> ExitActivity {
        let mut activity = ExitActivity::default();
        for session in self.sessions.iter().filter(|session| session.resident) {
            match session.activity {
                RuntimeActivity::Working => activity.running = true,
                RuntimeActivity::NeedsInput => activity.waiting = true,
                _ => {}
            }
        }
        activity
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum TaskKind {
    Session,
    Group,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum TaskState {
    Waiting,
    Running,
    Recent,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct TrayTask {
    pub kind: TaskKind,
    pub id: String,
    pub title: String,
    pub state: TaskState,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TraySnapshot {
    pub waiting_count: u32,
    pub running_count: u32,
    pub tasks: Vec<TrayTask>,
}

impl TraySnapshot {
    pub fn validate(&self) -> Result<(), String> {
        if self.tasks.len() > 3 || self.waiting_count > 20_000 || self.running_count > 20_000 {
            return Err("托盘任务摘要超出范围".into());
        }
        let mut keys = std::collections::HashSet::new();
        for task in &self.tasks {
            if task.id.trim().is_empty()
                || task.id.chars().count() > 256
                || task.id.chars().any(char::is_control)
                || task.title.chars().count() > 512
                || !keys.insert(format!("{:?}:{}", task.kind, task.id))
            {
                return Err("托盘任务摘要无效".into());
            }
        }
        Ok(())
    }

    pub fn heading(&self) -> String {
        match (self.waiting_count, self.running_count) {
            (0, 0) => "最近任务".into(),
            (waiting, 0) => format!("{waiting} 项待确认"),
            (0, running) => format!("{running} 项运行中"),
            (waiting, running) => format!("{waiting} 项待确认 · {running} 项运行中"),
        }
    }

    pub fn tooltip(&self) -> String {
        if self.waiting_count + self.running_count > 0 {
            format!("EchoAgent · {}", self.heading())
        } else {
            "EchoAgent".into()
        }
    }
}

pub fn task_label(task: &TrayTask, windows: bool) -> String {
    // Native Windows menus interpret '&' as a mnemonic. Never let a title
    // create an accelerator or line break, and keep Unicode truncation valid.
    let title: String = task
        .title
        .chars()
        .filter(|c| !matches!(*c, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}'))
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let title = title.split_whitespace().collect::<Vec<_>>().join(" ");
    let title = if title.is_empty() {
        "未命名任务".into()
    } else if title.chars().count() > 28 {
        format!("{}…", title.chars().take(28).collect::<String>())
    } else {
        title
    };
    let prefix = match task.state {
        TaskState::Waiting => "待确认 · ",
        TaskState::Running => "运行中 · ",
        TaskState::Recent => "",
    };
    let label = format!("{prefix}{title}");
    if windows {
        label.replace('&', "&&")
    } else {
        label
    }
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TrayAction {
    pub id: String,
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_id: Option<String>,
}

#[derive(Default)]
pub struct PendingAction {
    next_id: u64,
    action: Option<TrayAction>,
}
impl PendingAction {
    pub fn replace(&mut self, kind: &str, target_id: Option<String>) {
        self.next_id += 1;
        self.action = Some(TrayAction {
            id: self.next_id.to_string(),
            kind: kind.into(),
            target_id,
        });
    }
    pub fn peek(&self) -> Option<TrayAction> {
        self.action.clone()
    }
    pub fn acknowledge(&mut self, id: &str) -> bool {
        if self.action.as_ref().is_some_and(|action| action.id == id) {
            self.action = None;
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exit_activity_includes_live_archived_and_hidden_sessions_without_menu_rows() {
        let runtime: RuntimeExitSnapshot = serde_json::from_value(serde_json::json!({
            "sessions": [
                {"resident": true, "activity": "working", "archived": true},
                {"resident": true, "activity": "needs_input", "hidden": true}
            ]
        }))
        .unwrap();
        assert!(TraySnapshot::default().tasks.is_empty());
        assert_eq!(
            runtime.activity(),
            ExitActivity {
                running: true,
                waiting: true
            }
        );
        assert!(runtime.activity().has_tasks());
    }

    #[test]
    fn exit_activity_ignores_idle_and_historical_sessions() {
        let runtime: RuntimeExitSnapshot = serde_json::from_value(serde_json::json!({
            "sessions": [
                {"resident": true, "activity": "idle"},
                {"resident": true, "activity": "completed"},
                {"resident": true, "activity": "dead"},
                {"resident": false, "activity": "working"},
                {"resident": false, "activity": "needs_input"},
                {"resident": false, "activity": "dormant"}
            ]
        }))
        .unwrap();
        assert!(!runtime.activity().has_tasks());
        assert!(!RuntimeExitSnapshot::default().activity().has_tasks());
    }

    #[test]
    fn an_unreadable_runtime_response_must_not_be_treated_as_idle() {
        for payload in [
            serde_json::json!({}),
            serde_json::json!({"sessions": null}),
            serde_json::json!({"sessions": [{"resident": true, "activity": "unknown"}]}),
            serde_json::json!({"sessions": [{"activity": "working"}]}),
        ] {
            assert!(serde_json::from_value::<RuntimeExitSnapshot>(payload).is_err());
        }
    }

    fn task() -> TrayTask {
        TrayTask {
            kind: TaskKind::Session,
            id: "one".into(),
            title: "计划 A&B\n项目".into(),
            state: TaskState::Waiting,
        }
    }
    #[test]
    fn native_labels_are_bounded_and_escape_windows_mnemonics() {
        assert_eq!(task_label(&task(), true), "待确认 · 计划 A&&B 项目");
        assert_eq!(task_label(&task(), false), "待确认 · 计划 A&B 项目");
        let mut long = task();
        long.title = "中文🙂".repeat(30);
        assert!(task_label(&long, false).ends_with('…'));
        assert_eq!(task_label(&long, false).chars().count(), 35);
        long.title = "\u{202e}\t\n".into();
        assert_eq!(task_label(&long, false), "待确认 · 未命名任务");
        long.title = "项\0目".into();
        assert_eq!(task_label(&long, true), "待确认 · 项 目");
    }
    #[test]
    fn no_fake_running_state_for_idle_or_new_install() {
        assert_eq!(TraySnapshot::default().tooltip(), "EchoAgent");
        assert_eq!(TraySnapshot::default().heading(), "最近任务");
        assert_eq!(
            TraySnapshot {
                waiting_count: 2,
                running_count: 1,
                tasks: vec![]
            }
            .heading(),
            "2 项待确认 · 1 项运行中"
        );
    }
    #[test]
    fn old_navigation_acknowledgement_cannot_lose_a_newer_click() {
        let mut pending = PendingAction::default();
        pending.replace("session", Some("one".into()));
        let first = pending.peek().unwrap();
        pending.replace("new", None);
        assert!(!pending.acknowledge(&first.id));
        let latest = pending.peek().unwrap();
        assert_eq!(latest.kind, "new");
        assert_eq!(pending.peek(), Some(latest.clone()));
        assert!(pending.acknowledge(&latest.id));
        assert!(pending.peek().is_none());
    }
    #[test]
    fn rejects_duplicate_and_oversized_payloads() {
        let mut snapshot = TraySnapshot {
            tasks: vec![task()],
            ..Default::default()
        };
        assert!(snapshot.validate().is_ok());
        snapshot.tasks.push(task());
        assert!(snapshot.validate().is_err());
        snapshot.tasks = vec![task()];
        snapshot.tasks[0].id = "\n".into();
        assert!(snapshot.validate().is_err());
    }
}
