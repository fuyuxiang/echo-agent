//! First-party office export. The worker is bundled with the desktop app; user
//! documents never need a host Python, Node, LibreOffice, or an online converter.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::{Read, Seek};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use tauri::{AppHandle, Manager};
use tokio::io::AsyncWriteExt;

const MAX_MARKDOWN_BYTES: usize = 2 * 1024 * 1024;
const MAX_OUTPUT_BYTES: u64 = 50 * 1024 * 1024;
const EXPORT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

#[derive(Clone, Copy)]
enum OutputPolicy<'a> {
    Replace,
    Create {
        identity: Option<(&'a str, &'a str)>,
    },
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DocumentExportRequest {
    pub(crate) title: String,
    #[serde(default)]
    pub(crate) markdown: String,
    pub(crate) format: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) workbook: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) slides: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) document: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DocumentExportReceipt {
    pub(crate) path: String,
    pub(crate) format: String,
    pub(crate) byte_size: u64,
    pub(crate) sha256: String,
}

fn valid_format(format: &str) -> bool {
    matches!(format, "docx" | "pdf" | "xlsx" | "pptx")
}

pub(crate) fn safe_file_title(title: &str) -> String {
    let mut name: String = title
        .chars()
        .map(|character| match character {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '-',
            value if value.is_control() => ' ',
            value => value,
        })
        .take(80)
        .collect();
    // Leave room for the UUID and extension on filesystems with a 255-byte
    // filename limit, including Chinese and other multibyte titles.
    while name.len() > 200 {
        name.pop();
    }
    let name = name.trim().trim_end_matches(['.', ' ']);
    if name.is_empty() {
        "办公文档".to_string()
    } else if matches!(
        name.to_ascii_uppercase().as_str(),
        "CON"
            | "PRN"
            | "AUX"
            | "NUL"
            | "COM1"
            | "COM2"
            | "COM3"
            | "COM4"
            | "COM5"
            | "COM6"
            | "COM7"
            | "COM8"
            | "COM9"
            | "LPT1"
            | "LPT2"
            | "LPT3"
            | "LPT4"
            | "LPT5"
            | "LPT6"
            | "LPT7"
            | "LPT8"
            | "LPT9"
    ) {
        format!("_{name}")
    } else {
        name.to_string()
    }
}

fn office_resource(app: &AppHandle, relative: &str) -> Result<PathBuf, String> {
    #[cfg(debug_assertions)]
    {
        let staged = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("resources/office")
            .join(relative);
        if staged.is_file() {
            return Ok(staged);
        }
    }
    let bundled = app
        .path()
        .resource_dir()
        .map_err(|error| format!("无法定位文档资源：{error}"))?
        .join("office")
        .join(relative);
    if bundled.is_file() {
        Ok(bundled)
    } else {
        Err(format!("安装包缺少文档处理资源：{relative}"))
    }
}

pub(crate) fn validate_output(path: &Path, format: &str) -> Result<(u64, String), String> {
    let metadata =
        std::fs::symlink_metadata(path).map_err(|error| format!("文档未生成：{error}"))?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err("生成结果不是普通文件".into());
    }
    if metadata.len() == 0 || metadata.len() > MAX_OUTPUT_BYTES {
        return Err("生成文件为空或超过 50 MiB 上限".into());
    }
    let mut file =
        std::fs::File::open(path).map_err(|error| format!("无法检查生成文件：{error}"))?;
    if format == "pdf" {
        let mut head = [0u8; 5];
        file.read_exact(&mut head)
            .map_err(|error| error.to_string())?;
        if &head != b"%PDF-" {
            return Err("生成的 PDF 文件头无效".into());
        }
        file.rewind()
            .map_err(|error| format!("无法计算完整文件摘要：{error}"))?;
    } else {
        let expected = match format {
            "docx" => "word/document.xml",
            "xlsx" => "xl/workbook.xml",
            "pptx" => "ppt/presentation.xml",
            _ => return Err("不支持的文档格式".into()),
        };
        let mut archive = zip::ZipArchive::new(file)
            .map_err(|error| format!("生成的 Office 文件无法打开：{error}"))?;
        if archive.by_name(expected).is_err() {
            return Err(format!("生成的 Office 文件缺少 {expected}"));
        }
        file = std::fs::File::open(path).map_err(|error| error.to_string())?;
    }
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let count = file
            .read(&mut buffer)
            .map_err(|error| format!("计算文件摘要失败：{error}"))?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    Ok((metadata.len(), format!("{:x}", hasher.finalize())))
}

