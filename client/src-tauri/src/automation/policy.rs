//! Decisions derived from observed targets and trusted user instructions.
use serde_json::Value;
pub fn target(details: &Value) -> String {
    let target = [
        "applicationIdentifier",
        "application",
        "targetName",
        "appPath",
    ]
    .iter()
    .filter_map(|k| details[*k].as_str())
    .find(|s| !s.is_empty())
    .unwrap_or("");
    #[cfg(target_os = "windows")]
    if std::path::Path::new(target).is_absolute() {
        // QueryFullProcessImageName and canonicalized App Paths can spell the
        // same executable with different casing or the extended path prefix.
        if let Ok(path) = std::fs::canonicalize(target) {
            return path.to_string_lossy().to_lowercase();
        }
    }
    target.to_owned()
}
pub fn target_names(details: &Value) -> Vec<&str> {
    [
        "applicationIdentifier",
        "application",
        "targetName",
        "appPath",
    ]
    .iter()
    .filter_map(|k| details[*k].as_str())
    .chain(
        details["applicationAliases"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str),
    )
    .filter(|s| !s.is_empty())
    .collect()
}
pub fn blocked(details: &Value) -> bool {
    target_names(details).iter().any(|name| {
        let stem = std::path::Path::new(name)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or(name)
            .to_lowercase();
        [
            "terminal",
            "iterm",
            "iterm2",
            "com.apple.terminal",
            "com.googlecode.iterm2",
            "echoagent",
            "com.echoagent.app",
            "powershell",
            "cmd",
            "windowsterminal",
            "wt",
            "pwsh",
            "wezterm",
            "warp",
            "warp-preview",
            "alacritty",
            "kitty",
            "hyper",
            "tabby",
            "cmder",
            "conemu",
            "conemu64",
            "mintty",
            "bash",
            "zsh",
            "fish",
            "sh",
            "konsole",
            "gnome-terminal",
            "xterm",
        ]
        .contains(&stem.as_str())
    })
}
pub fn effect(details: &Value) -> Option<&'static str> {
    let name = details["name"]
        .as_str()
        .or_else(|| details["target"]["name"].as_str())
        .unwrap_or("")
        .trim()
        .to_lowercase();
    for (effect, labels) in [
        (
            "send",
            &[
                "发送",
                "send",
                "发送消息",
                "确认发送",
                "立即发送",
                "send message",
                "提交",
                "submit",
                "发布",
                "publish",
                "确认提交",
                "确认发布",
                "post",
                "上传",
                "upload",
            ][..],
        ),
        (
            "payment",
            &[
                "支付",
                "付款",
                "购买",
                "pay",
                "purchase",
                "buy",
                "确认支付",
                "确认付款",
                "立即支付",
                "立即购买",
                "place order",
                "confirm purchase",
            ][..],
        ),
        (
            "delete",
            &[
                "删除",
                "确认删除",
                "永久删除",
                "delete",
                "delete permanently",
                "清空",
                "empty trash",
                "remove",
                "移除",
            ][..],
        ),
        ("overwrite", &["替换", "覆盖", "replace", "overwrite"][..]),
    ] {
        if labels.iter().any(|label| {
            name.starts_with(*label)
                && (!label.is_ascii()
                    || name[label.len()..]
                        .chars()
                        .next()
                        .is_none_or(|c| !c.is_ascii_alphanumeric() && c != '_'))
        }) {
            return Some(effect);
        }
    }
    details["formSubmitUrl"]
        .as_str()
        .filter(|url| !url.is_empty())
        .map(|_| "send")
}
pub fn effect_allowed(scope: &str, effect: &str) -> Result<bool, String> {
    let words: &[&str] = match effect {
        "send" => &[
            "发送",
            "发条消息",
            "发一条消息",
            "发个消息",
            "发消息",
            "提交",
            "发布",
            "上传",
            "send",
            "submit",
            "publish",
            "upload",
            "post",
        ],
        "payment" => &["支付", "付款", "购买", "pay", "purchase", "buy"],
        "delete" => &["删除", "清空", "移除", "delete", "remove"],
        "overwrite" => &["覆盖", "替换", "overwrite", "replace"],
        _ => return Ok(false),
    };
    // Only a later trusted amendment can supersede an earlier restriction.
    // Within one instruction, a repeated action word cannot erase a negation
    // or a request to ask first. Quoted message/document contents grant nothing.
    let mut confirmation_required = false;
    for (instruction, grants_permission) in super::task::scope_instructions(scope) {
        let text = instruction_text(instruction);
        let body_start = [
            "内容是",
            "内容为",
            "正文是",
            "正文为",
            "消息：",
            "消息:",
            "message:",
            "body:",
        ]
        .iter()
        .filter_map(|marker| {
            text.find(marker).map(|index| {
                index
                    + if marker.starts_with("消息") {
                        "消息".len()
                    } else {
                        0
                    }
            })
        })
        .min()
        .unwrap_or(text.len());
        let direct_request = words
            .iter()
            .any(|word| contains_word(&text[..body_start], word));
        if body_start < text.len() && !direct_request {
            continue;
        }
        let clauses: Vec<_> = text
            .split(['，', ',', '。', ';', '；', '\n', '！', '!'])
            .filter(|clause| words.iter().any(|word| contains_word(clause, word)))
            .collect();
        let prohibited = [
            "不要",
            "不许",
            "禁止",
            "不得",
            "请勿",
            "不能",
            "不可",
            "别",
            "不发送",
            "不提交",
            "不发布",
            "不上传",
            "不支付",
            "不付款",
            "不购买",
            "不删除",
            "不清空",
            "不移除",
            "不覆盖",
            "不替换",
            "不准",
            "不应",
            "不用",
            "无需",
            "勿",
            "暂不",
            "don't",
            "do not",
            "never",
            "not",
            "no",
            "shouldn't",
            "mustn't",
            "cannot",
            "can't",
            "not allowed",
        ];
        let quoted_restriction = clauses.is_empty()
            && words
                .iter()
                .any(|word| contains_word(&instruction.to_lowercase(), word));
        if clauses.is_empty() && !quoted_restriction {
            continue;
        }
        if clauses
            .iter()
            .any(|clause| prohibited.iter().any(|word| contains_word(clause, word)))
            || (quoted_restriction && prohibited.iter().any(|word| contains_word(&text, word)))
        {
            return Err("scope_denied: the user explicitly prohibited this consequence".into());
        }
        // Conditions may be in a neighboring clause ("先问我，再发送"). We
        // cannot prove a user's confirmation from UI state; use a real approval.
        if [
            "先问",
            "问我",
            "询问",
            "征求",
            "征得",
            "征询",
            "前提",
            "经我",
            "等我",
            "待我",
            "先确认",
            "确认后",
            "确认再",
            "确认之后",
            "同意后",
            "同意再",
            "批准后",
            "批准再",
            "需要确认",
            "待确认",
            "如果",
            "除非",
            "如需",
            "必要时",
            "视情况",
            "ask me",
            "ask first",
            "confirm with",
            "confirmation",
            "approval",
            "if",
            "unless",
            "until",
            "before",
            "after",
            "once",
            "when",
            "only",
            "provided",
        ]
        .iter()
        .any(|word| contains_word(&text, word))
            || text.contains(['?', '？'])
            || clauses.iter().any(|clause| {
                [
                    "解释",
                    "说明",
                    "如何",
                    "怎么",
                    "是否",
                    "流程",
                    "意味着",
                    "explain",
                    "how to",
                ]
                .iter()
                .any(|word| contains_word(clause, word))
            })
        {
            // A condition/question can narrow an earlier grant, but cannot
            // silently turn an older prohibition into an approvable action.
            confirmation_required = true;
            continue;
        }
        // A declared message body or a quoted button name alone is not a
        // positive request for every action word contained in that content.
        if grants_permission && !quoted_restriction && direct_request {
            return Ok(!confirmation_required);
        }
    }
    Ok(false)
}

