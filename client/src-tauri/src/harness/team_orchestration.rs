//! Conservative preflight for explicit requests naming existing experts.
use regex::Regex;
use std::sync::LazyLock;

#[derive(Debug, PartialEq)]
pub struct TeamRequest {
    pub name: String,
    pub members: Vec<String>,
}

pub fn referenced_team(text: &str, team_ids: &[String]) -> Result<Option<String>, String> {
    if ["不要", "不用", "不需要", "停止", "取消", "解散"]
        .iter()
        .any(|word| text.contains(word))
    {
        return Ok(None);
    }
    static QUOTED: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(
            r#"(?:使用|用)\s*(?:任务编组|专家团队|专家团|团队)\s*[“「\"']([^”」\"']+)[”」\"']"#,
        )
        .expect("valid team reference")
    });
    if let Some(capture) = QUOTED.captures(text) {
        let id = capture[1].trim();
        return team_ids
            .iter()
            .find(|name| name.as_str() == id)
            .cloned()
            .map(Some)
            .ok_or_else(|| format!("本会话没有团队“{id}”，请先创建或选择已有团队。"));
    }
    let matches = team_ids
        .iter()
        .filter(|id| {
            let pattern = format!(
                r#"(?:任务编组|专家团队|专家团|团队)\s*(?:“{}”|「{}」|{}(?:[\s，,。；;：:的]|$))"#,
                regex::escape(id),
                regex::escape(id),
                regex::escape(id)
            );
            Regex::new(&pattern).is_ok_and(|expression| expression.is_match(text))
        })
        .collect::<Vec<_>>();
    static UNNAMED: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?:使用|用|让)\s*(?:任务编组|专家团队|专家团|团队)(?:来|完成|处理|分工|协作|，|,|\s|$)|按(?:任务编组|专家团队|专家团|团队)分工")
            .expect("valid unnamed team reference")
    });
    let continuing =
        ["继续", "请继续", "继续执行"].contains(&text.trim_end_matches(['。', '！', '!']).trim());
    Ok(if matches.len() == 1 {
        Some(matches[0].clone())
    } else if matches.is_empty() && team_ids.len() == 1 && (UNNAMED.is_match(text) || continuing) {
        Some(team_ids[0].clone())
    } else {
        None
    })
}

