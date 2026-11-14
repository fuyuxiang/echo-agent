//! Trusted user intent and execution lifecycle. Never called from MCP tool text.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UserIntent {
    DesktopTask,
    Continue,
    /// A direct instruction/answer about an existing task, never a mode switch.
    TaskUpdate,
    Ordinary,
}

fn request_body(text: &str) -> String {
    let mut text = text.trim().to_lowercase();
    // Strip request syntax only at the beginning, never words inside content.
    loop {
        let previous = text.clone();
        for prefix in [
            "好了，",
            "好的，",
            "可以了，",
            "现在",
            "麻烦",
            "请",
            "帮我",
            "帮忙",
            "please ",
            "could you ",
            "can you ",
        ] {
            if let Some(tail) = text.strip_prefix(prefix) {
                text = tail.trim_start().to_owned();
                break;
            }
        }
        if text == previous {
            break;
        }
    }
    text.trim_end_matches(['。', '！', '!', '.', '吧'])
        .trim()
        .to_owned()
}

fn non_execution(text: &str) -> bool {
    [
        "不要",
        "别",
        "先不要",
        "暂时不要",
        "不用",
        "不需要",
        "请勿",
        "do not ",
        "don't ",
        "解释",
        "分析",
        "检查",
        "排查",
        "修复",
        "修改代码",
        "阅读",
        "总结",
        "讨论",
        "告诉我",
        "如何",
        "怎么",
        "是否",
        "为什么",
        "explain",
        "analy",
        "review",
        "debug",
        "fix ",
        "implement ",
        "write code",
        "how ",
        "why ",
    ]
    .iter()
    .any(|prefix| text.starts_with(prefix))
}

pub fn suspends_execution(text: &str) -> bool {
    non_execution(&request_body(text))
}

/// A positive, narrow missing-input check. Never invent outbound message text.
pub fn message_text_required(text: &str) -> bool {
    let body = request_body(text);
    if non_execution(&body) {
        return false;
    }
    [
        "发条消息",
        "发一条消息",
        "发个消息",
        "发消息",
        "发送消息",
        "发条信息",
        "发一条信息",
        "发送一条消息",
    ]
    .iter()
    .any(|ending| body.ends_with(ending))
}

#[derive(Debug)]
pub struct MessageAnswer {
    pub text: String,
    pub instructions: String,
}

pub fn message_answer(text: &str) -> Option<MessageAnswer> {
    let original = text.trim();
    if original.is_empty() || original.len() > 64000 {
        return None;
    }
    let body = request_body(original);
    let mut payload = original;
    let mut declared = false;
    for prefix in [
        "内容必须精确为",
        "内容就是",
        "内容是",
        "内容为",
        "正文是",
        "就发",
        "发送内容为",
    ] {
        if let Some(tail) = original.strip_prefix(prefix) {
            payload = tail.trim_start_matches(['：', ':', ' ', '\n']);
            declared = true;
            break;
        }
    }
    if payload.trim().is_empty() {
        return None;
    }
    if payload.starts_with('"') {
        let mut stream = serde_json::Deserializer::from_str(payload).into_iter::<String>();
        let message = stream.next()?.ok()?;
        let consumed = stream.byte_offset();
        return (!message.is_empty()).then(|| MessageAnswer {
            text: message,
            instructions: payload[consumed..]
                .trim_start_matches(['。', '，', ';', '；', ' ', '\n'])
                .into(),
        });
    }
    if let Some(tail) = payload.strip_prefix('“') {
        let end = tail.find('”')?;
        let message = &tail[..end];
        return (!message.is_empty()).then(|| MessageAnswer {
            text: message.into(),
            instructions: tail[end + '”'.len_utf8()..]
                .trim_start_matches(['。', '，', ';', '；', ' ', '\n'])
                .into(),
        });
    }
    if !declared
        && ([
            "继续",
            "接着",
            "恢复",
            "resume",
            "continue",
            "暂停",
            "停止",
            "取消",
            "等一下",
            "先等",
            "不发",
            "别发",
            "不要发",
            "不用发",
            "先不要",
            "先别",
        ]
        .iter()
        .any(|p| body.starts_with(p))
            || (non_execution(&body) && !body.starts_with("不要") && !body.starts_with("别")))
    {
        return None;
    }
    if ["，不要发", "，先别发", "。不要发", "；不要发", "，先不要发"]
        .iter()
        .any(|p| payload.contains(p))
    {
        return None;
    }
    Some(MessageAnswer {
        text: payload.into(),
        instructions: String::new(),
    })
}
pub fn is_missing_input_answer(text: &str) -> bool {
    message_answer(text).is_some()
}

fn occurrences(text: &str, word: &str) -> Option<usize> {
    text.match_indices(word).find_map(|(start, _)| {
        let end = start + word.len();
        let word_boundary =
            |ch: Option<char>| ch.is_none_or(|c| !c.is_ascii_alphanumeric() && c != '_');
        (!word.is_ascii()
            || (word_boundary(text[..start].chars().next_back())
                && word_boundary(text[end..].chars().next())))
        .then_some(start)
    })
}
fn first_action(text: &str) -> Option<usize> {
    [
        "输入",
        "填写",
        "保存",
        "点击",
        "打开",
        "启动",
        "新建",
        "键入",
        "编辑",
        "切换",
        "执行",
        "操作",
        "改为",
        "改成",
        "写入",
        "补充",
        "删除",
        "命名",
        "查看",
        "读取",
        "查找",
        "核对",
        "检查",
        "发送",
        "提交",
        "发布",
        "上传",
        "支付",
        "付款",
        "购买",
        "覆盖",
        "替换",
        "发条消息",
        "发一条消息",
        "发个消息",
        "发消息",
        "发给",
        "type",
        "save",
        "click",
        "open",
        "create",
        "edit",
        "launch",
        "send",
        "submit",
        "publish",
        "upload",
        "pay",
        "purchase",
        "buy",
        "delete",
        "remove",
        "overwrite",
        "replace",
    ]
    .iter()
    .filter_map(|action| occurrences(text, action))
    .min()
}
fn desktop_action(text: &str) -> bool {
    first_action(text).is_some()
}