pub(crate) fn install_output(
    source: &Path,
    destination: &Path,
    overwrite: bool,
) -> Result<(), String> {
    let parent = destination.parent().ok_or("无效的保存目录")?;
    let stage = tempfile::Builder::new()
        .prefix(".echo-office-stage-")
        .tempfile_in(parent)
        .map_err(|error| format!("无法在保存目录创建临时文件：{error}"))?;
    std::fs::copy(source, stage.path()).map_err(|error| format!("复制文档失败：{error}"))?;
    stage
        .as_file()
        .sync_all()
        .map_err(|error| format!("无法同步文档临时文件：{error}"))?;
    if !overwrite {
        stage
            .persist_noclobber(destination)
            .map_err(|e| format!("目标文件已存在或无法保存，原文件保持：{}", e.error))?;
        return Ok(());
    }
    let (stage_file, stage_path) = stage.keep().map_err(|error| error.error.to_string())?;
    drop(stage_file);
    let backup = parent.join(format!(".echo-office-backup-{}", uuid::Uuid::now_v7()));
    let had_original = destination.exists();
    if had_original {
        if let Err(error) = std::fs::rename(destination, &backup) {
            let _ = std::fs::remove_file(&stage_path);
            return Err(format!("无法保护原文件：{error}"));
        }
    }
    if let Err(error) = std::fs::rename(&stage_path, destination) {
        if had_original {
            let _ = std::fs::rename(&backup, destination);
        }
        let _ = std::fs::remove_file(&stage_path);
        return Err(format!("保存文档失败：{error}"));
    }
    if had_original {
        let _ = std::fs::remove_file(&backup);
    }
    Ok(())
}

pub(crate) async fn generate(
    app: &AppHandle,
    request: &DocumentExportRequest,
    output: &Path,
) -> Result<(), String> {
    let script = office_resource(app, "worker.mjs")?;
    let font = office_resource(app, "fonts/NotoSansCJKsc-Regular.otf")?;
    let executable = crate::theia::node_executable(app);
    let executable = crate::theia::node_compatible_path(&executable)?;
    let script = crate::theia::node_compatible_path(&script)?;
    let output = crate::theia::node_compatible_path(output)?;
    let font = crate::theia::node_compatible_path(&font)?;
    let mut worker = tokio::process::Command::new(executable);
    worker
        .arg("--max-old-space-size=512")
        .arg(script)
        .arg(output)
        .arg(font)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(windows)]
    worker.creation_flags(crate::process_supervisor::CREATE_NO_WINDOW);
    let mut child = worker
        .spawn()
        .map_err(|error| format!("无法启动内置文档处理程序：{error}"))?;
    let data = serde_json::to_vec(request).map_err(|error| error.to_string())?;
    let mut stdin = child.stdin.take().ok_or("文档处理程序无法接收内容")?;
    stdin
        .write_all(&data)
        .await
        .map_err(|error| error.to_string())?;
    drop(stdin);
    let result = tokio::time::timeout(EXPORT_TIMEOUT, child.wait_with_output())
        .await
        .map_err(|_| "生成文档超时".to_string())?
        .map_err(|error| format!("文档处理程序异常退出：{error}"))?;
    if !result.status.success() {
        let details = String::from_utf8_lossy(&result.stderr);
        return Err(format!(
            "生成文档失败：{}",
            details.chars().take(400).collect::<String>()
        ));
    }
    Ok(())
}

pub async fn export(
    app: &AppHandle,
    request: DocumentExportRequest,
) -> Result<Option<DocumentExportReceipt>, String> {
    export_for_session(app, request, None).await
}

