//! Range-based playback of app-owned meeting WAV files. No renderer paths.
use super::*;
use tauri::http::{Request, Response, StatusCode};
const MAX_RANGE_BYTES: u64 = 2 * 1024 * 1024;
static TOKENS: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();

#[tauri::command]
pub fn meeting_audio_token(meeting_id: String) -> Result<String, String> {
    validate_id(&meeting_id)?;
    owned_audio_file(&meeting_id)?;
    let mut tokens = TOKENS
        .get_or_init(Default::default)
        .lock()
        .map_err(|_| "播放器暂不可用")?;
    if let Some(token) = tokens.get(&meeting_id) {
        return Ok(token.clone());
    }
    if tokens.len() >= 128 {
        tokens.clear();
    }
    let token = uuid::Uuid::now_v7().to_string();
    tokens.insert(meeting_id, token.clone());
    Ok(token)
}

pub(super) fn owned_audio_file(id: &str) -> Result<File, String> {
    let directory = meeting_dir(id)?
        .canonicalize()
        .map_err(|_| "录音目录不存在")?;
    let root = meetings_root()
        .canonicalize()
        .map_err(|_| "录音目录不存在")?;
    if directory.parent() != Some(root.as_path()) {
        return Err("录音目录无效".into());
    }
    crate::shell_fs::open_regular_file_no_follow(&directory.join("recording.wav"))
}

fn byte_range(value: Option<&str>, length: u64) -> Result<(u64, u64, bool), ()> {
    if length == 0 {
        return Err(());
    }
    let Some(value) = value else {
        return Ok((
            0,
            (length - 1).min(MAX_RANGE_BYTES - 1),
            length > MAX_RANGE_BYTES,
        ));
    };
    let parts = value.strip_prefix("bytes=").ok_or(())?;
    if parts.contains(',') {
        return Err(());
    }
    let (first, last) = parts.split_once('-').ok_or(())?;
    let (start, end) = if first.is_empty() {
        let suffix: u64 = last.parse().map_err(|_| ())?;
        if suffix == 0 {
            return Err(());
        }
        (length.saturating_sub(suffix), length - 1)
    } else {
        let start: u64 = first.parse().map_err(|_| ())?;
        let end: u64 = if last.is_empty() {
            length - 1
        } else {
            last.parse().map_err(|_| ())?
        };
        (start, end.min(length - 1))
    };
    if start >= length || end < start {
        return Err(());
    }
    Ok((
        start,
        end.min(start.saturating_add(MAX_RANGE_BYTES - 1)),
        true,
    ))
}

pub(crate) fn respond(request: Request<Vec<u8>>, main_window: bool) -> Response<Vec<u8>> {
    let failure = |status: StatusCode| {
        Response::builder()
            .status(status)
            .header("Cache-Control", "no-store")
            .body(Vec::new())
            .unwrap()
    };
    if !main_window {
        return failure(StatusCode::FORBIDDEN);
    }
    if request.method() != "GET" && request.method() != "HEAD" {
        return failure(StatusCode::METHOD_NOT_ALLOWED);
    }
    // convertFileSrc percent-encodes complete paths, including the separator.
    let Ok(decoded) = urlencoding::decode(request.uri().path()) else {
        return failure(StatusCode::BAD_REQUEST);
    };
    let mut parts = decoded.trim_start_matches('/').split('/');
    let id = parts.next().unwrap_or("");
    if validate_id(id).is_err() {
        return failure(StatusCode::BAD_REQUEST);
    }
    let token = parts.next().unwrap_or("");
    let authorized = parts.next().is_none()
        && !token.is_empty()
        && TOKENS
            .get_or_init(Default::default)
            .lock()
            .ok()
            .is_some_and(|tokens| tokens.get(id).is_some_and(|saved| saved == token));
    if !authorized {
        return failure(StatusCode::FORBIDDEN);
    }
    let Ok(mut file) = owned_audio_file(id) else {
        return failure(StatusCode::NOT_FOUND);
    };
    let Ok(metadata) = file.metadata() else {
        return failure(StatusCode::NOT_FOUND);
    };
    let length = metadata.len();
    let range = request.headers().get("Range").and_then(|v| v.to_str().ok());
    let Ok((start, end, partial)) = byte_range(range, length) else {
        return Response::builder()
            .status(416)
            .header("Content-Range", format!("bytes */{length}"))
            .body(Vec::new())
            .unwrap();
    };
    let mut bytes = Vec::new();
    if request.method() == "GET"
        && (file.seek(SeekFrom::Start(start)).is_err()
            || file.take(end - start + 1).read_to_end(&mut bytes).is_err())
    {
        return failure(StatusCode::INTERNAL_SERVER_ERROR);
    }
    let mut response = Response::builder()
        .status(if partial { 206 } else { 200 })
        .header("Content-Type", "audio/wav")
        .header("Accept-Ranges", "bytes")
        .header("Content-Length", end - start + 1)
        .header("Cache-Control", "no-store")
        .header("Access-Control-Allow-Origin", "*");
    if partial {
        response = response.header("Content-Range", format!("bytes {start}-{end}/{length}"));
    }
    response.body(bytes).unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ranges_seek_suffix_and_bound_memory_for_multi_hour_recordings() {
        assert_eq!(
            byte_range(Some("bytes=32000-63999"), 100000),
            Ok((32000, 63999, true))
        );
        assert_eq!(
            byte_range(Some("bytes=-100"), 100000),
            Ok((99900, 99999, true))
        );
        assert_eq!(byte_range(Some("bytes=100000-"), 100000), Err(()));
        assert_eq!(byte_range(Some("bytes=0-1,10-20"), 100000), Err(()));
        let (_, end, _) = byte_range(Some("bytes=0-"), 900000000).unwrap();
        assert_eq!(end + 1, MAX_RANGE_BYTES);
    }
    #[test]
    fn protocol_rejects_other_webviews_and_renderer_paths() {
        assert_eq!(
            respond(
                Request::builder()
                    .uri("meeting-audio://localhost/etc/passwd")
                    .body(vec![])
                    .unwrap(),
                true
            )
            .status(),
            400
        );
        assert_eq!(
            respond(
                Request::builder()
                    .uri("meeting-audio://localhost/a")
                    .body(vec![])
                    .unwrap(),
                false
            )
            .status(),
            403
        );
    }
}