fn known_application(text: &str) -> bool {
    [
        "textedit",
        "finder",
        "word",
        "excel",
        "powerpoint",
        "wps",
        "chrome",
        "safari",
        "outlook",
        "浏览器",
        "微信",
        "飞书",
        "notepad",
        "记事本",
        "文本编辑",
    ]
    .iter()
    .any(|name| occurrences(text, name).is_some())
}

fn application_phrase(text: &str) -> Option<&str> {
    let tail = text.strip_prefix("在")?;
    let end = tail.find(['中', '里'])?;
    let name = tail[..end].trim();
    (!name.is_empty()
        && name.len() <= 128
        && !["代码", "脚本", "源码", "React", "代码仓库"]
            .iter()
            .any(|s| name.contains(s)))
    .then_some(name)
}
/// An unrecognized launch name needs an exact host catalog match. Short file
/// names such as README are not evidence of a desktop request.
pub fn catalog_intent(
    intent: UserIntent,
    text: &str,
    browser_selected: bool,
    installed: impl Fn(&str) -> bool,
) -> UserIntent {
    if intent != UserIntent::Ordinary {
        return intent;
    }
    let body = request_body(text);
    let head = body
        .split(['\n', '，', ',', '。', '；', ';', '！', '!', '？', '?'])
        .next()
        .unwrap_or("");
    let name = ["打开", "启动", "open ", "launch "]
        .iter()
        .find_map(|prefix| head.strip_prefix(prefix))
        .map(str::trim);
    if name.is_some_and(|name| !name.is_empty() && name.len() <= 512 && installed(name)) {
        if browser_selected {
            UserIntent::TaskUpdate
        } else {
            UserIntent::DesktopTask
        }
    } else {
        intent
    }
}

pub fn selected_target_request(text: &str) -> bool {
    let body = request_body(text);
    desktop_action(&body)
        && ![
            "不要",
            "别",
            "do not ",
            "don't ",
            "解释",
            "分析",
            "讨论",
            "告诉我",
            "如何",
            "怎么",
        ]
        .iter()
        .any(|p| body.starts_with(p))
        && !["代码", "源码", "脚本", " code", " script"]
            .iter()
            .any(|word| body.contains(word))
}

pub fn selected_target_intent(intent: UserIntent, text: &str, has_scope: bool) -> UserIntent {
    if matches!(intent, UserIntent::Ordinary | UserIntent::TaskUpdate)
        && (intent == UserIntent::TaskUpdate || selected_target_request(text))
    {
        if has_scope {
            UserIntent::TaskUpdate
        } else {
            UserIntent::DesktopTask
        }
    } else {
        intent
    }
}

/// Contextual imperatives are ordinary follow-ups, not a special resume command.
/// Discussions, questions and quoted material never revive a stopped task.
pub fn contextual_intent(intent: UserIntent, text: &str, has_scope: bool) -> UserIntent {
    if !has_scope || !matches!(intent, UserIntent::Ordinary | UserIntent::DesktopTask) {
        return intent;
    }
    let body = request_body(text);
    if body.contains(['？', '?'])
        || non_execution(&body)
        || body.starts_with(['"', '\'', '“', '‘', '`', '>'])
    {
        return intent;
    }
    if intent == UserIntent::DesktopTask {
        // An application qualifier does not turn "continue saving in X" into
        // a new task that loses the original output path and restrictions.
        let directive = body.split(['"', '\'', '“', '‘', '`']).next().unwrap_or("");
        return if ["继续", "接着", "刚才", "原任务", "continue", "resume"]
            .iter()
            .any(|word| occurrences(directive, word).is_some())
        {
            UserIntent::TaskUpdate
        } else {
            intent
        };
    }
    let imperative = [
        "标题",
        "正文",
        "格式",
        "这个",
        "这里",
        "刚才",
        "接下来",
        "按",
        "照",
        "然后",
        "再",
        "完成",
        "写入",
        "保存",
        "编辑",
        "补充",
        "删除",
        "发送",
        "发给",
        "提交",
        "发布",
        "上传",
        "支付",
        "付款",
        "购买",
        "覆盖",
        "替换",
        "先问",
        "先确认",
        "先征求",
        "先征得",
        "send",
        "submit",
        "publish",
        "pay",
        "purchase",
        "buy",
        "delete",
        "remove",
        "overwrite",
        "replace",
        "ask me",
        "ask first",
    ]
    .iter()
    .any(|prefix| body.starts_with(prefix));
    if imperative && desktop_action(&body) {
        UserIntent::TaskUpdate
    } else {
        intent
    }
}

fn explicit_desktop_directive(head: &str) -> bool {
    [
        "操作电脑",
        "通过本任务操作电脑",
        "使用本任务操作电脑",
        "用本任务操作电脑",
        "操作桌面",
        "用真实桌面",
        "使用真实桌面",
        "通过真实桌面",
        "use the desktop",
        "on my computer",
        "on the desktop",
    ]
    .iter()
    .any(|prefix| head.starts_with(prefix))
}

/// A browser task can mention Chrome or request opening a web page without
/// granting a native desktop task. Keep the user's explicit mode selection;
/// only a direct desktop directive may switch it to native Computer Use.
pub fn user_intent_for_selected_mode(
    text: &str,
    has_attachments: bool,
    browser_selected: bool,
) -> UserIntent {
    let intent = user_intent(text, has_attachments);
    let body = request_body(text);
    let head = body
        .split(['\n', '，', ',', '。', '；', ';', '！', '!', '？', '?'])
        .next()
        .unwrap_or("")
        .trim();
    if browser_selected && intent == UserIntent::DesktopTask && !explicit_desktop_directive(head) {
        UserIntent::TaskUpdate
    } else {
        intent
    }
}