pub(crate) async fn export_for_session(
    app: &AppHandle,
    request: DocumentExportRequest,
    session_id: Option<&str>,
) -> Result<Option<DocumentExportReceipt>, String> {
    validate_request(&request)?;
    let filename = format!("{}.{}", safe_file_title(&request.title), request.format);
    let Some(mut destination) =
        crate::meeting_minutes::choose_save_path(app, "导出办公文档", &filename, &request.format)
            .await?
    else {
        return Ok(None);
    };
    if destination.extension().is_none() {
        destination.set_extension(&request.format);
    }
    export_to_path_for_session(
        app,
        request,
        &destination,
        session_id,
        OutputPolicy::Replace,
    )
    .await
    .map(Some)
}

pub(crate) fn validate_request(request: &DocumentExportRequest) -> Result<(), String> {
    if !valid_format(&request.format) {
        return Err("不支持的导出格式".into());
    }
    let structured =
        request.workbook.is_some() || request.slides.is_some() || request.document.is_some();
    if let Some(document) = &request.document {
        if !matches!(request.format.as_str(), "docx" | "pdf") {
            return Err("可编辑文档仅支持 Word 或 PDF 导出".into());
        }
        crate::document_store::validate(document)?;
    }
    if (request.markdown.trim().is_empty() && !structured)
        || serde_json::to_vec(request)
            .map_err(|e| e.to_string())?
            .len()
            > MAX_MARKDOWN_BYTES
    {
        return Err("文档内容为空或超过 2 MiB 上限".into());
    }
    if (request.workbook.is_some() && request.format != "xlsx")
        || (request.slides.is_some() && request.format != "pptx")
        || (structured && !request.markdown.trim().is_empty())
    {
        return Err(
            "workbook 仅用于 xlsx，slides 仅用于 pptx；结构化内容与 markdown 只能选择一种".into(),
        );
    }
    if request.title.chars().count() > 160 {
        return Err("文档标题过长".into());
    }
    Ok(())
}

/// Agent creation never overwrites a destination. Identical retries are
/// recognized by request digest and verified receipt in office_mcp.
pub(crate) async fn create_for_session(
    app: &AppHandle,
    request: DocumentExportRequest,
    destination: &Path,
    session: &str,
    identity: Option<(&str, &str)>,
) -> Result<DocumentExportReceipt, String> {
    export_to_path_for_session(
        app,
        request,
        destination,
        Some(session),
        OutputPolicy::Create { identity },
    )
    .await
}

async fn export_to_path_for_session(
    app: &AppHandle,
    request: DocumentExportRequest,
    destination: &Path,
    session_id: Option<&str>,
    policy: OutputPolicy<'_>,
) -> Result<DocumentExportReceipt, String> {
    validate_request(&request)?;
    if !destination
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case(&request.format))
    {
        return Err("保存路径的扩展名与文档格式不一致".into());
    }
    let canonical_destination;
    let destination = if session_id.is_some() {
        let parent = destination.parent().ok_or("无效的保存目录")?;
        let name = destination.file_name().ok_or("无效的保存文件名")?;
        canonical_destination = dunce::canonicalize(parent)
            .map_err(|error| format!("无法确认保存目录：{error}"))?
            .join(name);
        canonical_destination.as_path()
    } else {
        destination
    };
    let document_lock = crate::document_workspace::document_lock(destination)?;
    let _document_guard = document_lock.lock().await;
    let temp = match policy {
        OutputPolicy::Create { .. } => tempfile::Builder::new()
            .prefix(".echo-office-")
            .tempdir_in(destination.parent().ok_or("无效的保存目录")?),
        OutputPolicy::Replace => tempfile::tempdir(),
    }
    .map_err(|error| format!("无法创建文档临时目录：{error}"))?;
    let output = temp.path().join(format!("output.{}", request.format));
    generate(app, &request, &output).await?;
    let (byte_size, sha256) = validate_output(&output, &request.format)?;
    let receipt = DocumentExportReceipt {
        path: destination.to_string_lossy().into_owned(),
        format: request.format,
        byte_size,
        sha256,
    };
    // Persist the expected verified receipt before publishing the output. If
    // the final index promotion fails, history can recover the staged receipt
    // by verifying its hash against the installed file after restart.
    if let Some(session) = session_id {
        crate::session_artifacts::stage(session, receipt.clone(), &request.title)?;
        if let OutputPolicy::Create {
            identity: Some((key, digest)),
        } = policy
        {
            crate::session_artifacts::identify_creation(session, &receipt.path, key, digest)?;
        }
    }
    install_output(
        &output,
        destination,
        matches!(policy, OutputPolicy::Replace),
    )?;
    if let Some(session) = session_id {
        if let Err(error) =
            crate::session_artifacts::record(session, receipt.clone(), &request.title)
        {
            tracing::warn!(%error, "export receipt staged; history will verify published output");
        }
    }
    if receipt.format == "docx" {
        let source = output.with_file_name("output.docx.document.json");
        let source = crate::shell_fs::read_regular_file_bounded(&source, 2 * 1024 * 1024)?;
        let model = serde_json::from_slice(&source)
            .map_err(|error| format!("文档已生成，但编辑稿无法读取：{error}"))?;
        if let Some(session) = session_id {
            crate::document_workspace::register_generated(
                session,
                destination,
                &request.title,
                model,
                &receipt.sha256,
                receipt.byte_size,
            )?;
        }
    }
    Ok(receipt)
}

