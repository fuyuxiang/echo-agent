//! Prepare an already-open OS file panel using native UI only. No app recipes,
//! filesystem writes, dialog opening, final submission, or action replay.
use super::*;

pub struct Dialog<E> {
    pub kind: &'static str,
    pub root: E,
    pub path: Option<E>,
    pub filename: Option<E>,
}

fn guard<B: Native>(
    backend: &B,
    window: &B::Element,
    token: &CancellationToken,
) -> Result<(), String> {
    if token.is_cancelled() {
        return Err("cancelled: file panel preparation stopped; inspect current state".into());
    }
    backend.check_session()?;
    backend.validate(window, None)
}
fn wait<B: Native>(
    backend: &B,
    window: &B::Element,
    kind: &str,
    token: &CancellationToken,
) -> Result<Dialog<B::Element>, String> {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        guard(backend, window, token)?;
        match backend.file_dialog(window) {
            Ok(Some(panel)) if panel.kind == kind => return Ok(panel),
            Err(e) if !e.starts_with("native_timeout") => return Err(e),
            _ => {}
        }
        if Instant::now() >= deadline {
            return Err("panel_transition_unknown: expected file panel not observed; do not reopen or repeat input".into());
        }
        std::thread::sleep(Duration::from_millis(80));
    }
}
fn replace<B: Native>(
    backend: &B,
    window: &B::Element,
    element: &B::Element,
    text: &str,
    token: &CancellationToken,
) -> Result<(), String> {
    guard(backend, window, token)?;
    backend.validate(element, Some(window))?;
    backend.replace_text(element, window, text, token)?;
    let deadline = Instant::now() + Duration::from_millis(500);
    loop {
        guard(backend, window, token)?;
        let (actual, more) = backend.read(element, 0, 8193)?;
        if !more && actual == text {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(
                "input_unverified: path/filename readback differs; no confirmation dispatched"
                    .into(),
            );
        }
        std::thread::sleep(Duration::from_millis(40));
    }
}

pub(super) fn prepare<B: Native>(
    backend: &B,
    window: &B::Element,
    path: &str,
    token: &CancellationToken,
) -> Result<Value, String> {
    let target = std::path::Path::new(path);
    if path.len() > 4096
        || path.chars().any(char::is_control)
        || !target.is_absolute()
        || target.components().any(|c| {
            matches!(
                c,
                std::path::Component::ParentDir | std::path::Component::CurDir
            )
        })
    {
        return Err("invalid_argument: provide a literal absolute file path without control characters or dot components".into());
    }
    guard(backend, window, token)?;
    let original = backend.file_dialog(window)?.ok_or(
        "unsupported_panel: no supported OS file panel is open; this tool never opens one",
    )?;
    if !["save", "open"].contains(&original.kind) {
        return Err(
            "unsupported_panel: start from the Open/Save panel, not its nested path editor".into(),
        );
    }
    #[cfg(target_os = "windows")]
    {
        let field = original
            .filename
            .as_ref()
            .ok_or("unsupported_panel: filename field unavailable")?;
        replace(backend, window, field, path, token)?;
        return Ok(
            json!({"prepared":true,"verified":false,"status":"prepared_not_submitted","panelKind":original.kind,"requestedPath":path,"pathInputVerified":true,"finalActionDispatched":false,"replaySafe":false,"guidance":"Absolute filename was read back in the standard Windows file panel. Invoke its final Open/Save button once, then verify the actual result."}),
        );
    }
    #[cfg(not(target_os = "windows"))]
    {
        let filename = target
            .file_name()
            .and_then(|s| s.to_str())
            .filter(|s| !s.is_empty())
            .ok_or("invalid_argument: file basename required")?;
        let navigation = if original.kind == "save" {
            // A trailing separator asks the system to navigate a directory, rather
            // than ambiguously treating the last component as a file candidate.
            target
                .parent()
                .unwrap_or_else(|| std::path::Path::new(std::path::MAIN_SEPARATOR_STR))
                .join("")
                .display()
                .to_string()
        } else {
            path.to_owned()
        };
        if !backend.prepare_input_window(window)? {
            return Err("focus_changed: file panel did not activate".into());
        }
        guard(backend, window, token)?;
        backend.validate(&original.root, Some(window))?;
        backend.key_cancellable(window, "g", &["cmd".into(), "shift".into()], token)?;
        let goto = wait(backend, window, "goto", token)?;
        let field = goto
            .path
            .ok_or("unsupported_panel: system path input unavailable")?;
        replace(backend, window, &field, &navigation, token)?;
        guard(backend, window, token)?;
        backend.validate(&goto.root, Some(window))?;
        backend.key_cancellable(window, "return", &[], token)?;
        let panel = wait(backend, window, original.kind, token)?;
        if !backend.same_element(&original.root, &panel.root) {
            return Err(
                "target_changed: original file panel was replaced; no final action dispatched"
                    .into(),
            );
        }
        if panel.kind == "save" {
            let field = panel
                .filename
                .ok_or("unsupported_panel: save filename field unavailable")?;
            replace(backend, window, &field, filename, token)?;
        }
        Ok(
            json!({"prepared":true,"verified":false,"status":"prepared_not_submitted","panelKind":panel.kind,"requestedPath":path,"pathInputVerified":true,"panelReturned":true,"finalActionDispatched":false,"replaySafe":false,"guidance":"Path entry and panel return were observed, not final save/selection. Use the returned current OK button once, then verify actual output/selection. Do not reopen the panel or repeat navigation. Overwrite/error dialogs require a new decision from their actual state."}),
        )
    }
}