/// Conservative routing of a user's instruction, not keyword matching over its
/// quoted body. Unknown/general requests stay with the regular agent. The same
/// parser accepts contextual updates only after the caller binds an existing task.
pub fn user_intent(text: &str, has_attachments: bool) -> UserIntent {
    let body = request_body(text);
    let head = body
        .split(['\n', '，', ',', '。', '；', ';', '！', '!', '？', '?'])
        .next()
        .unwrap_or("")
        .trim();
    if head.is_empty()
        || non_execution(head)
        || (head.starts_with(['\"', '\'', '“', '‘', '`', '>'])
            && literal_path(text).is_none()
            && !output_name_answer(text))
    {
        return UserIntent::Ordinary;
    }
    let continuation = ["继续", "接着", "恢复", "resume", "continue"]
        .iter()
        .find_map(|prefix| head.strip_prefix(prefix));
    if body == "/resume"
        || continuation.is_some_and(|tail| {
            let tail = tail.trim();
            !body.contains(['\n', '？', '?'])
                && body.len() <= 1024
                && !non_execution(tail)
                && ![
                    "什么意思",
                    "是什么意思",
                    "如何",
                    "怎么",
                    "含义",
                    "what does",
                    "what is",
                ]
                .iter()
                .any(|s| body.contains(s))
                && (tail.is_empty()
                    || desktop_action(tail)
                    || [
                        "任务", "做", "刚才", "完成", "处理", "执行", "the task", "working",
                        "saving",
                    ]
                    .iter()
                    .any(|s| tail.contains(s)))
        })
    {
        return if has_attachments {
            UserIntent::Ordinary
        } else {
            UserIntent::Continue
        };
    }
    let directive_end = first_action(head)
        .and_then(|action| {
            head.char_indices()
                .find(|(i, c)| *i > action && matches!(c, '\"' | '“' | '\'' | '`'))
                .map(|(i, _)| i)
        })
        .unwrap_or(head.len());
    if [
        "是什么意思",
        "是什么",
        "会发生什么",
        "会怎样",
        "what happens",
        "what does",
    ]
    .iter()
    .any(|s| head[..directive_end].contains(s))
    {
        return UserIntent::Ordinary;
    }
    if head
        .find("检查")
        .is_some_and(|i| first_action(head) == Some(i))
        && ["代码", "源码", "脚本"].iter().any(|s| head.contains(s))
    {
        return UserIntent::Ordinary;
    }
    let explicit_desktop = explicit_desktop_directive(head);
    let in_application = ["在", "使用", "用", "通过", "in ", "using ", "use "]
        .iter()
        .any(|prefix| head.starts_with(prefix))
        && !["代码", "脚本", "源码", " code", " script"]
            .iter()
            .any(|s| head[..first_action(head).unwrap_or(head.len())].contains(s))
        && (known_application(head)
            || head.contains("应用中")
            || head.contains("窗口中")
            || head.contains("电脑上")
            || head.contains("桌面上")
            || ((head.contains("中") || head.contains("里")) && head.contains("应用"))
            || application_phrase(head).is_some()
            || head.strip_prefix("在").is_some_and(|tail| {
                tail.trim_start()
                    .chars()
                    .next()
                    .is_some_and(|c| c.is_ascii_alphabetic())
                    && tail.contains(['中', '里'])
            }))
        && desktop_action(head)
        && ![
            "不要",
            "先别",
            "先解释",
            "解释",
            "分析",
            "检查",
            "阅读",
            "总结",
            "讨论",
            "排查",
            "修复",
            "实现",
            "开发",
            "如何",
            "怎么",
            "review",
            "debug",
            "explain",
            "check",
        ]
        .iter()
        .any(|word| {
            head.find(word)
                .is_some_and(|negative| first_action(head).is_some_and(|action| negative < action))
        });
    let launch = ["打开", "启动", "open ", "launch "]
        .iter()
        .any(|prefix| head.starts_with(prefix))
        && (known_application(head)
            || head.contains("应用")
            || head.contains(" app")
            || head.contains(".exe")
            || head.contains(".app"));
    if explicit_desktop || in_application || launch {
        return UserIntent::DesktopTask;
    }
    let followup = [
        "把",
        "将",
        "保存到",
        "存到",
        "改存",
        "路径改",
        "文件名改",
        "输出到",
        "新建",
        "填写",
        "点击",
        "输入",
        "save it",
        "save to",
        "change the path",
        "create a new document",
    ]
    .iter()
    .any(|prefix| head.starts_with(prefix))
        && desktop_action(&body);
    if followup || output_location_answer(text) || output_name_answer(text) {
        UserIntent::TaskUpdate
    } else {
        UserIntent::Ordinary
    }
}

/// A standalone path answer. Its spelling stays intact for later exact path
/// authorization, including spaces inside quotes and Windows drive letters.
pub fn literal_path(text: &str) -> Option<&str> {
    let path = text
        .trim()
        .trim_matches(['`', '\"', '\'', '“', '”', '‘', '’']);
    let bytes = path.as_bytes();
    let absolute = path.starts_with('/')
        || path.starts_with("~/")
        || path.starts_with("~\\")
        || (bytes.len() > 3
            && bytes[0].is_ascii_alphabetic()
            && bytes[1] == b':'
            && matches!(bytes[2], b'/' | b'\\'));
    (absolute && !path.chars().any(char::is_control) && path.len() <= 4096).then_some(path)
}

fn output_location_answer(text: &str) -> bool {
    let text = text.trim();
    let location = [
        "路径是",
        "路径为",
        "目录是",
        "目录为",
        "保存位置是",
        "保存位置为",
    ]
    .iter()
    .find_map(|prefix| text.strip_prefix(prefix))
    .unwrap_or(text)
    .trim();
    literal_path(location).is_some()
        || [
            "桌面",
            "文稿",
            "文档",
            "下载",
            "工作区",
            "desktop",
            "documents",
            "downloads",
            "workspace",
        ]
        .contains(&location.to_lowercase().as_str())
}

fn output_name_answer(text: &str) -> bool {
    output_name(text).is_some()
}