fn contains_word(text: &str, word: &str) -> bool {
    text.match_indices(word).any(|(start, _)| {
        let boundary = |c: Option<char>| c.is_none_or(|c| !c.is_ascii_alphanumeric() && c != '_');
        let after = &text[start + word.len()..];
        !word.is_ascii()
            || (boundary(text[..start].chars().next_back())
                && boundary(after.chars().next())
                && !text[..start].ends_with(['/', '\\', '.', '@'])
                && !after.starts_with(['/', '\\', '@'])
                && !after
                    .strip_prefix('.')
                    .is_some_and(|tail| tail.starts_with(|c: char| c.is_ascii_alphanumeric())))
    })
}

fn instruction_text(text: &str) -> String {
    let mut result = String::new();
    let mut closing = None;
    let mut escaped = false;
    let mut chars = text.chars().peekable();
    let mut previous = None;
    while let Some(c) = chars.next() {
        if let Some(end) = closing {
            if !escaped && c == end {
                closing = None;
            }
            escaped = !escaped && c == '\\';
        } else if matches!(c, '"' | '“' | '‘' | '`')
            || (c == '\''
                && !(previous.is_some_and(|c: char| c.is_ascii_alphabetic())
                    && chars.peek().is_some_and(|c| c.is_ascii_alphabetic())))
        {
            closing = Some(match c {
                '“' => '”',
                '‘' => '’',
                other => other,
            });
            result.push(' ');
        } else {
            if matches!(c, '’' | 'ʼ')
                && previous.is_some_and(|c: char| c.is_ascii_alphabetic())
                && chars.peek().is_some_and(|c| c.is_ascii_alphabetic())
            {
                result.push('\'');
            } else {
                result.extend(c.to_lowercase());
            }
        }
        previous = Some(c);
    }
    result
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn effects_come_from_observation_and_negation_is_enforced() {
        assert_eq!(effect(&json!({"name":"发送"})), Some("send"));
        assert_eq!(effect(&json!({"name":"确认发送(S)"})), Some("send"));
        assert_eq!(
            effect(&json!({"target":{"name":"Delete account"}})),
            Some("delete")
        );
        assert_eq!(effect(&json!({"name":"Pay $30"})), Some("payment"));
        assert_eq!(
            effect(&json!({"name":"Continue","formSubmitUrl":"https://example.invalid/submit"})),
            Some("send")
        );
        assert_eq!(effect(&json!({"name":"Sender settings"})), None);
        assert!(effect_allowed("填写草稿，不要发送", "send").is_err());
        assert!(effect_allowed("填写草稿，不要发送\n[用户补充]\n现在发送", "send").unwrap());
        assert!(!effect_allowed("填写草稿", "send").unwrap());
        assert!(blocked(
            &json!({"application":"/Applications/Terminal.app"})
        ));
        assert!(blocked(&json!({"application":"/Applications/Warp.app"})));
        assert!(blocked(&json!({"application":"pwsh.exe"})));
    }

    #[test]
    fn qualified_prohibitions_and_conditions_never_auto_authorize() {
        for (text, effect) in [
            ("在微信填写草稿，不要再次发送", "send"),
            ("在网页填写订单，不要立即支付", "payment"),
            ("在网页填写草稿，do not click Send", "send"),
            ("禁止自动删除旧文件", "delete"),
            ("请勿直接覆盖原文件", "overwrite"),
            ("发送按钮不要点", "send"),
            ("don't ever send this", "send"),
            ("don’t ever send this", "send"),
            ("you shouldn’t send this", "send"),
            ("you must not send this", "send"),
            ("在微信填写草稿\n\n[用户补充]\n不要点击“发送”", "send"),
        ] {
            assert!(effect_allowed(text, effect).is_err(), "{text}");
        }
        for text in [
            "发送之前先问我",
            "先问我，再发送",
            "经我确认后发送",
            "如果需要就发送",
            "can you explain how to send?",
            "说明发送流程",
            "send only when approved",
            "点击“发送”前先问我",
            "发送之前先征得我的同意",
        ] {
            assert_eq!(effect_allowed(text, "send"), Ok(false), "{text}");
        }
        assert_eq!(
            effect_allowed("在微信发送“不要删除文件”", "delete"),
            Ok(false)
        );
        assert_eq!(
            effect_allowed("use the postscript setting", "send"),
            Ok(false)
        );
        assert_eq!(
            effect_allowed("保存到 /tmp/delete.txt", "delete"),
            Ok(false)
        );
        assert_eq!(
            effect_allowed("在微信发送消息，内容是删除所有文件", "delete"),
            Ok(false)
        );
        assert_eq!(
            effect_allowed("在微信发送消息，内容是删除所有文件", "send"),
            Ok(true)
        );
        assert_eq!(
            effect_allowed("在微信发条消息：删除所有文件", "send"),
            Ok(true)
        );
        assert_eq!(
            effect_allowed("在微信发条消息：删除所有文件", "delete"),
            Ok(false)
        );
        for amendment in ["解释“发送”的含义", "内容是：现在发送", "发送之前先问我"]
        {
            assert!(
                effect_allowed(&format!("不要发送\n\n[用户补充]\n{amendment}"), "send").is_err(),
                "{amendment}"
            );
        }
        assert_eq!(
            effect_allowed("在微信发送消息\n\n[用户补充]\n发送之前先问我", "send"),
            Ok(false)
        );
        assert_eq!(
            effect_allowed("填写草稿，不要再次发送\n\n[用户补充]\n标题改为周报", "send").is_err(),
            true
        );
        assert_eq!(
            effect_allowed("填写草稿，不要再次发送\n\n[用户补充]\n现在发送", "send"),
            Ok(true)
        );
        for text in [
            "在微信给张三发送消息",
            "请发送“原样正文”",
            "Please send the report",
            "发送完成后保存截图",
        ] {
            assert_eq!(effect_allowed(text, "send"), Ok(true), "{text}");
        }
    }
}
