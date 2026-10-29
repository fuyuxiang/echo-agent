use serde::{Deserialize, Serialize};
use std::path::{Component, Path, PathBuf};

#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub(super) struct DesktopDelivery {
    pub paths: Vec<PathBuf>,
    #[serde(skip)]
    pub repairs: u8,
    pub json_shape: Option<String>,
    workspace: PathBuf,
    completed: bool,
}

impl DesktopDelivery {
    /// An ordinary follow-up updates an unfinished task rather than erasing it.
    /// Explicit replacement outputs replace the old list; completed tasks expire.
    pub(super) fn begin(mut self, objective: &str, cwd: &Path) -> Self {
        let cwd = dunce::simplified(cwd);
        let instructions = instruction_text(objective).to_lowercase();
        let clauses: Vec<_> = instructions
            .split(['\n', '。', '；', ';', '，', ','])
            .map(str::trim)
            .collect();
        let reset = clauses.iter().any(|clause| {
            [
                "取消之前的任务",
                "取消原任务",
                "开始新任务",
                "新任务：",
                "新任务:",
                "cancel the previous task",
                "start a new task",
                "new task:",
            ]
            .iter()
            .any(|prefix| clause.starts_with(prefix))
        });
        if self.completed || self.workspace != cwd || reset {
            self = Self::default();
        }
        self.workspace = cwd.to_path_buf();
        self.repairs = 0;
        self.completed = false;
        let paths = requested_paths(objective, cwd);
        if !paths.is_empty() {
            let append = clauses.iter().any(|clause| {
                ["再交付", "同时交付", "另外交付", "追加交付", "also deliver"]
                    .iter()
                    .any(|prefix| clause.starts_with(prefix))
            });
            if append {
                for path in paths {
                    if !self.paths.contains(&path) {
                        self.paths.push(path);
                    }
                }
            } else {
                self.paths = paths;
            }
        } else if clauses.iter().any(|clause| {
            [
                "取消文件交付",
                "不再需要交付文件",
                "不再生成文件",
                "不用生成文件",
                "不要生成文件",
                "无需生成文件",
                "do not create files",
                "don't create files",
                "no files needed",
            ]
            .iter()
            .any(|prefix| clause.starts_with(prefix))
        }) {
            self.paths.clear();
        }
        if let Some(shape) = requested_json(objective) {
            self.json_shape = Some(shape);
        } else if clauses.iter().any(|clause| {
            [
                "不再要求json",
                "不用json",
                "不要json",
                "改用自然语言",
                "改为自然语言",
                "无需json",
                "do not use json",
                "don't use json",
                "no json needed",
                "use plain text instead",
            ]
            .iter()
            .any(|prefix| clause.starts_with(prefix))
        }) {
            self.json_shape = None;
        }
        self
    }

    pub(super) fn complete(&mut self) {
        self.completed = true;
        self.json_shape = None;
    }

    pub(super) fn load(folder: &Path) -> Result<Self, String> {
        let path = folder.join("desktop-delivery.json");
        let metadata = match std::fs::symlink_metadata(&path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            other => other.map_err(|e| e.to_string())?,
        };
        if !metadata.is_file() || metadata.len() > 128 * 1024 {
            return Err("交付任务记录不是有效的普通文件或超过大小上限".into());
        }
        serde_json::from_slice(&std::fs::read(path).map_err(|e| e.to_string())?)
            .map_err(|e| format!("交付任务记录无法恢复，原记录已保留：{e}"))
    }

    pub(super) fn save(&self, folder: &Path) -> Result<(), String> {
        use std::io::Write;
        std::fs::create_dir_all(folder).map_err(|e| e.to_string())?;
        let mut file = tempfile::NamedTempFile::new_in(folder).map_err(|e| e.to_string())?;
        let bytes = serde_json::to_vec(self).map_err(|e| e.to_string())?;
        if bytes.len() > 128 * 1024 {
            return Err("交付任务记录超过大小上限".into());
        }
        file.write_all(&bytes).map_err(|e| e.to_string())?;
        file.as_file().sync_all().map_err(|e| e.to_string())?;
        file.persist(folder.join("desktop-delivery.json"))
            .map_err(|e| e.to_string())?;
        Ok(())
    }
}