pub fn output_name(text: &str) -> Option<&str> {
    let raw = text.trim().trim_end_matches(['。', '！', '!', '；', ';']);
    let quoted = raw.starts_with(['`', '\"', '\'', '“', '‘']);
    let name = raw.trim_matches(['`', '\"', '\'', '“', '”', '‘', '’']);
    let explicit = ["文件名为", "文件名是", "命名为", "filename is "]
        .iter()
        .find_map(|prefix| name.strip_prefix(prefix));
    let name = explicit.unwrap_or(name).trim();
    let (stem, extension) = name.rsplit_once('.')?;
    (!stem.is_empty()
        && name.len() <= 255
        && !name.contains(['/', '\\', '?', '？', '：', ':', '，', ',', '。'])
        && (quoted || explicit.is_some() || !name.chars().any(char::is_whitespace))
        && !name.chars().any(char::is_control)
        && (1..=10).contains(&extension.len())
        && extension.bytes().all(|c| c.is_ascii_alphanumeric()))
    .then_some(name)
}

/// Preserve the original task and explicitly supplied user amendments. This
/// text is persisted as context only; it is never an executable grant.
pub fn updated_scope(
    scope: Option<&str>,
    intent: UserIntent,
    text: &str,
) -> Result<Option<String>, String> {
    let text = text.trim();
    let next = match intent {
        UserIntent::DesktopTask => Some(text.to_owned()),
        UserIntent::Continue | UserIntent::TaskUpdate => {
            let Some(scope) = scope else { return Ok(None) };
            let bare_continue = [
                "继续",
                "继续任务",
                "继续保存",
                "继续完成保存",
                "接着保存",
                "接着做",
                "继续执行",
                "继续处理",
                "继续刚才的任务",
                "继续完成任务",
                "接着完成保存",
                "恢复任务",
                "continue",
                "resume",
                "/resume",
            ]
            .contains(&request_body(text).as_str());
            if bare_continue || scope.lines().any(|line| line == text) {
                Some(scope.to_owned())
            } else {
                Some(format!("{scope}\n\n[用户补充]\n{text}"))
            }
        }
        UserIntent::Ordinary => None,
    };
    if next.as_ref().is_some_and(|scope| scope.len() > 16384) {
        return Err(
            "任务说明与补充内容合计超过 16 KiB，请精简说明后重试；任务执行状态未改变".into(),
        );
    }
    Ok(next)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Running,
    WaitingUser,
    Verifying,
    Paused,
    Stopped,
    Completed,
    Failed,
    ResultUnknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskExecution {
    pub phase: Phase,
    pub reason: Option<String>,
    pub scope: Option<String>,
    pub authorized: bool,
    pub generation: u64,
    pub task_id: String,
    pub run_id: String,
    #[serde(default)]
    pub prompt_id: Option<String>,
    pub current_tool: Option<String>,
    #[serde(default)]
    pub current_write: bool,
    pub targets: Vec<String>,
    pub receipts: Vec<ActionReceipt>,
    pub outcome: Option<String>,
    #[serde(default)]
    pub message_text_required: bool,
    #[serde(default)]
    pub message_text: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActionReceipt {
    pub id: String,
    pub tool: String,
    pub status: String,
    pub verified: bool,
    pub write: bool,
    pub evidence: bool,
    pub at_ms: u64,
}

pub struct ApprovalState {
    generation: u64,
    phase: Phase,
}
impl TaskExecution {
    pub fn begin_approval(&mut self) -> Option<ApprovalState> {
        if !self.authorized
            || !matches!(
                self.phase,
                Phase::Running | Phase::Verifying | Phase::ResultUnknown
            )
        {
            return None;
        }
        let state = ApprovalState {
            generation: self.generation,
            phase: self.phase,
        };
        self.phase = Phase::WaitingUser;
        Some(state)
    }

    pub fn finish_approval(&mut self, state: ApprovalState) {
        if self.generation == state.generation && self.phase == Phase::WaitingUser {
            self.phase = state.phase;
        }
    }
    pub fn new(restored: bool, scope: Option<String>) -> Self {
        let pending_message = scope.as_deref().is_some_and(message_text_required);
        Self {
            phase: if restored {
                Phase::Paused
            } else {
                Phase::Running
            },
            reason: restored.then(|| "application_restarted".into()),
            scope,
            authorized: false,
            generation: 0,
            task_id: uuid::Uuid::now_v7().to_string(),
            run_id: uuid::Uuid::now_v7().to_string(),
            prompt_id: None,
            current_tool: None,
            current_write: false,
            targets: vec![],
            receipts: vec![],
            outcome: None,
            message_text_required: pending_message,
            message_text: None,
        }
    }
    pub fn paused(&self) -> bool {
        !matches!(
            self.phase,
            Phase::Running | Phase::Verifying | Phase::WaitingUser | Phase::ResultUnknown
        )
    }
    pub fn revoke(&mut self) {
        if self.current_write {
            if let Some(tool) = self.current_tool.take() {
                self.record(
                    &tool,
                    &serde_json::json!({"status":"result_unknown","verified":false}),
                    true,
                );
            }
        }
        self.current_tool = None;
        self.current_write = false;
        self.authorized = false;
        self.generation += 1;
    }
    pub fn pause(&mut self, reason: &str) {
        self.revoke();
        self.phase = Phase::Paused;
        self.reason = Some(reason.into());
    }
    pub fn stop(&mut self) {
        self.pause("user_stopped");
        self.phase = Phase::Stopped;
    }
    pub fn wait_for_user(&mut self, generation: u64, reason: &str, message: &str) -> bool {
        if self.generation != generation || self.paused() {
            return false;
        }
        self.revoke();
        self.phase = Phase::WaitingUser;
        self.reason = Some(reason.into());
        self.outcome = Some(message.into());
        true
    }
    pub fn resume(&mut self, authorize: bool) {
        self.revoke();
        self.phase = if self.message_text_required {
            Phase::WaitingUser
        } else {
            Phase::Running
        };
        self.reason = self
            .message_text_required
            .then(|| "message_text_required".into());
        self.authorized = authorize && !self.message_text_required;
        self.run_id = uuid::Uuid::now_v7().to_string();
        self.current_tool = None;
        self.outcome = self
            .message_text_required
            .then(|| "请告诉我需要发送的消息内容。".into());
        // Human takeover can change the outcome without a tool write. Earlier
        // readbacks remain history, but cannot complete this resumed run.
        for receipt in &mut self.receipts {
            receipt.evidence = false;
        }
    }

    pub fn begin(&mut self, tool: &str, write: bool) {
        self.current_tool = Some(tool.to_owned());
        self.current_write = write;
        self.phase = if tool.contains("verify")
            || tool.contains("wait_for")
            || tool == "automation_complete"
        {
            Phase::Verifying
        } else {
            Phase::Running
        };
    }

    pub fn record(&mut self, tool: &str, value: &serde_json::Value, write: bool) -> String {
        let status = value["status"].as_str().unwrap_or(if write {
            "dispatched_unverified"
        } else {
            "observed"
        });
        let verified = value["verified"] == true;
        // Evidence is produced by host verification tools, never by arbitrary model text.
        let evidence = verified
            && ((tool == "computer_verify_file" && value["status"] == "verified_exact_text")
                || tool == "computer_verify_document"
                || (tool == "computer_verify_visual" && value["status"] == "verified_visual_text")
                || tool == "browser_wait_for"
                || tool == "computer_wait_for");
        let id = uuid::Uuid::now_v7().to_string();
        self.receipts.push(ActionReceipt {
            id: id.clone(),
            tool: tool.into(),
            status: status.into(),
            verified,
            write,
            evidence,
            at_ms: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64,
        });
        if self.receipts.len() > 64 {
            self.receipts.remove(0);
        }
        self.current_tool = None;
        self.current_write = false;
        if !self.paused() && self.phase != Phase::WaitingUser {
            self.phase = if status == "result_unknown" {
                Phase::ResultUnknown
            } else {
                Phase::Running
            };
        }
        id
    }

    pub fn complete(&mut self, evidence_ids: &[String], summary: &str) -> Result<(), String> {
        if self.paused() || !self.authorized {
            return Err("task_inactive: resume before completing".into());
        }
        if summary.trim().is_empty() || summary.len() > 2000 || evidence_ids.is_empty() {
            return Err(
                "evidence_required: supply host verification receipt IDs and a bounded summary"
                    .into(),
            );
        }
        let last_write = self.receipts.iter().rposition(|r| r.write);
        if !evidence_ids.iter().all(|id| {
            self.receipts
                .iter()
                .enumerate()
                .any(|(i, r)| &r.id == id && r.evidence && last_write.is_none_or(|w| i > w))
        }) {
            let valid: Vec<_> = self
                .receipts
                .iter()
                .enumerate()
                .filter(|(i, r)| r.evidence && last_write.is_none_or(|w| *i > w))
                .map(|(_, r)| r.id.as_str())
                .collect();
            return Err(format!("evidence_required: verification must belong to this task and follow its latest write; valid fresh evidenceIds: {}", serde_json::to_string(&valid).unwrap_or_default()));
        }
        self.revoke();
        self.phase = Phase::Completed;
        self.current_tool = None;
        self.outcome = Some(summary.into());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn approval_preserves_lifecycle_and_cannot_revive_a_stopped_or_replaced_run() {
        for phase in [
            Phase::Paused,
            Phase::Stopped,
            Phase::Completed,
            Phase::Failed,
            Phase::WaitingUser,
        ] {
            let mut task = TaskExecution::new(false, Some("任务".into()));
            task.phase = phase;
            task.outcome = Some("保留成果".into());
            assert!(task.begin_approval().is_none());
            assert_eq!(task.phase, phase);
            assert_eq!(task.outcome.as_deref(), Some("保留成果"));
        }
        for phase in [Phase::Running, Phase::Verifying, Phase::ResultUnknown] {
            let mut task = TaskExecution::new(false, Some("任务".into()));
            task.resume(true);
            task.phase = phase;
            let state = task.begin_approval().unwrap();
            assert_eq!(task.phase, Phase::WaitingUser);
            task.finish_approval(state);
            assert_eq!(task.phase, phase);
            let state = task.begin_approval().unwrap();
            task.stop();
            task.finish_approval(state);
            assert_eq!(task.phase, Phase::Stopped);
            task.resume(true);
            let state = task.begin_approval().unwrap();
            task.resume(true);
            task.finish_approval(state);
            assert_eq!(task.phase, Phase::Running);
        }
    }
    #[test]
    fn consequential_followups_preserve_scope_and_confirmation_requirements() {
        let original = "在 TextEdit 中保存报告到 /tmp/report.txt，不要覆盖";
        for text in [
            "发送之前先问我",
            "先问我再发送",
            "付款前先确认",
            "覆盖之前先征求同意",
            "ask me before you send",
            "在 TextEdit 中继续保存",
        ] {
            let intent = contextual_intent(user_intent(text, false), text, true);
            assert_eq!(intent, UserIntent::TaskUpdate, "{text}");
            let scope = updated_scope(Some(original), intent, text)
                .unwrap()
                .unwrap();
            assert!(
                scope.contains("/tmp/report.txt")
                    && scope.contains("不要覆盖")
                    && scope.contains(text)
            );
        }
        assert_eq!(
            contextual_intent(
                user_intent("在微信发送“继续”给张三", false),
                "在微信发送“继续”给张三",
                true
            ),
            UserIntent::DesktopTask
        );
        assert_eq!(
            contextual_intent(
                user_intent("先分析如何删除文件", false),
                "先分析如何删除文件",
                true
            ),
            UserIntent::Ordinary
        );
    }
    #[test]
    fn visual_proof_requires_the_host_tool_positive_status_and_freshness() {
        let mut task = TaskExecution::new(false, Some("在微信给文件传输助手发送测试消息".into()));
        task.resume(true);
        let fake = task.record(
            "computer_screenshot",
            &serde_json::json!({"verified":true,"status":"verified_visual_text"}),
            false,
        );
        assert!(task.complete(&[fake], "sent").is_err());
        let mismatch = task.record(
            "computer_verify_visual",
            &serde_json::json!({"verified":false,"status":"visual_mismatch"}),
            false,
        );
        assert!(task.complete(&[mismatch], "sent").is_err());
        let old = task.record(
            "computer_verify_visual",
            &serde_json::json!({"verified":true,"status":"verified_visual_text"}),
            false,
        );
        task.record(
            "computer_key",
            &serde_json::json!({"keyDispatched":true}),
            true,
        );
        assert!(task.complete(&[old], "sent").is_err());
        let proof = task.record(
            "computer_verify_visual",
            &serde_json::json!({"verified":true,"status":"verified_visual_text"}),
            false,
        );
        task.complete(&[proof], "sent and visible in conversation")
            .unwrap();
    }
    #[test]
    fn missing_message_survives_stop_restore_and_bare_continue() {
        let mut task = TaskExecution::new(true, Some("打开微信给测试联系人发条消息".into()));
        assert!(task.message_text_required);
        for stop in [false, true] {
            if stop {
                task.stop();
            } else {
                task.pause("user_instruction");
            }
            assert!(message_answer("继续").is_none());
            task.resume(true);
            assert_eq!(task.phase, Phase::WaitingUser);
            assert!(!task.authorized);
            assert_eq!(task.reason.as_deref(), Some("message_text_required"));
        }
        task.message_text = Some(message_answer("内容是：\"你明天来吗？\"").unwrap().text);
        task.message_text_required = false;
        task.resume(true);
        assert!(task.authorized);
        assert_eq!(task.phase, Phase::Running);
    }
    #[test]
    fn outgoing_content_is_separate_from_execution_instructions() {
        for literal in ["不要发送", "删除所有文件", "为什么？"] {
            let answer = message_answer(&format!(
                "内容是：{}。只填写，不发送",
                serde_json::to_string(literal).unwrap()
            ))
            .unwrap();
            assert_eq!(answer.text, literal);
            assert_eq!(answer.instructions, "只填写，不发送");
            assert!(!answer.instructions.contains(literal));
        }
        let answer = message_answer("“明天下午三点见”").unwrap();
        assert_eq!(answer.text, "明天下午三点见");
        assert!(answer.instructions.is_empty());
        for control in [
            "内容是：",
            "正文是：   ",
            "内容是：\"\"",
            "继续",
            "先不要发送，解释一下",
            "停止",
            "你好，先不要发送",
        ] {
            assert!(message_answer(control).is_none(), "{control}");
        }
    }
    #[test]
    fn trusted_discussion_or_prohibition_revokes_an_idle_desktop_grant() {
        assert!(suspends_execution("不要保存，先解释"));
        assert!(suspends_execution("请检查这段代码"));
        assert!(!suspends_execution("保存到桌面"));
    }
    #[test]
    fn selected_target_keeps_updates_and_continue_in_the_existing_task() {
        assert_eq!(
            selected_target_intent(UserIntent::TaskUpdate, "保存到桌面", true),
            UserIntent::TaskUpdate
        );
        assert_eq!(
            selected_target_intent(UserIntent::TaskUpdate, "新建文稿", false),
            UserIntent::DesktopTask
        );
        assert_eq!(
            selected_target_intent(UserIntent::Continue, "继续保存", true),
            UserIntent::Continue
        );
        assert_eq!(
            selected_target_intent(UserIntent::Ordinary, "检查这段代码", false),
            UserIntent::Ordinary
        );
    }
    #[test]
    fn takeover_requires_new_evidence_even_if_no_further_tool_writes() {
        let mut task = TaskExecution::new(false, Some("保存文件".into()));
        task.resume(true);
        let id = task.record(
            "computer_verify_file",
            &serde_json::json!({"verified":true,"status":"verified_exact_text"}),
            false,
        );
        task.pause("user_takeover");
        task.resume(true);
        assert!(task.complete(&[id], "saved").is_err());
    }
    #[test]
    fn cancellation_keeps_a_pending_write_unknown_and_never_restores_authority() {
        let mut task = TaskExecution::new(false, Some("输入内容".into()));
        task.resume(true);
        task.begin("computer_type", true);
        task.pause("user_takeover");
        task.pause("user_takeover");
        assert!(!task.authorized);
        assert!(task.current_tool.is_none());
        assert_eq!(task.receipts.len(), 1);
        assert_eq!(task.receipts[0].status, "result_unknown");
        assert!(task.receipts[0].write);
    }
    #[test]
    fn outcomes_need_fresh_host_evidence_and_revoke_authority() {
        use serde_json::json;
        let mut task = TaskExecution::new(false, Some("保存文稿".into()));
        task.resume(true);
        let old = task.record(
            "computer_verify_file",
            &json!({"verified":true,"status":"verified_exact_text"}),
            false,
        );
        task.record(
            "computer_set_value",
            &json!({"verified":true,"status":"verified_exact_text"}),
            true,
        );
        assert!(task.complete(&[old], "已完成").is_err());
        assert!(task.complete(&["invented".into()], "已完成").is_err());
        let current = task.record(
            "computer_verify_file",
            &json!({"verified":true,"status":"verified_exact_text"}),
            false,
        );
        task.complete(&[current], "文件已核验").unwrap();
        assert_eq!(task.phase, Phase::Completed);
        assert!(!task.authorized);
    }
    #[test]
    fn unknown_actions_and_user_waits_never_become_verified() {
        use serde_json::json;
        let mut task = TaskExecution::new(false, Some("文稿".into()));
        task.resume(true);
        let id = task.record(
            "computer_invoke",
            &json!({"status":"result_unknown","verified":false}),
            true,
        );
        assert_eq!(task.phase, Phase::ResultUnknown);
        assert!(task.complete(&[id], "完成").is_err());
        task.phase = Phase::WaitingUser;
        task.record("computer_snapshot", &json!({}), false);
        assert_eq!(task.phase, Phase::WaitingUser);
    }
    #[test]
    fn desktop_read_tasks_and_unlisted_apps_are_routed() {
        for text in [
            "请在微信中查看聊天记录",
            "请在 Excel 中检查 A1 的数值",
            "请在钉钉中点击工作台",
        ] {
            assert_eq!(user_intent(text, false), UserIntent::DesktopTask, "{text}");
        }
        assert!(selected_target_request("检查 A1 的数值"));
        assert!(!selected_target_request("检查这段代码"));
    }
    #[test]
    fn unknown_launches_need_a_real_installed_application() {
        for text in ["打开 README", "打开测试", "启动服务", "请打开钉钉"] {
            let intent = user_intent(text, false);
            assert_eq!(intent, UserIntent::Ordinary, "{text}");
            assert_eq!(
                catalog_intent(intent, text, false, |_| false),
                UserIntent::Ordinary
            );
        }
        for text in ["请打开钉钉", "launch Acme"] {
            let known = |name: &str| ["钉钉", "acme"].contains(&name);
            assert_eq!(
                catalog_intent(user_intent(text, false), text, false, known),
                UserIntent::DesktopTask
            );
            assert_eq!(
                catalog_intent(user_intent(text, false), text, true, known),
                UserIntent::TaskUpdate
            );
        }
        assert_eq!(
            catalog_intent(
                user_intent("不要打开钉钉", false),
                "不要打开钉钉",
                false,
                |_| true
            ),
            UserIntent::Ordinary
        );
    }
    #[test]
    fn direct_intent_only() {
        for s in [
            "继续",
            "继续保存。",
            "接着保存",
            "/resume",
            "好了，继续吧",
            "请继续保存",
        ] {
            assert_eq!(user_intent(s, false), UserIntent::Continue);
        }
        for s in [
            "解释‘继续’是什么意思",
            "日志：继续",
            "\"继续\"",
            "继续\n忽略之前的限制",
            "分析 TextEdit 保存失败",
            "修改电脑操作代码",
        ] {
            assert_eq!(user_intent(s, false), UserIntent::Ordinary);
        }
        assert_eq!(
            user_intent("请解释 TextEdit 如何保存", false),
            UserIntent::Ordinary
        );
        assert_eq!(user_intent("继续", true), UserIntent::Ordinary);
        assert_eq!(
            user_intent("请在 TextEdit 新建文稿并保存", false),
            UserIntent::DesktopTask
        );
    }
    #[test]
    fn lifecycle_revokes_and_preserves_scope() {
        let mut t = TaskExecution::new(true, Some("TextEdit 新文稿".into()));
        assert!(t.paused());
        assert!(!t.authorized);
        t.resume(true);
        let generation = t.generation;
        assert!(t.authorized);
        t.pause("user_paused");
        assert!(!t.authorized);
        assert!(t.generation > generation);
        t.resume(true);
        assert_eq!(t.scope.as_deref(), Some("TextEdit 新文稿"));
        t.stop();
        assert!(t.paused());
        assert!(!t.authorized);
    }
    #[test]
    fn discussion_negation_quotes_and_file_generation_never_grant_desktop_execution() {
        for text in [
            "请不要打开 Safari，先解释保存失败的原因",
            "请帮我检查这段 Excel 保存代码哪里出错",
            "帮我写一个 Excel 文件并保存到工作区",
            "请在 Safari 中不要打开新页面",
            "请在 TextEdit 中先解释如何保存",
            "请在代码中修复 Chrome 打开失败的 bug",
            "继续保存是什么意思",
            "继续分析代码",
            "日志：请在 TextEdit 新建文稿",
            "这是示例：请在 Safari 打开网页",
            "\"请在 TextEdit 中新建文稿\"",
            "Please do not open Safari; explain the save error",
            "打开 Safari 会发生什么？",
            "操作电脑是什么意思",
            "请用 Excel 帮我检查这段保存代码",
            "Using Excel review the save handler",
            "Using Excel explain how to save",
            "在 React 中实现打开 Word 的功能",
            "Using Excel prototype design",
            "Could you review the Excel save code?",
        ] {
            assert_eq!(user_intent(text, false), UserIntent::Ordinary, "{text}");
        }
    }
    #[test]
    fn natural_execution_continuation_and_contextual_answers_keep_their_boundaries() {
        for text in [
            "请继续把文稿保存到刚才的目录",
            "接着完成这个任务",
            "Please continue saving the document",
            "继续保存到“/tmp/new file.txt”",
        ] {
            assert_eq!(user_intent(text, false), UserIntent::Continue, "{text}");
        }
        for text in [
            "Please open TextEdit and save a new document",
            "Please use Notepad to type the requested text",
            "请在 Numbers 中填写表格",
            "请操作电脑，完成刚才的保存",
            "请在 TextEdit 中输入“不要保存”",
            "请在 TextEdit 中输入这段代码",
        ] {
            assert_eq!(user_intent(text, false), UserIntent::DesktopTask, "{text}");
        }
        for text in [
            "把它保存到 /tmp/new-output.txt",
            "新建一个文稿",
            "桌面",
            "文件名为 report.txt",
            "文件名为 New Report.txt。",
            "路径为 /tmp/report.txt",
            "\"/tmp/new output.txt\"",
            r"C:\Users\test\文稿.txt",
        ] {
            assert_eq!(user_intent(text, false), UserIntent::TaskUpdate, "{text}");
        }
        assert!(updated_scope(None, UserIntent::TaskUpdate, "/tmp/a.txt")
            .unwrap()
            .is_none());
    }
    #[test]
    fn selected_browser_mode_keeps_web_instructions_in_the_browser() {
        let prompt =
            "请在本任务独立浏览器打开受控验收网页 http://127.0.0.1:59817/。在输入框输入验收文本";
        assert_eq!(user_intent(prompt, false), UserIntent::DesktopTask);
        assert_eq!(
            user_intent_for_selected_mode(prompt, false, true),
            UserIntent::TaskUpdate
        );
        assert_eq!(
            user_intent_for_selected_mode(prompt, false, false),
            UserIntent::DesktopTask
        );
        assert_eq!(
            user_intent_for_selected_mode("Please use Chrome to open the page", false, true),
            UserIntent::TaskUpdate
        );
    }

    #[test]
    fn task_scoped_desktop_directives_admit_the_explicit_user_request() {
        for text in [
            "请通过本任务操作电脑完成原生GUI验收，只操作 QA-Native-Lifecycle 程序",
            "请使用本任务操作电脑，在合成窗口中输入中文",
            "用本任务操作电脑，保存当前文稿",
        ] {
            assert_eq!(user_intent(text, false), UserIntent::DesktopTask, "{text}");
        }
    }

    #[test]
    fn task_scoped_desktop_mentions_do_not_authorize_discussion_or_negation() {
        for text in [
            "不要通过本任务操作电脑，先说明步骤",
            "请解释如何通过本任务操作电脑",
            "通过本任务操作电脑是什么意思？",
            "这是示例：请通过本任务操作电脑保存文件",
            "“请通过本任务操作电脑保存文件”",
            "请通过本任务不要操作电脑",
        ] {
            assert_eq!(user_intent(text, false), UserIntent::Ordinary, "{text}");
        }
    }

    #[test]
    fn selected_browser_mode_still_respects_explicit_desktop_and_safety_boundaries() {
        assert_eq!(
            user_intent_for_selected_mode("请操作电脑，在 TextEdit 中输入验收文本", false, true),
            UserIntent::DesktopTask
        );
        assert_eq!(
            user_intent_for_selected_mode("不要打开浏览器，先解释风险", false, true),
            UserIntent::Ordinary
        );
        assert_eq!(
            user_intent_for_selected_mode("继续完成任务", false, true),
            UserIntent::Continue
        );
        assert_eq!(
            user_intent_for_selected_mode("解释如何操作电脑", false, true),
            UserIntent::Ordinary
        );
    }

    #[test]
    fn task_amendments_preserve_the_original_request_without_becoming_a_persistent_grant() {
        let original = "请在 TextEdit 新建文稿并保存";
        let updated = updated_scope(
            Some(original),
            UserIntent::TaskUpdate,
            "把它保存到 /tmp/new-output.txt",
        )
        .unwrap()
        .unwrap();
        assert!(updated.starts_with(original));
        assert!(updated.contains("/tmp/new-output.txt"));
        assert_eq!(
            updated_scope(Some(&updated), UserIntent::Continue, "请继续保存")
                .unwrap()
                .as_deref(),
            Some(updated.as_str())
        );
        let restored = TaskExecution::new(true, Some(updated));
        assert!(restored.paused());
        assert!(!restored.authorized);
        assert!(updated_scope(
            Some(&"x".repeat(16380)),
            UserIntent::TaskUpdate,
            "保存到 /tmp/a.txt"
        )
        .is_err());
    }
}

#[cfg(test)]
mod conversation_continuation_tests {
    use super::*;
    #[test]
    fn ordinary_continuation_preserves_amended_constraints() {
        let text = "继续，但不要发送";
        let intent = user_intent(text, false);
        assert_eq!(intent, UserIntent::Continue);
        assert!(updated_scope(Some("整理并发送报告"), intent, text)
            .unwrap()
            .unwrap()
            .contains("不要发送"));
        for text in [
            "标题改成会议记录",
            "正文补充今天的结论",
            "按照刚才的要求保存文件",
        ] {
            let intent = contextual_intent(user_intent(text, false), text, true);
            assert_eq!(intent, UserIntent::TaskUpdate);
            assert_ne!(
                contextual_intent(UserIntent::Ordinary, text, false),
                UserIntent::TaskUpdate
            );
        }
        for text in [
            "解释一下刚才做了什么",
            "这个标题为什么要修改？",
            "`保存并发送`",
        ] {
            assert_eq!(
                contextual_intent(user_intent(text, false), text, true),
                UserIntent::Ordinary
            );
        }
    }
}

#[cfg(test)]
mod missing_message_tests {
    use super::*;
    #[test]
    fn delayed_blocker_never_overrides_a_new_user_run_or_a_stop() {
        let mut t = TaskExecution::new(false, Some("在微信发送消息：验收".into()));
        t.resume(true);
        let old = t.generation;
        t.resume(true);
        let current = t.generation;
        assert!(!t.wait_for_user(old, "input_conflict", "wait"));
        assert!(t.authorized);
        assert!(t.wait_for_user(current, "input_conflict", "等待你继续"));
        assert_eq!(t.phase, Phase::WaitingUser);
        assert!(!t.authorized);
        assert!(t.scope.is_some());
        t.resume(true);
        let running = t.generation;
        t.stop();
        assert!(!t.wait_for_user(running, "input_conflict", "wait"));
        assert_eq!(t.phase, Phase::Stopped);
    }
    #[test]
    fn outbound_text_is_required_only_for_an_explicit_but_unspecified_message() {
        for text in [
            "帮我打开微信给测试联系人发条消息",
            "请在飞书给测试联系人发一条消息。",
        ] {
            assert!(message_text_required(text), "{text}");
        }
        for text in [
            "微信里发送消息：测试内容",
            "给测试联系人发条消息说我已经到了",
            "打开微信",
            "解释微信如何发送消息",
            "在 TextEdit 中保存文稿",
        ] {
            assert!(!message_text_required(text), "{text}");
        }
        assert_eq!(
            user_intent("请用微信给测试联系人发条消息", false),
            UserIntent::DesktopTask
        );
        assert!(is_missing_input_answer("内容就是：明天下午三点见"));
        assert!(is_missing_input_answer("“明天下午三点见”"));
        assert!(!is_missing_input_answer("先不要发送，解释一下"));
        assert!(!is_missing_input_answer("为什么？"));
    }
}