pub fn explicit_team_request(
    text: &str,
    available: &[String],
) -> Result<Option<TeamRequest>, String> {
    let text = text.trim();
    if text.contains(['?', '？', '`', '\n'])
        || [
            "如何",
            "怎么",
            "为什么",
            "不要",
            "不需要",
            "别创建",
            "是否",
            "如果",
            "假如",
        ]
        .iter()
        .any(|word| text.contains(word))
    {
        return Ok(None);
    }
    static REQUEST: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(
        r#"^(?:请|帮我|请帮我|麻烦)?\s*(?:使用|用)\s*(?:已经有的|已有的|现有的|已有|现有|本地的)?(?P<members>.+?)(?:来)?(?:创建|组建|建立|新建)(?:一个)?(?:任务编组|专家团队|专家团|团队)\s*[「“\"'‘]?(?P<name>[^\s，,。；;：:！!「」“”\"'‘’]+)[」”\"'’]?(?:[\s，,。；;：:！!]|$)"#
    ).expect("valid team request pattern")
    });
    let Some(capture) = REQUEST.captures(text) else {
        return Ok(None);
    };
    let names = capture["members"]
        .trim()
        .trim_end_matches(['来', '，', ',', ' ']);
    static SEPARATOR: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r#"[、，,]+|以及|和|与|及"#).expect("valid member separator"));
    let mut members = Vec::new();
    for part in SEPARATOR.split(names) {
        let name = part
            .trim()
            .trim_matches(['「', '」', '“', '”', '"', '\'', '‘', '’'])
            .trim();
        if name.is_empty() {
            continue;
        }
        if !available.iter().any(|agent| agent == name) {
            return Err(format!("无法创建团队：本地专家“{name}”不存在。请在专家页面创建，或在团队面板选择已有专家。"));
        }
        if !members.contains(&name.to_string()) {
            members.push(name.to_string());
        }
    }
    if members.is_empty() {
        return Ok(None);
    }
    Ok(Some(TeamRequest {
        name: capture["name"].to_string(),
        members,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn agents() -> Vec<String> {
        ["业务分析师", "代码专家", "文档助手"]
            .map(str::to_string)
            .to_vec()
    }

    #[test]
    fn expert_team_commands_preserve_legacy_names_and_intent_guards() {
        let ids = ["1".to_string(), "123".to_string()];
        for label in ["任务编组", "专家团", "团队", "专家团队"] {
            let request = explicit_team_request(
                &format!("请用代码专家、文档助手创建{label}「123」，完成任务"),
                &agents(),
            )
            .unwrap()
            .unwrap();
            assert_eq!(request.name, "123");
            assert_eq!(request.members, ["代码专家", "文档助手"]);
            for prompt in [
                format!("请使用{label}“123”，完成任务"),
                format!("请用{label}123，完成任务"),
            ] {
                assert_eq!(
                    referenced_team(&prompt, &ids).unwrap().as_deref(),
                    Some("123")
                );
            }
            assert_eq!(
                referenced_team(&format!("请让{label}完成任务"), &ids[..1])
                    .unwrap()
                    .as_deref(),
                Some("1")
            );
            assert!(referenced_team(&format!("请使用{label}“999”完成任务"), &ids).is_err());
            assert_eq!(
                referenced_team(&format!("不要使用{label}1"), &ids).unwrap(),
                None
            );
            assert!(
                explicit_team_request(&format!("怎么用代码专家创建{label}123？"), &agents())
                    .unwrap()
                    .is_none()
            );
            assert!(
                explicit_team_request(&format!("请不要用代码专家创建{label}123"), &agents())
                    .unwrap()
                    .is_none()
            );
            assert!(explicit_team_request(
                &format!("请用缺失专家创建{label}123，完成任务"),
                &agents()
            )
            .is_err());
        }
    }

    #[test]
    fn registers_the_reported_request_and_quoted_names() {
        let request = explicit_team_request(
            "请使用已经有的业务分析师、代码专家、文档助手创建团队123，分别分析需求并生成代码和文档",
            &agents(),
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            request,
            TeamRequest {
                name: "123".into(),
                members: agents()
            }
        );
        assert_eq!(
            explicit_team_request(
                "请用“代码专家”和“文档助手”创建团队「交付组」，完成任务",
                &agents()
            )
            .unwrap()
            .unwrap()
            .name,
            "交付组"
        );
    }

    #[test]
    fn does_not_register_questions_negations_examples_or_partial_members() {
        for text in [
            "怎么使用代码专家创建团队123？",
            "请不要用代码专家创建团队123",
            "如果用代码专家创建团队123，会怎样",
            "请解释 `用代码专家创建团队123`",
            "请用代码专家完成任务",
        ] {
            assert!(
                explicit_team_request(text, &agents()).unwrap().is_none(),
                "{text}"
            );
        }
        assert!(explicit_team_request(
            "请用代码专家、尚未创建的专家创建团队123，完成任务",
            &agents()
        )
        .unwrap_err()
        .contains("不存在"));
    }

    #[test]
    fn team_references_do_not_confuse_prefixes_or_unknown_names() {
        let ids = ["1".to_string(), "123".to_string()];
        assert_eq!(
            referenced_team("请用团队123，完成任务", &ids)
                .unwrap()
                .as_deref(),
            Some("123")
        );
        assert_eq!(
            referenced_team("请用团队“1”完成任务", &ids)
                .unwrap()
                .as_deref(),
            Some("1")
        );
        assert!(referenced_team("请使用团队“999”完成任务", &ids).is_err());
        assert_eq!(
            referenced_team("今天的天气如何？", &ids[..1]).unwrap(),
            None
        );
        assert_eq!(referenced_team("不要使用团队1", &ids[..1]).unwrap(), None);
    }
}