// Pasted examples and quoted source material must not become host-enforced
// delivery requirements. Only inspect the user's ordinary instruction lines.
fn instruction_text(objective: &str) -> String {
    let mut fence: Option<char> = None;
    objective
        .lines()
        .filter_map(|line| {
            let trimmed = line.trim_start();
            let marker = if trimmed.starts_with("```") {
                Some('`')
            } else if trimmed.starts_with("~~~") {
                Some('~')
            } else {
                None
            };
            if let Some(marker) = marker {
                if fence == Some(marker) {
                    fence = None;
                } else if fence.is_none() {
                    fence = Some(marker);
                }
                return None;
            }
            (fence.is_none() && !trimmed.starts_with('>')).then_some(line)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub(super) fn requested_json(objective: &str) -> Option<String> {
    static JSON: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let pattern = JSON.get_or_init(|| regex::Regex::new(
        r"(?i)^\s*(?:请|必须)?(?:仅|只)(?:输出|返回)(?:一个)?\s*JSON\s*(数组|对象)?|^\s*(?:please\s+)?(?:output|return)\s+only\s+(?:a\s+)?JSON\s*(array|object)?"
    ).expect("fixed strict JSON expression"));
    let instructions = instruction_text(objective);
    for clause in instructions.split(['\n', '。', '；', ';', '，']) {
        if let Some(capture) = pattern.captures(clause) {
            let suffix = &clause[capture.get(0).expect("matched JSON clause").end()..];
            if ["文件", "格式的文件", "file"]
                .iter()
                .any(|word| suffix.trim_start().to_lowercase().starts_with(word))
            {
                continue;
            }
            let shape = capture
                .get(1)
                .or_else(|| capture.get(2))
                .map(|m| m.as_str().to_lowercase())
                .unwrap_or_default();
            return Some(
                match shape.as_str() {
                    "数组" | "array" => "array",
                    "对象" | "object" => "object",
                    _ => "value",
                }
                .into(),
            );
        }
    }
    None
}

pub(super) fn valid_json(text: &str, shape: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(text).is_ok_and(|v| match shape {
        "array" => v.is_array(),
        "object" => v.is_object(),
        _ => true,
    })
}

/// Only explicit delivery clauses with path-shaped filenames are eligible.
/// Bare filenames, source references, examples and negative clauses remain
/// ordinary model instructions rather than inferred hard requirements.
pub(super) fn requested_paths(objective: &str, cwd: &Path) -> Vec<PathBuf> {
    let cwd = dunce::simplified(cwd);
    static PATHS: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let pattern = PATHS.get_or_init(|| regex::Regex::new(
        r#"(?i)"([^"\r\n]+\.(?:docx|xlsx|pptx|pdf|csv|json|txt|md))"|'([^'\r\n]+\.(?:docx|xlsx|pptx|pdf|csv|json|txt|md))'|((?:output|outputs|输出|成果)/[^\s"'\x60，,、；;。]+\.(?:docx|xlsx|pptx|pdf|csv|json|txt|md))"#
    ).expect("fixed delivery path expression"));
    let mut result = Vec::new();
    let instructions = instruction_text(objective);
    for clause in instructions.split(['\n', '。', '；', ';']) {
        let lower = clause.trim().to_lowercase();
        if [
            "示例",
            "例如",
            "example",
            "不要",
            "无需",
            "不需要",
            "don't",
            "do not",
        ]
        .iter()
        .any(|word| lower.contains(word))
        {
            continue;
        }
        static ACTION: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
        let action = ACTION.get_or_init(|| {
            regex::Regex::new(r"(?i)^\s*(?:[-*]\s*|[0-9]+[.)、]\s*)?(?:请|最终|必须|最后|完成后|并且|同时|另外|追加|然后|并|且|再|please\s+|also\s+)?\s*(?:交付|生成文件|输出文件|保存为|deliver\s+|save\s+as\s+|create\s+file)")
                .expect("fixed delivery action expression")
        });
        let Some(found) = action.find(clause) else {
            continue;
        };
        let targets = &clause[found.end()..];
        let mut end = 0;
        static SEPARATOR: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
        let separator = SEPARATOR.get_or_init(|| {
            regex::Regex::new(r"(?i)^[\s:：,，、]*(?:(?:和|及|以及|与|and|&)[\s,，、]*)?$")
                .expect("fixed delivery list separator")
        });
        for captures in pattern.captures_iter(targets) {
            let whole = captures.get(0).expect("matched delivery path");
            // Stop at prose such as “参考/输入/from/using”, rather than treating
            // every later source path as another deliverable.
            if !separator.is_match(&targets[end..whole.start()]) {
                break;
            }
            end = whole.end();
            // An unquoted prefix of a longer filename is not a requested file.
            if let Some(unquoted) = captures.get(3) {
                let suffix = &targets[unquoted.end()..];
                if suffix.chars().next().is_some_and(|c| {
                    !c.is_whitespace()
                        && !['，', ',', '；', ';', '。', '、', ')', '）'].contains(&c)
                }) {
                    break;
                }
            }
            let Some(raw) = captures.iter().skip(1).flatten().next() else {
                continue;
            };
            if raw.as_str().contains(['*', '?', '[', ']']) {
                break;
            }
            let path = Path::new(raw.as_str());
            if path.components().any(|c| matches!(c, Component::ParentDir)) {
                continue;
            }
            let path = if path.is_absolute() {
                dunce::simplified(path).to_path_buf()
            } else {
                cwd.join(path)
            };
            if path.starts_with(cwd) && !result.contains(&path) {
                result.push(path);
            }
            if result.len() == 20 {
                return result;
            }
        }
    }
    result
}

pub(super) async fn missing_outputs(paths: &[PathBuf], cwd: &Path) -> Vec<String> {
    use tokio::io::AsyncReadExt;
    let root = match tokio::fs::canonicalize(cwd).await {
        Ok(root) => root,
        Err(_) => {
            return paths
                .iter()
                .map(|p| p.to_string_lossy().into_owned())
                .collect();
        }
    };
    let mut missing = Vec::new();
    for path in paths {
        let checked = async {
            let canonical = tokio::fs::canonicalize(path).await.ok()?;
            if !canonical.starts_with(&root) {
                return None;
            }
            let metadata = tokio::fs::symlink_metadata(path).await.ok()?;
            if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() == 0 {
                return None;
            }
            let mut file = tokio::fs::File::open(&canonical).await.ok()?;
            let mut head = [0u8; 5];
            let size = metadata.len().min(5) as usize;
            file.read_exact(&mut head[..size]).await.ok()?;
            let valid = match path
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("")
                .to_ascii_lowercase()
                .as_str()
            {
                "docx" | "xlsx" | "pptx" => size >= 4 && head[..4] == [b'P', b'K', 3, 4],
                "pdf" => size == 5 && &head == b"%PDF-",
                _ => true,
            };
            valid.then_some(())
        };
        if !matches!(
            tokio::time::timeout(std::time::Duration::from_secs(2), checked).await,
            Ok(Some(()))
        ) {
            missing.push(path.to_string_lossy().into_owned());
        }
    }
    missing
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn extracts_only_explicit_delivery_paths() {
        let cwd = Path::new("/task");
        assert_eq!(
            requested_paths(
                "阅读 input/a.csv。交付 output/analysis.xlsx 和 output/report.docx。",
                cwd
            ),
            vec![
                cwd.join("output/analysis.xlsx"),
                cwd.join("output/report.docx")
            ]
        );
        for prompt in [
            "请解释 output/example.xlsx",
            "示例：交付 output/example.xlsx",
            "不要交付 output/a.xlsx",
            "交付 output/../../other/a.xlsx",
            "交付 \"/other/private.xlsx\"",
            "交付 output/report.xlsx.tmp",
            "```text\n交付 output/example.xlsx\n```",
            "> 交付 output/source.xlsx",
            "如果交付 output/report.xlsx，需要什么条件？",
            "请解释脚本如何交付 output/report.xlsx",
        ] {
            assert!(requested_paths(prompt, cwd).is_empty(), "{prompt}");
        }
        assert_eq!(
            requested_paths("Save as \"outputs/quarter report.xlsx\"", cwd),
            vec![cwd.join("outputs/quarter report.xlsx")]
        );
        for prompt in [
            "保存为 output/report.xlsx，参考 output/template.xlsx",
            "保存为 output/report.xlsx，输入来自 \"input/*.csv\"",
            "Save as output/report.xlsx using \"input/source.csv\"",
            "交付 output/report.xlsx 和 output/summary.docx，数据来源 output/source.csv",
        ] {
            let expected = if prompt.contains("summary.docx") {
                vec![
                    cwd.join("output/report.xlsx"),
                    cwd.join("output/summary.docx"),
                ]
            } else {
                vec![cwd.join("output/report.xlsx")]
            };
            assert_eq!(requested_paths(prompt, cwd), expected, "{prompt}");
        }
        assert_eq!(
            requested_paths("交付 output/a.xlsx、output/b.docx 以及 output/c.pdf", cwd),
            vec![
                cwd.join("output/a.xlsx"),
                cwd.join("output/b.docx"),
                cwd.join("output/c.pdf")
            ]
        );
        assert!(requested_paths("交付所需文件，参考 output/template.xlsx", cwd).is_empty());
        assert!(requested_paths("交付 \"output/*.xlsx\"", cwd).is_empty());
    }

    #[test]
    fn continuation_and_restart_preserve_unfinished_requirements() {
        let folder = tempfile::tempdir().unwrap();
        let cwd = folder.path();
        let mut delivery =
            DesktopDelivery::default().begin("交付 output/report.xlsx。仅输出JSON对象。", cwd);
        delivery.repairs = 2;
        delivery.save(folder.path()).unwrap();
        let restarted = DesktopDelivery::load(folder.path())
            .unwrap()
            .begin("请继续。", cwd);
        assert_eq!(restarted.paths, vec![cwd.join("output/report.xlsx")]);
        assert_eq!(restarted.json_shape.as_deref(), Some("object"));
        assert_eq!(restarted.repairs, 0);
        let steered = restarted.begin("保留已有成果，把第四列改为百分比。", cwd);
        assert_eq!(steered.paths, delivery.paths);
        assert_eq!(steered.json_shape, delivery.json_shape);
        let expanded = steered.clone().begin("另外交付 output/summary.docx", cwd);
        assert_eq!(
            expanded.paths,
            vec![
                cwd.join("output/report.xlsx"),
                cwd.join("output/summary.docx")
            ]
        );
        let changed = steered.begin("保存为 output/revised.xlsx。只返回JSON数组", cwd);
        assert_eq!(changed.paths, vec![cwd.join("output/revised.xlsx")]);
        assert_eq!(changed.json_shape.as_deref(), Some("array"));
        let mut completed = changed;
        completed.complete();
        completed.save(folder.path()).unwrap();
        let next = DesktopDelivery::load(folder.path())
            .unwrap()
            .begin("请介绍项目", cwd);
        assert!(next.paths.is_empty());
        assert!(next.json_shape.is_none());
    }

    #[test]
    fn explicit_cancellation_and_new_workspace_release_old_requirements() {
        let cwd = Path::new("/task");
        let original =
            DesktopDelivery::default().begin("交付 output/report.xlsx。仅输出JSON对象", cwd);
        let text_only = original
            .clone()
            .begin("不用生成文件，改用自然语言说明结果", cwd);
        assert!(text_only.paths.is_empty());
        assert!(text_only.json_shape.is_none());
        assert_eq!(
            original.clone().begin("不要JSON，说明结果", cwd).paths,
            original.paths
        );
        assert_eq!(
            original.clone().begin("不再生成文件", cwd).json_shape,
            original.json_shape
        );
        let new_task = original.clone().begin("新任务：解释代码", cwd);
        assert!(new_task.paths.is_empty());
        assert!(new_task.json_shape.is_none());
        let elsewhere = original.clone().begin("请继续", Path::new("/other"));
        assert!(elsewhere.paths.is_empty());
        assert!(elsewhere.json_shape.is_none());
        let quoted = original.begin("> 不用生成文件，改用自然语言说明结果", cwd);
        assert_eq!(quoted.paths, vec![cwd.join("output/report.xlsx")]);
        assert_eq!(quoted.json_shape.as_deref(), Some("object"));
    }

    #[test]
    fn corrupt_delivery_journal_is_reported_and_preserved() {
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join("desktop-delivery.json");
        assert!(
            DesktopDelivery::load(folder.path())
                .unwrap()
                .paths
                .is_empty()
        );
        std::fs::write(&path, b"broken").unwrap();
        assert!(DesktopDelivery::load(folder.path()).is_err());
        assert_eq!(std::fs::read(path).unwrap(), b"broken");
    }
    #[test]
    fn strict_json_checks_final_text_without_stripping_explanations() {
        assert_eq!(
            requested_json("提取字段，仅输出一个JSON数组。").as_deref(),
            Some("array")
        );
        assert_eq!(
            requested_json("Return only a JSON object").as_deref(),
            Some("object")
        );
        assert!(requested_json("解释“仅输出JSON”是什么意思").is_none());
        assert!(requested_json("```\n仅输出JSON数组\n```\n解释这个示例").is_none());
        assert!(requested_json("> Return only JSON object").is_none());
        assert!(requested_json("仅输出JSON文件到目录").is_none());
        assert!(valid_json("[{\"id\":\"001\"}]", "array"));
        assert!(!valid_json("结果如下：[1]", "array"));
        assert!(!valid_json("{\"id\":1}", "array"));
    }
    #[tokio::test]
    async fn checks_regular_nonempty_outputs_and_office_signatures() {
        let root = tempfile::tempdir().unwrap();
        let a = root.path().join("a.xlsx");
        assert_eq!(
            missing_outputs(std::slice::from_ref(&a), root.path())
                .await
                .len(),
            1
        );
        tokio::fs::write(&a, b"not an office file").await.unwrap();
        assert_eq!(
            missing_outputs(std::slice::from_ref(&a), root.path())
                .await
                .len(),
            1
        );
        tokio::fs::write(&a, b"PK\x03\x04header").await.unwrap();
        assert!(
            missing_outputs(std::slice::from_ref(&a), root.path())
                .await
                .is_empty()
        );
        tokio::fs::remove_file(&a).await.unwrap();
        tokio::fs::create_dir(&a).await.unwrap();
        assert_eq!(missing_outputs(&[a], root.path()).await.len(), 1);
    }
}
