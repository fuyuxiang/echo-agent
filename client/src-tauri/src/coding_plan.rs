//! An explicit UI action may save precisely one new plan file without changing run permissions.
use crate::shell_fs::FilesystemAccess;
use std::io::Write;
use std::path::{Component, Path};
use tauri::State;

fn save_plan(root: &Path, relative: &str, content: &str) -> Result<String, String> {
    let path = Path::new(relative);
    if path.is_absolute()
        || path
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
        || path.extension().and_then(|ext| ext.to_str()) != Some("md")
        || content.len() > 256 * 1024
        || content.contains('\0')
    {
        return Err("计划需要工程内的新 Markdown 相对路径，内容上限 256KiB".into());
    }
    let target = root.join(path);
    let parent = target
        .parent()
        .ok_or("计划路径无效")?
        .canonicalize()
        .map_err(|_| "计划目录不存在")?;
    if !parent.starts_with(root) {
        return Err("计划目录位于工程之外".into());
    }
    let destination = parent.join(target.file_name().ok_or("计划路径无效")?);
    let mut file = std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&destination)
        .map_err(|error| format!("计划未保存（不会覆盖已有文件）：{error}"))?;
    file.write_all(content.as_bytes())
        .map_err(|error| format!("计划保存失败：{error}"))?;
    file.sync_all().map_err(|error| error.to_string())?;
    Ok(destination.to_string_lossy().into_owned())
}
#[tauri::command]
pub async fn coding_plan_save(
    access: State<'_, FilesystemAccess>,
    root: String,
    path: String,
    content: String,
) -> Result<String, String> {
    let root = access.require_workspace(&root)?;
    crate::coding_document_bridge::ensure_workspace_admission(&root.to_string_lossy()).await?;
    tokio::task::spawn_blocking(move || save_plan(&root, &path, &content))
        .await
        .map_err(|error| error.to_string())?
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn saves_only_the_explicit_new_markdown_file() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        save_plan(&root, "plan.md", "final plan").unwrap();
        assert_eq!(
            std::fs::read_to_string(root.join("plan.md")).unwrap(),
            "final plan"
        );
        assert!(save_plan(&root, "plan.md", "overwrite").is_err());
        assert!(save_plan(&root, "../escape.md", "no").is_err());
        assert!(save_plan(&root, "source.ts", "no").is_err());
    }
}
