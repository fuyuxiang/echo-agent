use super::ArtifactChange;
use std::io::{Read, Write};
use std::path::Path;

const MAX_HTML_BYTES: usize = 2 * 1024 * 1024;
const HISTORY_LIMIT: usize = 6;

#[derive(Default)]
pub(super) struct History {
    undo: Vec<Revision>,
    redo: Vec<Revision>,
}

struct Revision {
    before: String,
    after: String,
}

impl History {
    pub(super) fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub(super) fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    pub(super) fn push(&mut self, before: String, after: String) {
        self.redo.clear();
        self.undo.push(Revision { before, after });
        if self.undo.len() > HISTORY_LIMIT {
            self.undo.remove(0);
        }
    }

    pub(super) fn apply(&mut self, path: &Path, redo: bool) -> Result<(), String> {
        let (source, destination) = if redo {
            (&mut self.redo, &mut self.undo)
        } else {
            (&mut self.undo, &mut self.redo)
        };
        let item = source.last().ok_or("没有可恢复的 HTML 修改")?;
        let (expected, next) = if redo {
            (&item.before, &item.after)
        } else {
            (&item.after, &item.before)
        };
        write(path, expected, next)?;
        if let Some(item) = source.pop() {
            destination.push(item);
        }
        Ok(())
    }
}

pub(super) fn validate(change: &ArtifactChange) -> Result<(), String> {
    if change
        .text
        .as_ref()
        .is_some_and(|text| text.len() > 16 * 1024)
        || change.color.as_ref().is_some_and(|color| {
            color.len() != 7
                || !color.starts_with('#')
                || !color.as_bytes()[1..].iter().all(u8::is_ascii_hexdigit)
        })
        || change
            .font_size
            .is_some_and(|size| !(8..=240).contains(&size))
        || change.image.as_ref().is_some_and(|image| {
            image.is_empty()
                || image.len() > 512
                || image.starts_with('/')
                || image.contains([':', '\\', '?', '#'])
                || image
                    .split('/')
                    .any(|segment| segment.starts_with('.') || segment.is_empty())
        })
    {
        return Err("文字、颜色、字号或图片路径超出可编辑范围".into());
    }
    Ok(())
}

pub(super) fn read(path: &Path) -> Result<String, String> {
    let error = |error| format!("读取 HTML 产物失败：{error}");
    let metadata = path.symlink_metadata().map_err(error)?;
    if !metadata.is_file() || metadata.len() > MAX_HTML_BYTES as u64 {
        return Err("可视化编辑支持 2 MB 以内的普通 HTML 文件".into());
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    std::fs::File::open(path)
        .map_err(error)?
        .take(MAX_HTML_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(error)?;
    if bytes.len() > MAX_HTML_BYTES {
        return Err("HTML 产物超过可视化编辑大小上限".into());
    }
    String::from_utf8(bytes).map_err(|_| "可视化编辑需要 UTF-8 格式的 HTML".into())
}

pub(super) fn write(path: &Path, expected: &str, next: &str) -> Result<(), String> {
    if next.len() > MAX_HTML_BYTES {
        return Err("修改后的 HTML 超过 2 MB".into());
    }
    let error = |error| format!("保存 HTML 产物失败：{error}");
    let parent = path.parent().ok_or("HTML 文件没有上级目录")?;
    let permissions = path.metadata().map_err(error)?.permissions();
    let mut staging = tempfile::NamedTempFile::new_in(parent).map_err(error)?;
    staging.write_all(next.as_bytes()).map_err(error)?;
    staging
        .as_file()
        .set_permissions(permissions)
        .map_err(error)?;
    staging.as_file().sync_all().map_err(error)?;
    if read(path)? != expected {
        return Err("HTML 文件已被其他操作更新，请刷新后再编辑".into());
    }
    let staging = staging.into_temp_path();
    crate::paths::replace_file_atomically(staging.as_ref(), path)
}
