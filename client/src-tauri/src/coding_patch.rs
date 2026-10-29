//! Reuse the Runtime's patch grammar and matching rules against editor snapshots.
use echo_agent_tools::implementations::codex::apply_patch::{
    derive_new_contents, parse_patch, Hunk,
};
use serde::Serialize;
use std::collections::HashMap;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PatchFile {
    path: String,
    content: Option<String>,
    delete: bool,
    move_to: Option<String>,
}

pub(crate) fn paths(patch: &str) -> Result<Vec<String>, String> {
    let files = preview(patch, None)?;
    Ok(files
        .into_iter()
        .flat_map(|file| std::iter::once(file.path).chain(file.move_to))
        .collect())
}

fn preview(
    patch: &str,
    originals: Option<&HashMap<String, String>>,
) -> Result<Vec<PatchFile>, String> {
    if patch.len() > 2 * 1024 * 1024 {
        return Err("补丁超过 2 MiB 上限".into());
    }
    let parsed = parse_patch(patch).map_err(|error| error.to_string())?;
    if parsed.hunks.is_empty() || parsed.hunks.len() > 50 {
        return Err("补丁需要包含 1～50 个文件".into());
    }
    let mut seen = std::collections::HashSet::new();
    parsed
        .hunks
        .into_iter()
        .map(|hunk| {
            let (path, content, delete, move_to) = match hunk {
                Hunk::AddFile { path, contents } => (path, Some(contents), false, None),
                Hunk::DeleteFile { path } => (path, None, true, None),
                Hunk::UpdateFile {
                    path,
                    move_path,
                    chunks,
                } => {
                    let content = originals
                        .map(|originals| {
                            let original = originals
                                .get(&path.to_string_lossy().to_string())
                                .ok_or("补丁缺少当前文件快照")?;
                            derive_new_contents(original, &path, &chunks)
                                .map_err(|error| error.to_string())
                        })
                        .transpose()?;
                    (path, content, false, move_path)
                }
            };
            let path = path.to_string_lossy().to_string();
            let move_to = move_to.map(|path| path.to_string_lossy().to_string());
            for candidate in std::iter::once(&path).chain(move_to.iter()) {
                if candidate.len() > 8192 || !seen.insert(candidate.clone()) {
                    return Err("补丁路径重复或超出预算".into());
                }
            }
            Ok(PatchFile {
                path,
                content,
                delete,
                move_to,
            })
        })
        .collect()
}

#[tauri::command]
pub fn coding_document_patch_preview(
    patch: String,
    originals: Option<HashMap<String, String>>,
) -> Result<Vec<PatchFile>, String> {
    if originals.as_ref().is_some_and(|values| {
        values.len() > 50 || values.values().map(String::len).sum::<usize>() > 20 * 1024 * 1024
    }) {
        return Err("补丁快照超过预算".into());
    }
    preview(&patch, originals.as_ref())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_patch_grammar_preserves_add_update_delete_and_move() {
        let patch="*** Begin Patch\n*** Add File: new.txt\n+new\n*** Update File: old.txt\n*** Move to: moved.txt\n@@\n-before\n+after\n*** Delete File: delete.txt\n*** End Patch";
        let files = preview(
            patch,
            Some(&HashMap::from([("old.txt".into(), "before\n".into())])),
        )
        .unwrap();
        assert_eq!(files[1].content.as_deref(), Some("after\n"));
        assert_eq!(files[1].move_to.as_deref(), Some("moved.txt"));
        assert!(files[2].delete);
        assert_eq!(paths(patch).unwrap().len(), 4);
    }
}