#[tauri::command]
pub async fn document_export(
    app: AppHandle,
    access: tauri::State<'_, crate::shell_fs::FilesystemAccess>,
    state: tauri::State<'_, crate::commands::AppState>,
    session_id: Option<String>,
    request: DocumentExportRequest,
) -> Result<Option<DocumentExportReceipt>, String> {
    if let Some(session) = &session_id {
        state.session_workspace(session)?;
    }
    let result = export_for_session(&app, request, session_id.as_deref()).await?;
    if let Some(receipt) = &result {
        access
            .authorize_file(Path::new(&receipt.path))
            .map_err(|error| {
                format!(
                    "文件已保存到 {}，成果回执已保留；打开前需要重新授权：{error}",
                    receipt.path
                )
            })?;
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_filenames_fit_multibyte_filesystem_limits() {
        for title in ["季度经营分析".repeat(30), "😀".repeat(160), "a".repeat(160)] {
            let title = safe_file_title(&title);
            assert!(!title.is_empty());
            assert!(format!("{title}-{}.pptx", uuid::Uuid::now_v7().simple()).len() <= 255);
        }
    }

    #[test]
    fn creation_never_overwrites_an_existing_output_and_cleans_staging_files() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("new.pdf");
        let destination = dir.path().join("report.pdf");
        std::fs::write(&source, b"new document").unwrap();
        std::fs::write(&destination, b"original document").unwrap();
        assert!(install_output(&source, &destination, false).is_err());
        assert_eq!(std::fs::read(&destination).unwrap(), b"original document");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 2);
        std::fs::remove_file(&destination).unwrap();
        install_output(&source, &destination, false).unwrap();
        assert_eq!(std::fs::read(&destination).unwrap(), b"new document");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 2);
    }

    #[test]
    fn pdf_receipt_hash_covers_the_header_and_entire_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("中文成果.pdf");
        let bytes = b"%PDF-1.7\n1 0 obj\n<< /Type /Catalog >>\nendobj\n%%EOF\n";
        std::fs::write(&path, bytes).unwrap();
        let (size, digest) = validate_output(&path, "pdf").unwrap();
        assert_eq!(size, bytes.len() as u64);
        assert_eq!(digest, format!("{:x}", Sha256::digest(bytes)));
    }

    #[test]
    fn pdf_receipt_hash_covers_streamed_utf8_content_across_buffer_boundaries() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("多缓冲区.pdf");
        for length in [65_531, 65_536, 131_073] {
            let mut bytes = b"%PDF-1.7\n".to_vec();
            bytes.extend(std::iter::repeat_n(b'x', length));
            bytes.extend_from_slice("中文内容\n%%EOF\n".as_bytes());
            std::fs::write(&path, &bytes).unwrap();
            let (size, digest) = validate_output(&path, "pdf").unwrap();
            assert_eq!(size, bytes.len() as u64);
            assert_eq!(digest, format!("{:x}", Sha256::digest(&bytes)));
        }
    }

    #[test]
    fn pdf_receipt_still_rejects_empty_truncated_and_invalid_headers() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("错误文件.pdf");
        for bytes in [b"".as_slice(), b"%PDF".as_slice(), b"wrong PDF".as_slice()] {
            std::fs::write(&path, bytes).unwrap();
            assert!(validate_output(&path, "pdf").is_err());
        }
    }
}
