//! Native objects live exclusively on a session worker (COM MTA on Windows).
//! No script interpreter, model-provided PID, pointer, or persistent index path.
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "windows")]
mod windows;
#[cfg(target_os = "macos")]
use macos::Backend as Platform;
#[cfg(target_os = "windows")]
use windows::Backend as Platform;
#[cfg(any(target_os = "windows", test))]
mod app_names;
#[cfg(target_os = "windows")]
pub(super) fn windows_application_names(path: &str) -> (String, Vec<String>) {
    app_names::windows_names(path, &windows::application_descriptions(path))
}
mod context;
mod helper;
pub fn run_helper() -> Result<(), String> {
    helper::main()
}
mod file_dialog;
#[cfg(target_os = "macos")]
mod macos_screencapture;
#[cfg(target_os = "macos")]
mod macos_video_capture;
#[cfg(target_os = "macos")]
mod macos_vision;
mod paging;
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
mod unsupported;
mod visual_text;
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
use unsupported::Backend as Platform;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::{HashMap, VecDeque},
    sync::mpsc,
    time::{Duration, Instant},
};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

const REF_AGE: Duration = Duration::from_secs(120);
const BUDGET: Duration = Duration::from_secs(3);
const MAX_REFS: usize = 4096;

// Retry observations only. The physical action remains single-dispatch, and
// its outcome is preserved even if both reads fail during a UI transition.
fn retry_observation<T>(
    token: &CancellationToken,
    mut read: impl FnMut() -> Result<T, String>,
) -> Result<T, String> {
    let first = read();
    if first
        .as_ref()
        .err()
        .is_some_and(|e| e.starts_with("native_timeout"))
        && !token.is_cancelled()
    {
        std::thread::sleep(Duration::from_millis(160));
        if token.is_cancelled() {
            return Err("cancelled: observation stopped".into());
        }
        read()
    } else {
        first
    }
}

#[cfg(target_os = "macos")]
pub(crate) fn macos_foreground_pid() -> Result<Option<i32>, String> {
    macos::foreground_pid()
}

#[cfg(target_os = "macos")]
pub(crate) fn macos_restore_foreground(pid: i32) -> Result<bool, String> {
    let backend = macos::Backend::new()?;
    let app = backend
        .apps()?
        .items
        .into_iter()
        .find(|(e, _)| macos::element_pid(e) == pid)
        .ok_or("application_exited: original screenshot application unavailable")?
        .0;
    backend.activate(&app)
}

#[cfg(target_os = "macos")]
pub(super) fn check_desktop_session() -> Result<(), String> {
    macos::desktop_available()
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Node {
    pub role: String,
    pub application: String,
    pub name: String,
    pub identifier: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aliases: Vec<String>,
    pub value: Option<String>,
    pub enabled: bool,
    pub visible: Option<bool>,
    pub focused: bool,
    pub selected: Option<bool>,
    pub expanded: Option<bool>,
    pub protected: bool,
    pub writable: bool,
    pub bounds: Option<[f64; 4]>,
    pub actions: Vec<String>,
    pub text_truncated: bool,
    pub metadata_truncated: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub observation_errors: Vec<String>,
}

pub struct Listing<E> {
    pub items: Vec<(E, Node)>,
    pub issues: Vec<(String, String)>,
}
#[cfg(target_os = "macos")]
impl<E> Listing<E> {
    fn complete(items: Vec<(E, Node)>) -> Self {
        Self {
            items,
            issues: vec![],
        }
    }
}

/// Canonical click accepts exactly one target representation. Native refs never
/// consume screenshot coordinates; existing coordinate clients remain valid.
pub fn native_click_ref(args: &Value) -> Result<Option<&str>, String> {
    let Some(value) = args.get("ref").filter(|v| !v.is_null()) else {
        return Ok(None);
    };
    let reference = value
        .as_str()
        .filter(|s| !s.is_empty())
        .ok_or("invalid_argument: ref must be nonempty text")?;
    if ["frameId", "x", "y"]
        .iter()
        .any(|key| args.get(*key).is_some_and(|v| !v.is_null()))
    {
        return Err(
            "invalid_argument: choose native ref OR screenshot frameId/x/y, not both".into(),
        );
    }
    if args
        .get("button")
        .and_then(Value::as_str)
        .is_some_and(|button| button != "left")
    {
        return Err("unsupported_action: native ref clicks currently use left button; use screenshot for other buttons".into());
    }
    Ok(Some(reference))
}

/// Some providers serialize integer tool options as decimal strings. Normalize
/// only explicitly named integer options, before their existing range checks.
/// Text, coordinates, references and authorization are never interpreted here.
pub fn normalize_integer_options(method: &str, args: &Value) -> Result<Value, String> {
    let keys: &[&str] = match method {
        "computer_click" | "computer_click_control" => &["clickCount"],
        "computer_set_value" => &["trailingNewlines"],
        "computer_verify_file" => &["expectedTrailingNewlines"],
        "computer_snapshot" => &["maxDepth", "maxNodes", "maxText"],
        "computer_find" | "computer_wait_for" => &["maxDepth", "maxNodes"],
        "computer_read" => &["offset", "limit"],
        _ => &[],
    };
    let mut normalized = args.clone();
    for key in keys {
        if let Some(Value::String(value)) = args.get(*key) {
            if value.is_empty() || value.len() > 20 || !value.bytes().all(|c| c.is_ascii_digit()) {
                return Err(format!(
                    "invalid_argument: {key} must be an unsigned integer"
                ));
            }
            let integer = value
                .parse::<u64>()
                .map_err(|_| format!("invalid_argument: {key} exceeds integer range"))?;
            normalized[*key] = json!(integer);
        }
    }
    Ok(normalized)
}

/// Capability is per action, not a promise that every operation is background.
pub fn foreground_required(method: &str, args: &Value) -> bool {
    match method {
        "computer_activate_window" => true,
        "computer_open_app" | "_open_app" => {
            args["foreground"] == true || !cfg!(target_os = "macos")
        }
        "computer_key_window" | "computer_click_control" | "computer_file_dialog" => {
            !cfg!(target_os = "macos")
        }
        "computer_set_value" => args["inputMethod"] == "keyboard" && !cfg!(target_os = "macos"),
        "computer_invoke" => args["action"] == "focus" && !cfg!(target_os = "macos"),
        _ => false,
    }
}

pub trait Native: Sized {
    type Element: Clone;
    fn new() -> Result<Self, String>;
    fn process_id(&self, _element: &Self::Element) -> Option<u32> {
        None
    }
    fn process_ids(&self, element: &Self::Element) -> Vec<u32> {
        self.process_id(element).into_iter().collect()
    }
    fn application_bundle_path(&self, _element: &Self::Element) -> Option<std::path::PathBuf> {
        None
    }
    fn prepare_input_window(&self, element: &Self::Element) -> Result<bool, String> {
        self.activate(element)
    }
    fn capture_window(&self, _element: &Self::Element) -> Result<Value, String> {
        Err(
            "unsupported_action: background window capture unavailable; use native observation"
                .into(),
        )
    }
    fn recognize_text(&self, _png: &[u8]) -> Result<Vec<visual_text::Line>, String> {
        Err(
            "unsupported_action: system visual text recognition is unavailable on this platform"
                .into(),
        )
    }
    fn focused_target(&self) -> Result<(Self::Element, Self::Element), String> {
        Err("unsupported_action: trustworthy input focus unavailable on this platform".into())
    }
    fn foreground_window(&self) -> Result<Self::Element, String> {
        self.focused_target().map(|(window, _)| window)
    }
    fn keyboard_focus(&self, window: &Self::Element) -> Result<Option<Self::Element>, String> {
        let (current, focus) = self.focused_target()?;
        if !self.same_element(window, &current) {
            return Err("focus_changed: foreground window changed".into());
        }
        Ok(Some(focus))
    }
    fn hit_target(
        &self,
        _window: &Self::Element,
        _x: f64,
        _y: f64,
    ) -> Result<Self::Element, String> {
        Err("unsupported_action: native hit testing unavailable".into())
    }
    fn check_session(&self) -> Result<(), String> {
        Ok(())
    }
    fn open_app(&self, _path: &str) -> Result<bool, String> {
        Err("unsupported_platform: application launch unavailable".into())
    }
    fn reopen_app(&self, path: &str) -> Result<bool, String> {
        self.open_app(path)
    }
    fn apps(&self) -> Result<Listing<Self::Element>, String>;
    fn windows(&self, app: Option<&Self::Element>) -> Result<Listing<Self::Element>, String>;
    fn inspect(&self, element: &Self::Element) -> Result<Node, String>;
    fn same_element(&self, _a: &Self::Element, _b: &Self::Element) -> bool {
        false
    }
    fn application_element(&self, _e: &Self::Element) -> Result<Option<Self::Element>, String> {
        Ok(None)
    }
    fn commands(&self, _e: &Self::Element) -> Result<Option<Vec<Value>>, String> {
        Ok(None)
    }
    fn application_window(&self, _app: &Self::Element) -> Result<Option<Self::Element>, String> {
        Ok(None)
    }
    fn observation_root(&self, window: &Self::Element) -> Result<Self::Element, String> {
        Ok(window.clone())
    }
    fn file_dialog(
        &self,
        _window: &Self::Element,
    ) -> Result<Option<file_dialog::Dialog<Self::Element>>, String> {
        Ok(None)
    }
    fn application_identifier(&self, _element: &Self::Element) -> Option<String> {
        None
    }
    fn document_url(&self, _element: &Self::Element) -> Result<Option<String>, String> {
        Ok(None)
    }
    fn children(
        &self,
        element: &Self::Element,
        limit: usize,
    ) -> Result<(Vec<Self::Element>, bool), String>;
    fn validate(
        &self,
        element: &Self::Element,
        window: Option<&Self::Element>,
    ) -> Result<(), String>;
    fn activate(&self, element: &Self::Element) -> Result<bool, String>;
    fn key(
        &self,
        _element: &Self::Element,
        _key: &str,
        _modifiers: &[String],
    ) -> Result<(), String> {
        Err("unsupported_action: window-bound keyboard input is unavailable".into())
    }
    fn key_cancellable(
        &self,
        element: &Self::Element,
        key: &str,
        modifiers: &[String],
        token: &CancellationToken,
    ) -> Result<(), String> {
        if token.is_cancelled() {
            return Err("cancelled: no key dispatched".into());
        }
        self.key(element, key, modifiers)
    }
    fn click_control(
        &self,
        _element: &Self::Element,
        _window: &Self::Element,
        _count: u32,
        _token: &CancellationToken,
    ) -> Result<(), String> {
        Err("unsupported_action: hit-tested control clicking unavailable; use supported native action or fresh screenshot".into())
    }
    fn invoke(&self, element: &Self::Element, action: &str) -> Result<(), String>;
    fn set_value(&self, element: &Self::Element, value: &str) -> Result<(), String>;
    fn verify_text_focus(
        &self,
        _element: &Self::Element,
        _window: &Self::Element,
    ) -> Result<(), String> {
        Err("unsupported_action: focused Unicode input unavailable".into())
    }
    fn emit_text(&self, _element: &Self::Element, text: &str) -> Result<(), String> {
        super::computer::text_after_native_focus(text)
    }
    fn select_all_text(
        &self,
        _element: &Self::Element,
        window: &Self::Element,
        token: &CancellationToken,
    ) -> Result<(), String> {
        self.key_cancellable(
            window,
            "a",
            &[if cfg!(target_os = "macos") {
                "cmd"
            } else {
                "ctrl"
            }
            .into()],
            token,
        )
    }
    fn replace_text(
        &self,
        element: &Self::Element,
        window: &Self::Element,
        value: &str,
        token: &CancellationToken,
    ) -> Result<(), String> {
        if !self.prepare_input_window(window)? {
            return Err(
                "focus_changed: original text window did not activate; no input dispatched".into(),
            );
        }
        if token.is_cancelled() {
            return Err("cancelled: no text dispatched".into());
        }
        self.invoke(element, "focus")?;
        std::thread::sleep(Duration::from_millis(30));
        let verify = || {
            if token.is_cancelled() {
                return Err("cancelled: text replacement interrupted; observe before retry".into());
            }
            self.check_session()?;
            self.verify_text_focus(element, window)
        };
        verify()?;
        self.select_all_text(element, window, token)?;
        verify()?;
        self.key_cancellable(window, "backspace", &[], token)?;
        for chunk in unicode_input_chunks(value) {
            verify()?;
            self.emit_text(element, chunk)?;
        }
        Ok(())
    }
    fn read(
        &self,
        element: &Self::Element,
        offset: usize,
        limit: usize,
    ) -> Result<(String, bool), String>;
}

fn unicode_input_chunks(value: &str) -> Vec<&str> {
    let mut chunks = vec![];
    let (mut start, mut units) = (0, 0);
    for (offset, character) in value.char_indices() {
        if units + character.len_utf16() > 20 {
            chunks.push(&value[start..offset]);
            start = offset;
            units = 0;
        }
        units += character.len_utf16();
    }
    if start < value.len() {
        chunks.push(&value[start..]);
    }
    chunks
}

#[derive(Clone, Debug)]
struct Reference<E> {
    element: E,
    window: Option<E>,
    window_ref: Option<String>,
    kind: &'static str,
    observed: Node,
    observed_document: Option<String>,
    approved_window: Option<Node>,
    created: Instant,
}
struct Context<B: Native> {
    backend: B,
    refs: HashMap<String, Reference<B::Element>>,
    approvals: HashMap<String, Reference<B::Element>>,
    pages: HashMap<String, paging::Page>,
}

impl<B: Native> Drop for Context<B> {
    fn drop(&mut self) {
        self.approvals.clear();
        self.pages.clear();
        self.refs.clear();
    }
}

fn string<'a>(v: &'a Value, key: &str) -> Result<&'a str, String> {
    v.get(key)
        .and_then(Value::as_str)
        .filter(|v| !v.is_empty())
        .ok_or_else(|| format!("invalid_argument: {key} is required"))
}
fn number(v: &Value, key: &str, default: usize, max: usize) -> usize {
    v.get(key)
        .and_then(Value::as_u64)
        .unwrap_or(default as u64)
        .clamp(1, max as u64) as usize
}
pub(super) fn text_input(args: &Value) -> Result<(&str, bool), String> {
    let value = args
        .get("value")
        .and_then(Value::as_str)
        .ok_or("invalid_argument: value must be a string")?;
    if value.len() > 64000 {
        return Err("invalid_argument: value exceeds 64KB".into());
    }
    let keyboard = match args.get("inputMethod") {
        None => false,
        Some(v) if v == "value" => false,
        Some(v) if v == "keyboard" => true,
        _ => return Err("invalid_argument: inputMethod must be value or keyboard".into()),
    };
    if keyboard && value.len() > 8000 {
        return Err(
            "invalid_argument: keyboard input exceeds 8KB; use native value input for larger text"
                .into(),
        );
    }
    Ok((value, keyboard))
}
pub(super) fn window_key_input(args: &Value) -> Result<(&str, Vec<String>), String> {
    let key_chord = args.get("shortcut").is_none()
        && args.get("modifiers").is_none()
        && args
            .get("key")
            .and_then(Value::as_str)
            .is_some_and(|s| s.contains('+'));
    let (key, input_modifiers) = if args.get("shortcut").is_some() || key_chord {
        if !key_chord && (args.get("key").is_some() || args.get("modifiers").is_some()) {
            return Err("invalid_argument: shortcut cannot be combined with key/modifiers".into());
        }
        let shortcut = string(args, if key_chord { "key" } else { "shortcut" })?;
        if shortcut.len() > 80 {
            return Err("invalid_argument: shortcut exceeds 80 bytes".into());
        }
        let mut parts: Vec<_> = shortcut.split('+').map(str::trim).collect();
        if parts.len() > 5 || parts.iter().any(|p| p.is_empty()) {
            return Err("invalid_argument: use shortcut=\"cmd+n\" or \"cmd+shift+s\"".into());
        }
        let key = parts.pop().unwrap();
        (
            key,
            parts
                .into_iter()
                .map(str::to_ascii_lowercase)
                .collect::<Vec<_>>(),
        )
    } else {
        let key = string(args, "key")?;
        let input = if let Some(input) = args.get("modifiers") {
            // Some providers encode XML-style arrays as {"item":[...]}. Accept
            // only this exact wrapper; all modifier/key validation still runs.
            let mut input = input;
            for _ in 0..2 {
                if let Some(item) = input
                    .as_object()
                    .filter(|o| o.len() == 1)
                    .and_then(|o| o.get("item"))
                {
                    input = item;
                } else {
                    break;
                }
            }
            if let Some(array) = input.as_array() {
                array
                    .iter()
                    .map(|v| {
                        v.as_str()
                            .map(str::to_owned)
                            .ok_or("invalid_argument: modifiers must be strings")
                    })
                    .collect::<Result<Vec<_>, _>>()?
            } else if let Some(single) = input.as_str() {
                vec![single.to_owned()]
            } else {
                return Err("invalid_argument: modifiers must contain known modifier strings; alternatively use shortcut=\"cmd+n\"".into());
            }
        } else {
            vec![]
        };
        (key, input)
    };
    if key.len() > 64 || (key.chars().count() > 1 && key.contains('+')) {
        return Err("invalid_argument: key must be bare; use shortcut=\"cmd+n\" or key=\"n\", modifiers=[\"cmd\"]".into());
    }
    let mut modifiers = Vec::new();
    if input_modifiers.len() > 4 {
        return Err("invalid_argument: at most four modifiers".into());
    }
    for modifier in input_modifiers {
        let canonical = match modifier.as_str() {
            "shift" => "shift",
            "ctrl" | "control" => "ctrl",
            "alt" | "option" => "alt",
            "cmd" | "command" | "meta" => "cmd",
            _ => return Err("invalid_argument: unsupported modifier".into()),
        };
        if !modifiers.iter().any(|v| v == canonical) {
            modifiers.push(canonical.to_owned());
        }
    }
    Ok((key, modifiers))
}
fn identity() -> String {
    uuid::Uuid::now_v7().to_string()
}
fn redact(mut node: Node) -> Node {
    if node.protected {
        node.value = None;
        node.name = "[protected]".into();
        node.writable = false;
        node.actions.clear();
    }
    node
}
fn matches(node: &Node, args: &Value) -> bool {
    ["role", "name", "identifier"].iter().all(|key| {
        let actual = match *key {
            "role" => &node.role,
            "name" => &node.name,
            _ => &node.identifier,
        };
        args.get(*key)
            .and_then(Value::as_str)
            .is_none_or(|expected| {
                actual == expected
                    || (*key == "name"
                        && node.role == "application"
                        && (actual.to_lowercase() == expected.to_lowercase()
                            || node
                                .aliases
                                .iter()
                                .any(|a| a.to_lowercase() == expected.to_lowercase())))
            })
    })
}

impl<B: Native> Context<B> {
    fn insert(&mut self, reference: Reference<B::Element>) -> Result<Value, String> {
        self.refs
            .retain(|_, r| !matches!(r.kind, "element" | "input") || r.created.elapsed() < REF_AGE);
        let existing = if reference.kind != "element" {
            self.refs
                .iter()
                .find(|(_, r)| {
                    r.kind == reference.kind
                        && self.backend.same_element(&r.element, &reference.element)
                        && self.backend.validate(&r.element, None).is_ok()
                })
                .map(|(id, _)| id.clone())
        } else {
            None
        };
        if existing.is_none() && self.refs.len() >= MAX_REFS {
            return Err("reference_limit: refresh this task's desktop session".into());
        }
        // Short opaque handles reduce transcription mistakes and output size.
        // They convey identity only; task authorization is checked separately.
        let id = existing.unwrap_or_else(|| loop {
            let id = format!(
                "{}_{}",
                &reference.kind[..1],
                &uuid::Uuid::now_v7().simple().to_string()[20..]
            );
            if !self.refs.contains_key(&id) {
                break id;
            }
        });
        let mut value = serde_json::to_value(&reference.observed).unwrap();
        value["ref"] = json!(id);
        value["kind"] = json!(reference.kind);
        value["windowRef"] = json!(reference.window_ref);
        self.refs.insert(id, reference);
        Ok(value)
    }
    fn resolve(&self, id: &str) -> Result<Reference<B::Element>, String> {
        let reference = self.refs.get(id).filter(|r| !matches!(r.kind,"element"|"input") || r.created.elapsed() < REF_AGE)
            .ok_or("stale_reference: unknown, expired, or belongs to another session; refresh discovery/snapshot")?;
        self.backend
            .validate(&reference.element, reference.window.as_ref())?;
        Ok(reference.clone())
    }
    fn observe(
        &mut self,
        window_id: &str,
        args: &Value,
        token: &CancellationToken,
        reset: bool,
    ) -> Result<Value, String> {
        let previous = if reset {
            self.refs.clone()
        } else {
            HashMap::new()
        };
        let mut result = self.observe_fresh(window_id, args, token, reset)?;
        self.reuse_unchanged_refs(&previous, &mut result);
        Ok(result)
    }
    fn reuse_unchanged_refs(
        &mut self,
        previous: &HashMap<String, Reference<B::Element>>,
        value: &mut Value,
    ) {
        let mut replacements = HashMap::new();
        for (id, fresh) in &self.refs {
            if fresh.kind != "element" {
                continue;
            }
            let mut current = fresh.observed.clone();
            current.focused = false;
            if let Some((old_id, _)) = previous.iter().find(|(_, old)| {
                let mut observed = old.observed.clone();
                observed.focused = false;
                old.kind == "element"
                    && old.created.elapsed() < REF_AGE
                    && old.window_ref == fresh.window_ref
                    && old.observed_document == fresh.observed_document
                    && old
                        .approved_window
                        .as_ref()
                        .zip(fresh.approved_window.as_ref())
                        .is_some_and(|(old, current)| {
                            let mut old = old.clone();
                            old.focused = false;
                            let mut current = current.clone();
                            current.focused = false;
                            old == current
                        })
                    && observed == current
                    && self.backend.same_element(&old.element, &fresh.element)
            }) {
                if old_id != id {
                    replacements.insert(id.clone(), old_id.clone());
                }
            }
        }
        for (fresh, old) in &replacements {
            if let Some(reference) = self.refs.remove(fresh) {
                self.refs.insert(old.clone(), reference);
            }
        }
        fn replace(value: &mut Value, replacements: &HashMap<String, String>) {
            match value {
                Value::Array(items) => {
                    for item in items {
                        replace(item, replacements);
                    }
                }
                Value::Object(object) => {
                    for (key, item) in object {
                        if matches!(key.as_str(), "ref" | "parentRef") {
                            if let Some(id) = item.as_str().and_then(|s| replacements.get(s)) {
                                *item = json!(id);
                            }
                        } else {
                            replace(item, replacements);
                        }
                    }
                }
                _ => {}
            }
        }
        replace(value, &replacements);
    }
    fn observe_fresh(
        &mut self,
        window_id: &str,
        args: &Value,
        token: &CancellationToken,
        reset: bool,
    ) -> Result<Value, String> {
        let window = self.resolve(window_id)?;
        let location = self.backend.document_url(&window.element);
        let (document_url, document_url_error) = match location {
            Ok(url) => (url, None),
            Err(error) => (None, Some(error.chars().take(256).collect::<String>())),
        };
        if window.kind != "window" {
            return Err("invalid_argument: windowRef must refer to a window".into());
        }
        if reset {
            self.refs
                .retain(|_, r| r.kind != "element" || r.window_ref.as_deref() != Some(window_id));
            self.pages
                .retain(|_, page| page.window_ref.as_deref() != Some(window_id));
        }
        let window_metadata = redact(self.backend.inspect(&window.element)?);
        let start = Instant::now();
        let depth = number(args, "maxDepth", 8, 20);
        let max = number(args, "maxNodes", 250, 1000);
        let text_limit = number(args, "maxText", 24000, 64000);
        // Visit siblings before expanding large document/file-list subtrees.
        // Otherwise the bounded time/text budget can hide shallow dialog buttons.
        let root = self.backend.observation_root(&window.element)?;
        let root_role = self.backend.inspect(&root)?.role;
        let mut pending = VecDeque::from([(root, 0, None::<String>)]);
        let mut nodes = Vec::new();
        let mut truncated = false;
        let mut tree_truncated = false;
        let mut metadata_truncated = false;
        let mut text_count = 0;
        let mut metadata_incomplete = false;
        let mut issue_count = 0;
        let mut issues = Vec::new();
        let mut record_issue = |reference: Option<&str>, error: &str| {
            issue_count += 1;
            if issues.len() < 4 {
                issues.push(
                    json!({"ref":reference,"error":error.chars().take(160).collect::<String>()}),
                );
            }
        };
        while let Some((element, level, parent)) = pending.pop_front() {
            if token.is_cancelled() {
                return Err("cancelled: snapshot interrupted".into());
            }
            if nodes.len() >= max || start.elapsed() > BUDGET || text_count >= text_limit {
                truncated = true;
                tree_truncated = true;
                break;
            }
            let mut node = match self.backend.inspect(&element) {
                Ok(node) => redact(node),
                Err(error) => {
                    if parent.is_none() || fatal_observation_error(&error) {
                        return Err(error);
                    }
                    record_issue(parent.as_deref(), &error);
                    truncated = true;
                    tree_truncated = true;
                    continue;
                }
            };
            metadata_incomplete |= !node.observation_errors.is_empty();
            metadata_truncated |= node.metadata_truncated;
            truncated |= node.metadata_truncated;
            if let Some(value) = node.value.as_mut() {
                let available = text_limit.saturating_sub(text_count);
                if value.chars().count() > available {
                    *value = value.chars().take(available).collect();
                    node.text_truncated = true;
                }
                text_count += value.chars().count();
            }
            let mut item = self.insert(Reference {
                element: element.clone(),
                window: Some(window.element.clone()),
                window_ref: Some(window_id.into()),
                kind: "element",
                observed: node,
                observed_document: document_url.clone(),
                approved_window: Some(window_metadata.clone()),
                created: Instant::now(),
            })?;
            item["parentRef"] = json!(parent);
            let id = item["ref"].as_str().unwrap().to_string();
            if let Some(errors) = item["observationErrors"].as_array() {
                for error in errors.iter().filter_map(Value::as_str) {
                    record_issue(Some(&id), error);
                }
            }
            let protected = item["protected"] == true;
            item["childrenRedacted"] = json!(protected);
            let (children, more) = if protected {
                (Vec::new(), false)
            } else {
                match self.backend.children(
                    &element,
                    max.saturating_sub(nodes.len() + pending.len() + 1).max(1),
                ) {
                    Ok(children) => children,
                    Err(error) if fatal_observation_error(&error) => return Err(error),
                    Err(error) => {
                        record_issue(Some(&id), &error);
                        (Vec::new(), true)
                    }
                }
            };
            tree_truncated |= more || (level >= depth && !children.is_empty());
            item["childrenTruncated"] = json!(more || (level >= depth && !children.is_empty()));
            truncated |=
                more || (level >= depth && !children.is_empty()) || item["textTruncated"] == true;
            nodes.push(item);
            if level < depth {
                for child in children {
                    pending.push_back((child, level + 1, Some(id.clone())));
                }
            }
        }
        Ok(
            json!({"windowRef":window_id,"observationRootRole":root_role,"focusedDialog":root_role=="AXSheet","documentUrl":document_url,"documentUrlError":document_url_error,"snapshotId":identity(),"nodes":nodes,"truncated":truncated,"treeTruncated":tree_truncated,"metadataTruncated":metadata_truncated,
            "metadataIncomplete":metadata_incomplete,"status":if truncated||issue_count>0 {"incomplete"} else {"complete"},
            "issues":issues,"issueCount":issue_count,"issuesTruncated":issue_count>4,
            "coordinateUnit":if cfg!(target_os="windows") {"physical_desktop_pixels"} else {"desktop_logical_points"},
            "origin":"primary-display-top-left; negative coordinates allowed", "coverage":"provider_exposed_tree_only",
            "completeHistory":false,"continuation":"read individual text using offset/limit; scroll or page then take a new snapshot for unloaded/virtual content"}),
        )
    }
    fn run(
        &mut self,
        method: &str,
        args: &Value,
        token: &CancellationToken,
    ) -> Result<Value, String> {
        if method == "_invalidate_elements" {
            self.refs
                .retain(|_, r| !matches!(r.kind, "element" | "input"));
            self.approvals.clear();
            self.pages.clear();
            return Ok(json!({"invalidated":true}));
        }
        if token.is_cancelled() {
            return Err("cancelled: no action dispatched".into());
        }
        if let Err(error) = self.backend.check_session() {
            self.refs.clear();
            self.approvals.clear();
            self.pages.clear();
            return Err(error);
        }
        self.pages
            .retain(|_, page| page.created.elapsed() < REF_AGE);
        if method == "computer_continue" {
            let page = self.pages.remove(string(args, "cursor")?)
                .ok_or("stale_cursor: unknown, expired, consumed, or belongs to another session; refresh discovery")?;
            if let Some(window) = &page.window_ref {
                // Continuing an immutable observation has its own 120s lifetime.
                // A discovery ref may be older than the snapshot; do not shorten
                // the cursor's lifetime or renew that ref's authority for actions.
                let reference = self
                    .refs
                    .get(window)
                    .ok_or("stale_cursor: original window was released; refresh discovery")?;
                self.backend
                    .validate(&reference.element, reference.window.as_ref())?;
            }
            return paging::next(page, &mut self.pages);
        }
        let follow = if [
            "computer_key_window",
            "computer_set_value",
            "computer_invoke",
            "computer_click_control",
            "computer_activate_window",
            "computer_file_dialog",
        ]
        .contains(&method)
        {
            args.get("ref")
                .and_then(Value::as_str)
                .and_then(|id| self.resolve(id).ok())
                .and_then(|r| self.backend.application_element(&r.element).ok().flatten())
        } else {
            None
        };
        let previous = if follow.is_some() {
            self.refs.clone()
        } else {
            HashMap::new()
        };
        let mut result = if method == "computer_context" {
            retry_observation(token, || self.run_inner(method, args, token))?
        } else {
            self.run_inner(method, args, token)?
        };
        if let Some(app) = follow {
            if !token.is_cancelled() && args.get("observe") != Some(&json!(false)) {
                std::thread::sleep(Duration::from_millis(120));
                match retry_observation(token, || {
                    self.context_for_app(app.clone(), None, token, method == "computer_key_window")
                }) {
                    Ok(mut state) => {
                        self.reuse_unchanged_refs(&previous, &mut state);
                        let state_args = json!({"windowRef":state["windowRef"]});
                        paging::attach_state(&mut result, &state_args, state, &mut self.pages);
                    }
                    Err(e) => result["stateError"] = json!(e),
                }
            }
        }
        if method.starts_with('_') {
            return Ok(result);
        }
        if method == "computer_read" {
            paging::bound_read(&mut result);
            return Ok(result);
        }
        paging::previews(&mut result);
        let context_args = json!({"windowRef":result["windowRef"]});
        paging::start(
            method,
            if method == "computer_context" {
                &context_args
            } else {
                args
            },
            result,
            &mut self.pages,
        )
    }
    fn run_inner(
        &mut self,
        method: &str,
        args: &Value,
        token: &CancellationToken,
    ) -> Result<Value, String> {
        if token.is_cancelled() {
            return Err("cancelled: no action dispatched".into());
        }
        self.approvals
            .retain(|_, r| r.created.elapsed() < Duration::from_secs(300));
        match method {
            "_verify_visual" => {
                visual_text::validate(args)?;
                let before = self.run_inner("_guard_window", args, token)?;
                let target =
                    self.resolve(before["windowRef"].as_str().ok_or("window_required")?)?;
                let image = self.backend.capture_window(&target.element)?;
                let mut result = visual_text::verify(&image, args, token, |png| {
                    self.backend.recognize_text(png)
                })?;
                let after = self.run_inner("_guard_window", args, token)?;
                if !super::visual_target::same_window(&before, &after) {
                    return Err(
                        "target_changed: requested window changed during visual verification"
                            .into(),
                    );
                }
                result["window"] = after;
                Ok(result)
            }
            "_capture_foreground_frame" => {
                let before = self.run_inner("_guard_window", args, token)?;
                let target =
                    self.resolve(before["windowRef"].as_str().ok_or("window_required")?)?;
                let image = self.backend.capture_window(&target.element)?;
                let after = self.run_inner("_guard_window", args, token)?;
                if !super::visual_target::same_window(&before, &after) {
                    return Err("target_changed: requested window changed during capture".into());
                }
                Ok(json!({"guard":after,"capture":image}))
            }
            "_guard_point" => {
                let window = self.backend.foreground_window()?;
                if let Some(expected) = args["windowRef"].as_str() {
                    if !self
                        .backend
                        .same_element(&window, &self.resolve(expected)?.element)
                    {
                        return Err("foreground_required: the requested window is not foreground; no input dispatched".into());
                    }
                }
                let x = args["x"]
                    .as_f64()
                    .filter(|v| v.is_finite())
                    .ok_or("invalid_point")?;
                let y = args["y"]
                    .as_f64()
                    .filter(|v| v.is_finite())
                    .ok_or("invalid_point")?;
                let hit = self.backend.hit_target(&window, x, y)?;
                self.backend.validate(&hit, Some(&window))?;
                let node = redact(self.backend.inspect(&hit)?);
                if node.protected {
                    return Err("protected: visual target requires user takeover".into());
                }
                let process_ids = self.backend.process_ids(&window);
                let window_info = self.backend.inspect(&window)?;
                let document = self.backend.document_url(&window)?;
                let reference = self.insert(Reference {
                    element: hit,
                    window: Some(window),
                    window_ref: None,
                    kind: "element",
                    observed: node.clone(),
                    observed_document: document,
                    approved_window: Some(window_info),
                    created: Instant::now(),
                })?;
                Ok(
                    json!({"ref":reference["ref"],"application":node.application,"name":node.name,"role":node.role,"identifier":node.identifier,"processIds":process_ids}),
                )
            }
            "_click_point" => {
                let target = self.resolve(string(args, "ref")?)?;
                let window = target.window.as_ref().ok_or("window_required")?;
                let x = args["x"].as_f64().ok_or("invalid_point")?;
                let y = args["y"].as_f64().ok_or("invalid_point")?;
                let focused = self.backend.foreground_window()?;
                if !self.backend.same_element(window, &focused) {
                    return Err("focus_changed: no visual click dispatched".into());
                }
                if let Some(expected) = &target.approved_window {
                    let mut expected = expected.clone();
                    let mut current = self.backend.inspect(window)?;
                    expected.focused = false;
                    current.focused = false;
                    if current != expected
                        || self.backend.document_url(window)? != target.observed_document
                    {
                        return Err("target_changed: original visual window changed".into());
                    }
                }
                let hit = self.backend.hit_target(window, x, y)?;
                let node = redact(self.backend.inspect(&hit)?);
                let mut before = target.observed.clone();
                let mut current = node.clone();
                before.focused = false;
                current.focused = false;
                if !self.backend.same_element(&hit, &target.element) || before != current {
                    return Err(
                        "target_changed: visual hit target changed; no click dispatched".into(),
                    );
                }
                if node.protected || !node.enabled || node.visible == Some(false) {
                    return Err("protected: target unavailable".into());
                }
                if token.is_cancelled() {
                    return Err("cancelled: no click dispatched".into());
                }
                #[cfg(any(target_os = "macos", target_os = "windows"))]
                let result = super::computer::click_button_after_native_target(
                    x,
                    y,
                    args["button"].as_str().unwrap_or("left"),
                    args["clickCount"].as_u64().unwrap_or(1).clamp(1, 3) as u32,
                );
                #[cfg(not(any(target_os = "macos", target_os = "windows")))]
                let result: Result<(), String> =
                    Err("unsupported_action: verified visual click unavailable".into());
                self.refs
                    .retain(|_, r| !matches!(r.kind, "element" | "input"));
                Ok(match result {
                    Ok(()) => {
                        json!({"clicked":true,"verified":false,"status":"dispatched_unverified","frameInvalidated":true})
                    }
                    Err(error) => {
                        json!({"status":"result_unknown","verified":false,"error":error,"replaySafe":false})
                    }
                })
            }
            "_guard_window" | "_guard_input" => {
                let window = self.backend.foreground_window()?;
                if let Some(expected) = args["ref"].as_str() {
                    let expected = self.resolve(expected)?;
                    if expected.kind != "window"
                        || !self.backend.same_element(&window, &expected.element)
                    {
                        return Err("foreground_required: activate the explicitly requested window before foreground input; observation must not switch to another app".into());
                    }
                }
                let info = self.backend.inspect(&window)?;
                let document = self.backend.document_url(&window)?;
                let process_ids = self.backend.process_ids(&window);
                let identifier = self.backend.application_identifier(&window);
                let focused = if method == "_guard_input" {
                    self.backend.keyboard_focus(&window)?
                } else {
                    None
                };
                let node = focused
                    .as_ref()
                    .map(|focused| -> Result<Node, String> {
                        self.backend.validate(focused, Some(&window))?;
                        let node = self.backend.inspect(focused)?;
                        if node.protected {
                            return Err(
                                "protected: user takeover required; no visual input dispatched"
                                    .into(),
                            );
                        }
                        Ok(node)
                    })
                    .transpose()?;
                let reference = self.insert(Reference {
                    element: window.clone(),
                    window: None,
                    window_ref: None,
                    kind: "window",
                    observed: info.clone(),
                    observed_document: None,
                    approved_window: None,
                    created: Instant::now(),
                })?;
                let focus = if let (Some(focused), Some(node)) = (focused, node) {
                    Some(self.insert(Reference {
                        element: focused,
                        window: Some(window),
                        window_ref: reference["ref"].as_str().map(str::to_owned),
                        kind: "input",
                        observed: node,
                        observed_document: None,
                        approved_window: None,
                        created: Instant::now(),
                    })?)
                } else {
                    None
                };
                Ok(
                    json!({"windowRef":reference["ref"],"focusRef":focus.as_ref().map(|f|f["ref"].clone()),"focusAvailable":focus.is_some(),"application":info.application,"applicationIdentifier":identifier,"windowName":info.name,"windowBounds":info.bounds,"documentUrl":document,"processIds":process_ids}),
                )
            }
            "computer_context" => self.desktop_context(args, token),
            "_open_app" => {
                let foreground = args["foreground"] == true;
                let opened = if foreground {
                    self.backend.reopen_app(string(args, "appPath")?)
                } else {
                    self.backend.open_app(string(args, "appPath")?)
                };
                let verified = match opened {
                    Ok(verified) => verified,
                    Err(error) => {
                        return Ok(
                            json!({"status":"result_unknown","executed":null,"verified":false,"error":error,"replaySafe":false}),
                        )
                    }
                };
                Ok(
                    json!({"executed":true,"verified":verified,"runningVerified":verified,"windowVerified":false,"foregroundRequested":foreground,"guidance":"Read computer_context for this app. If it has no usable window, foreground=true explicitly asks the OS to reopen its main window. Running alone does not verify a usable window.","status":if verified {"verified"} else {"executed_unverified"},"replaySafe":false}),
                )
            }
            "computer_list_apps" | "computer_list_windows" => {
                let app = args
                    .get("appRef")
                    .and_then(Value::as_str)
                    .map(|id| self.resolve(id))
                    .transpose()?;
                if app.as_ref().is_some_and(|a| a.kind != "app") {
                    return Err("invalid_argument: appRef is not an application".into());
                }
                let apps = method == "computer_list_apps";
                let mut listing = if apps {
                    self.backend.apps()?
                } else {
                    self.backend.windows(app.as_ref().map(|r| &r.element))?
                };
                let filtered = apps
                    && ["name", "identifier"]
                        .iter()
                        .any(|key| args.get(*key).is_some());
                if filtered {
                    for key in ["name", "identifier"] {
                        if args.get(key).is_some() {
                            string(args, key)?;
                        }
                    }
                    listing.items.retain(|(_, node)| matches(node, args));
                }
                let match_status = if filtered {
                    Some(match listing.items.len() {
                        n if n > 1 => "ambiguous",
                        _ if !listing.issues.is_empty() => "incomplete",
                        0 => "not_found",
                        _ => "unique",
                    })
                } else {
                    None
                };
                let issue_count = listing.issues.len();
                let issue_details_truncated =
                    listing.issues.iter().take(4).any(|(app, error)| {
                        app.chars().count() > 32 || error.chars().count() > 120
                    });
                let issues: Vec<_> = listing
                    .issues
                    .into_iter()
                    .take(4)
                    .map(|(application, error)| {
                        json!({
                            "application":application.chars().take(32).collect::<String>(),
                            "error":error.chars().take(120).collect::<String>()
                        })
                    })
                    .collect();
                let truncated = listing.items.len() > 256 || issue_count > 0;
                let mut output = Vec::new();
                for (element, node) in listing.items.into_iter().take(256) {
                    let location = if apps {
                        Ok(None)
                    } else {
                        self.backend.document_url(&element)
                    };
                    let mut item = self.insert(Reference {
                        element,
                        window: None,
                        window_ref: None,
                        kind: if apps { "app" } else { "window" },
                        observed: redact(node),
                        observed_document: None,
                        approved_window: None,
                        created: Instant::now(),
                    })?;
                    if !apps {
                        match location {
                            Ok(url) => {
                                item["documentUrl"] = json!(url);
                            }
                            Err(error) => {
                                item["documentUrlError"] =
                                    json!(error.chars().take(256).collect::<String>());
                            }
                        }
                    }
                    output.push(item);
                }
                Ok(
                    json!({"items":output,"truncated":truncated,"matchStatus":match_status,
                    "status":if truncated {"incomplete"} else {"complete"},
                    "probe":if issue_count==0 {"succeeded"} else if output.is_empty() {"failed"} else {"partial"},
                    "issues":issues,"issueCount":issue_count,"issuesTruncated":issue_count>4,
                    "issueDetailsTruncated":issue_details_truncated,
                    "continuation":"consume nextCursor for already observed items; retry windows with appRef for inaccessible or unvisited applications"}),
                )
            }
            "computer_snapshot" | "computer_find" => {
                let snapshot = self.observe(
                    string(args, "windowRef")?,
                    args,
                    token,
                    method == "computer_snapshot",
                )?;
                if method == "computer_snapshot" {
                    return Ok(snapshot);
                }
                if !["role", "name", "identifier"]
                    .iter()
                    .any(|k| args.get(*k).is_some())
                {
                    return Err("invalid_argument: provide role, name or identifier".into());
                }
                let candidates: Vec<_> = snapshot["nodes"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter(|v| {
                        matches(&serde_json::from_value::<Node>((*v).clone()).unwrap(), args)
                    })
                    .cloned()
                    .collect();
                // Independent find queries must not revoke one another's live
                // native handles. Keep only this query's matches and ancestry;
                // repeated not-found waits must not fill the reference cache.
                let nodes = snapshot["nodes"].as_array().unwrap();
                let mut retained = std::collections::HashSet::new();
                for candidate in &candidates {
                    let mut current = Some(candidate);
                    while let Some(node) = current {
                        let id = node["ref"].as_str().unwrap();
                        if !retained.insert(id) {
                            break;
                        }
                        current = node["parentRef"].as_str().and_then(|parent| {
                            nodes.iter().find(|n| n["ref"].as_str() == Some(parent))
                        });
                    }
                }
                for node in nodes {
                    let id = node["ref"].as_str().unwrap();
                    if !retained.contains(id) {
                        self.refs.remove(id);
                    }
                }
                // Unreadable labels do not hide known roles. Name/id searches
                // remain incomplete when any selector metadata was unavailable.
                let selector_metadata_incomplete = snapshot["metadataTruncated"] == true
                    && (args.get("name").is_some() || args.get("identifier").is_some());
                let search_complete =
                    snapshot["treeTruncated"] == false && !selector_metadata_incomplete;
                Ok(
                    json!({"status": match candidates.len() {n if n>1=>"ambiguous",_ if !search_complete=>"incomplete",0=>"not_found",_=>"unique"},"candidates":candidates,"truncated":!search_complete,"searchComplete":search_complete,"completeHistory":false,
                    "windowRef":snapshot["windowRef"],"focusedDialog":snapshot["focusedDialog"],"observationRootRole":snapshot["observationRootRole"],"snapshotId":snapshot["snapshotId"],"metadataIncomplete":snapshot["metadataIncomplete"],
                    "issues":snapshot["issues"],"issueCount":snapshot["issueCount"],"issuesTruncated":snapshot["issuesTruncated"]}),
                )
            }
            "computer_read" | "_read_matches" => {
                let target = self.resolve(string(args, "ref")?)?;
                if self.backend.inspect(&target.element)?.protected {
                    return Err("protected: user takeover required".into());
                }
                let offset = args.get("offset").and_then(Value::as_u64).unwrap_or(0);
                if offset > 2_000_000 {
                    return Err("invalid_argument: offset exceeds bounded provider text; scroll and refresh instead".into());
                }
                let offset = offset as usize;
                let limit = number(args, "limit", 8000, 64000);
                let (text, more) = self.backend.read(&target.element, offset, limit)?;
                if method == "_read_matches" {
                    return Ok(
                        json!({"matches":!more && Some(text.as_str()) == args["text"].as_str()}),
                    );
                }
                Ok(
                    json!({"text":text,"offset":offset,"nextOffset":if text.is_empty()&&more {None}else{Some(offset+text.chars().count())},"continuationBlocked":text.is_empty()&&more,"truncated":more,"completeHistory":false,"coverage":"provider_exposed_text_only","offsetUnit":"unicode_scalars"}),
                )
            }
            "_prepare" => {
                let mut target = self.resolve(string(args, "ref")?)?;
                if target.kind == "element" {
                    if let Some(window) = &target.window {
                        let mut current = redact(self.backend.inspect(window)?);
                        current.focused = false;
                        let expected = target.approved_window.as_ref().map(|n| {
                            let mut n = n.clone();
                            n.focused = false;
                            n
                        });
                        if expected.as_ref() != Some(&current)
                            || self.backend.document_url(window)? != target.observed_document
                        {
                            return Err("stale_reference: original document context changed; use current state".into());
                        }
                    }
                }
                let observed = redact(self.backend.inspect(&target.element)?);
                if observed.protected {
                    return Err("protected: user takeover required".into());
                }
                if !observed.enabled {
                    return Err("disabled: action not dispatched".into());
                }
                let mut previous = target.observed.clone();
                previous.focused = false;
                let mut current = observed.clone();
                current.focused = false;
                if target.kind != "app" && current != previous {
                    return Err(
                        "stale_reference: target changed since snapshot; refresh before approval"
                            .into(),
                    );
                }
                target.observed = observed;
                if self.approvals.len() >= 32 {
                    return Err("approval_limit: too many pending desktop actions".into());
                }
                let proof = identity();
                // Do not serialize values or input text into approval UI/logs.
                target.approved_window = target
                    .window
                    .as_ref()
                    .map(|w| self.backend.inspect(w).map(redact))
                    .transpose()?;
                let window_name = if target.kind == "window" {
                    Some(target.observed.name.clone())
                } else {
                    target.approved_window.as_ref().map(|n| n.name.clone())
                };
                let result = json!({"proof":proof,"target":{"processId":self.backend.process_id(&target.element),"processIds":self.backend.process_ids(&target.element),"application":target.observed.application,"applicationIdentifier":self.backend.application_identifier(&target.element),"windowName":window_name,"windowRef":target.window_ref,"role":target.observed.role,"name":target.observed.name,"identifier":target.observed.identifier,"bounds":target.observed.bounds,"kind":target.kind}});
                target.created = Instant::now();
                self.approvals.insert(proof, target);
                Ok(result)
            }
            "computer_capture_window" => {
                let target = self.resolve(string(args, "ref")?)?;
                if target.kind != "window" {
                    return Err("invalid_argument: observed window ref required".into());
                }
                self.backend.validate(&target.element, None)?;
                self.backend.capture_window(&target.element)
            }
            "computer_activate_window"
            | "computer_invoke"
            | "computer_set_value"
            | "computer_key_window"
            | "computer_file_dialog"
            | "computer_click_control" => {
                let target = self
                    .approvals
                    .remove(string(args, "_proof")?)
                    .ok_or("stale_approval: observe and request approval again")?;
                self.backend
                    .validate(&target.element, target.window.as_ref())?;
                if let (Some(window), Some(expected)) = (&target.window, &target.approved_window) {
                    let mut current = redact(self.backend.inspect(window)?);
                    let mut expected = expected.clone();
                    current.focused = false;
                    expected.focused = false;
                    if current != expected {
                        return Err(
                            "target_changed: approved window context changed; action cancelled"
                                .into(),
                        );
                    }
                }
                let before = redact(self.backend.inspect(&target.element)?);
                // Focus commonly changes when the user clicks the approval card.
                let mut expected = target.observed.clone();
                expected.focused = false;
                let mut current = before.clone();
                current.focused = false;
                if current != expected {
                    return Err("target_changed: approved target changed; action cancelled, refresh snapshot".into());
                }
                if before.protected {
                    return Err("protected: user takeover required".into());
                }
                if !before.enabled {
                    return Err("disabled: action not dispatched".into());
                }
                if token.is_cancelled() {
                    return Err("cancelled: action not dispatched".into());
                }
                if method == "computer_file_dialog" {
                    if target.kind != "window" {
                        return Err("invalid_argument: ref must be an observed window".into());
                    }
                    let result = file_dialog::prepare(
                        &self.backend,
                        &target.element,
                        string(args, "path")?,
                        token,
                    );
                    self.refs
                        .retain(|_, r| !matches!(r.kind, "element" | "input"));
                    self.pages.retain(|_, p| p.window_ref.is_none());
                    return Ok(result.unwrap_or_else(|error| json!({"prepared":false,"verified":false,"status":"result_unknown","error":error,"replaySafe":false})));
                }
                if method == "computer_key_window" {
                    if target.kind != "window" {
                        return Err("invalid_argument: requires an observed window ref".into());
                    }
                    let (key, modifiers) = window_key_input(args)?;
                    match self.backend.prepare_input_window(&target.element) {
                        Ok(true) => {}
                        focus => {
                            return Ok(
                                json!({"executed":false,"keyDispatched":false,"focusVerified":false,"verified":false,"status":"result_unknown","phase":"focus","error":focus.err(),"replaySafe":false}),
                            )
                        }
                    }
                    if token.is_cancelled() {
                        return Ok(
                            json!({"executed":false,"keyDispatched":false,"focusVerified":true,"verified":false,"status":"cancelled","phase":"before_key","replaySafe":false}),
                        );
                    }
                    let result =
                        self.backend
                            .key_cancellable(&target.element, key, &modifiers, token);
                    self.refs
                        .retain(|_, r| !matches!(r.kind, "element" | "input"));
                    self.pages.retain(|_, p| p.window_ref.is_none());
                    return Ok(match result {
                        Ok(()) => {
                            json!({"dispatched":true,"executed":null,"keyDispatched":true,"focusVerified":true,"verified":false,"status":"dispatched_unverified","refsInvalidated":true,"replaySafe":false})
                        }
                        Err(error) => {
                            json!({"executed":null,"keyDispatched":null,"focusVerified":true,"verified":false,"status":"result_unknown","phase":"key","error":error,"refsInvalidated":true,"replaySafe":false})
                        }
                    });
                }
                if method == "computer_activate_window" {
                    if !["window", "app"].contains(&target.kind) {
                        return Err("invalid_argument: requires app/window ref".into());
                    }
                    let verified = match self.backend.activate(&target.element) {
                        Ok(verified) => verified,
                        Err(error) => {
                            return Ok(
                                json!({"status":"result_unknown","executed":null,"verified":false,"error":error,"replaySafe":false}),
                            )
                        }
                    };
                    return Ok(
                        json!({"executed":true,"verified":verified,"status":if verified {"verified"} else {"result_unknown"},"replaySafe":false}),
                    );
                }
                if target.kind != "element" {
                    return Err("invalid_argument: requires element ref".into());
                }
                // Approval may have moved focus; native actions address the retained
                // object directly and never synthesize unconfirmed coordinate input.
                let result = if method == "computer_click_control" {
                    let count = args
                        .get("clickCount")
                        .map(|v| {
                            v.as_u64()
                                .ok_or("invalid_argument: clickCount must be an integer")
                        })
                        .transpose()?
                        .unwrap_or(1);
                    if !(1..=2).contains(&count) {
                        return Err("invalid_argument: clickCount must be 1 or 2".into());
                    }
                    let window = target
                        .window
                        .as_ref()
                        .ok_or("invalid_argument: control requires its original window")?;
                    self.backend
                        .click_control(&target.element, window, count as u32, token)
                } else if method == "computer_set_value" {
                    let (value, keyboard) = text_input(args)?;
                    if keyboard {
                        if ![
                            "AXTextArea",
                            "AXTextField",
                            "AXComboBox",
                            "edit",
                            "document",
                            "combo_box",
                        ]
                        .contains(&before.role.as_str())
                            || !before.actions.iter().any(|a| a == "focus")
                        {
                            return Err("unsupported_action: keyboard replacement requires an observed focusable text control".into());
                        }
                        let window = target
                            .window
                            .as_ref()
                            .ok_or("invalid_argument: text control requires its original window")?;
                        self.backend
                            .replace_text(&target.element, window, value, token)
                    } else {
                        if !before.writable {
                            return Err(
                                "readonly: use a fresh screenshot for confirmed keyboard fallback"
                                    .into(),
                            );
                        }
                        self.backend.set_value(&target.element, value)
                    }
                } else {
                    let action = string(args, "action")?;
                    if !before.actions.iter().any(|a| a == action) {
                        return Err(
                            "unsupported_action: refresh screenshot for coordinate fallback".into(),
                        );
                    }
                    self.backend.invoke(&target.element, action)
                };
                // A native call may have performed the action before returning an
                // error. Consume all control refs and never automatically replay.
                self.refs
                    .retain(|_, r| !matches!(r.kind, "element" | "input"));
                self.pages.retain(|_, page| page.window_ref.is_none());
                if let Err(error) = result {
                    return Ok(
                        json!({"executed":null,"verified":false,"status":"result_unknown","error":error,"replaySafe":false}),
                    );
                }
                let mut after = self.backend.inspect(&target.element).map(redact);
                let mut observation_error = after.as_ref().err().cloned();
                let verified = if method == "computer_set_value" {
                    let deadline = Instant::now() + Duration::from_millis(500);
                    loop {
                        match self.backend.read(&target.element, 0, 64001) {
                            Ok((actual, more))
                                if !more
                                    && actual == args["value"].as_str().unwrap_or_default() =>
                            {
                                break true
                            }
                            Ok(_) => {}
                            Err(error) => {
                                observation_error = Some(error);
                                break false;
                            }
                        }
                        if args["inputMethod"] != "keyboard"
                            || token.is_cancelled()
                            || Instant::now() >= deadline
                        {
                            break false;
                        }
                        std::thread::sleep(Duration::from_millis(25));
                    }
                } else {
                    matches!(&after,Ok(node) if (args["action"]=="focus" && node.focused) || (args["action"]=="select" && node.selected==Some(true)) || (args["action"]=="expand" && node.expanded==Some(true)) || (args["action"]=="collapse" && node.expanded==Some(false)))
                };
                if method == "computer_set_value" {
                    // Keyboard events are asynchronous: the first inspection
                    // may precede their delivery. Return the post-readback
                    // observation instead of a contradictory empty value.
                    after = self.backend.inspect(&target.element).map(redact);
                    if let Err(error) = &after {
                        observation_error = Some(error.clone());
                    }
                }
                Ok(
                    json!({"executed":true,"verified":verified,"status":if verified {"verified"} else {"executed_unverified"},"before":before,"after":after.ok(),"observationError":observation_error,"refsInvalidated":true,"replaySafe":false}),
                )
            }
            _ => Err(format!("unsupported: {method}")),
        }
    }
}

fn fatal_observation_error(error: &str) -> bool {
    ["permission_denied", "application_exited", "cancelled"]
        .iter()
        .any(|prefix| error.starts_with(prefix))
}

pub struct ExecutionLease {
    pub _desktop: Option<super::desktop_coordinator::DesktopLease>,
    pub _input: Vec<super::desktop_coordinator::InputLease>,
    pub _session: tokio::sync::OwnedRwLockReadGuard<()>,
}
struct Request {
    _lease: Option<ExecutionLease>,
    method: String,
    args: Value,
    token: CancellationToken,
    reply: oneshot::Sender<Result<Value, String>>,
}
static WORKERS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
struct WorkerSlot;
impl Drop for WorkerSlot {
    fn drop(&mut self) {
        WORKERS.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }
}
struct WorkerLife(std::sync::Arc<std::sync::atomic::AtomicBool>);
impl Drop for WorkerLife {
    fn drop(&mut self) {
        self.0.store(false, std::sync::atomic::Ordering::SeqCst);
    }
}
pub struct Controller {
    sender: mpsc::SyncSender<Request>,
    alive: std::sync::Arc<std::sync::atomic::AtomicBool>,
}
impl Controller {
    pub async fn invalidate_elements(&self) -> Result<(), String> {
        let (reply, receiver) = oneshot::channel();
        self.sender
            .try_send(Request {
                _lease: None,
                method: "_invalidate_elements".into(),
                args: json!({}),
                token: CancellationToken::new(),
                reply,
            })
            .map_err(|_| "native worker unavailable".to_string())?;
        receiver
            .await
            .map_err(|_| "native worker stopped".to_string())??;
        Ok(())
    }
    pub fn is_alive(&self) -> bool {
        self.alive.load(std::sync::atomic::Ordering::SeqCst)
    }
    pub fn new() -> Self {
        let alive = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (sender, receiver) = mpsc::sync_channel::<Request>(8);
        if WORKERS
            .try_update(
                std::sync::atomic::Ordering::SeqCst,
                std::sync::atomic::Ordering::SeqCst,
                |n| (n < 16).then_some(n + 1),
            )
            .is_err()
        {
            return Self { sender, alive };
        }
        let slot = WorkerSlot;
        alive.store(true, std::sync::atomic::Ordering::SeqCst);
        let life = WorkerLife(alive.clone());
        std::thread::Builder::new()
            .name("desktop-accessibility".into())
            .spawn(move || {
                let _slot = slot;
                let _life = life;
                let backend = if cfg!(test) {
                    Platform::new()
                } else {
                    Err("native objects live in helper".into())
                };
                let mut process = if cfg!(test) {
                    None
                } else {
                    Some(helper::Process::new())
                };
                let mut context = backend.map(|backend| Context {
                    backend,
                    refs: HashMap::new(),
                    approvals: HashMap::new(),
                    pages: HashMap::new(),
                });
                while let Ok(request) = receiver.recv_timeout(REF_AGE) {
                    if request.reply.is_closed() || request.token.is_cancelled() {
                        continue;
                    }
                    let result = if let Some(process) = process.as_mut() {
                        match process {
                            Ok(process) => {
                                process.call(&request.method, &request.args, &request.token)
                            }
                            Err(error) => Err(error.clone()),
                        }
                    } else {
                        match context.as_mut() {
                            Ok(c) => {
                                #[cfg(target_os = "macos")]
                                {
                                    objc2::rc::autoreleasepool(|_| {
                                        c.run(&request.method, &request.args, &request.token)
                                    })
                                }
                                #[cfg(not(target_os = "macos"))]
                                {
                                    c.run(&request.method, &request.args, &request.token)
                                }
                            }
                            Err(e) => Err(e.clone()),
                        }
                    };
                    if result.is_ok()
                        && matches!(
                            request.method.as_str(),
                            "computer_snapshot" | "computer_find"
                        )
                    {
                        PROBED.store(1, std::sync::atomic::Ordering::Relaxed);
                    }
                    if result.as_ref().is_err_and(|e| {
                        [
                            "permission_denied",
                            "native_failure",
                            "native_timeout",
                            "accessibility_unavailable",
                            "desktop_unavailable",
                        ]
                        .iter()
                        .any(|code| e.starts_with(code))
                    }) {
                        PROBED.store(2, std::sync::atomic::Ordering::Relaxed);
                    }
                    let fatal = result.as_ref().is_err_and(|e| {
                        e.starts_with("native_timeout")
                            || e.starts_with("native_failure")
                            || e.starts_with("helper_")
                    });
                    let _ = request.reply.send(result);
                    if fatal && process.is_some() {
                        break;
                    }
                }
            })
            .ok();
        Self { sender, alive }
    }
    pub async fn call(
        &self,
        method: &str,
        args: Value,
        token: &CancellationToken,
        lease: ExecutionLease,
    ) -> Result<Value, String> {
        let (reply, receiver) = oneshot::channel();
        self.sender
            .try_send(Request {
                _lease:Some(lease),
                method: method.into(),
                args,
                token: token.clone(),
                reply,
            })
            .map_err(|_| {
                "native_unavailable: worker stopped, idle references expired, or queue full; no action dispatched; refresh discovery/snapshot".to_string()
            })?;
        // The Request owns both execution guards. Dropping this HTTP future
        // cannot release the physical desktop while native work is in flight.
        // Native RPCs have limits; cancellation does not replay unknown writes.
        receiver.await.map_err(|_| {
            "native_failure: worker exited; result unknown, observe before retry".to_string()
        })?
    }
}

#[cfg(test)]
mod tests;

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Capability {
    pub supported: bool,
    pub authorization: &'static str,
    pub probe: &'static str,
    pub reason: Option<String>,
}
static PROBED: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);
pub fn capability() -> Capability {
    let supported = cfg!(any(target_os = "macos", target_os = "windows"));
    #[cfg(target_os = "macos")]
    let authorization = if unsafe { macos::trusted() } {
        "granted"
    } else {
        "required"
    };
    #[cfg(not(target_os = "macos"))]
    let authorization = "unknown";
    Capability {
        supported,
        authorization,
        probe: match PROBED.load(std::sync::atomic::Ordering::Relaxed) {
            1 => "succeeded",
            2 => "failed",
            _ => "unknown",
        },
        reason: if !supported {
            Some(
                "Native controls require macOS or Windows; screenshot fallback remains available"
                    .into(),
            )
        } else if authorization == "required" {
            Some("请在系统设置中向实际运行的 EchoAgent 授予辅助功能权限".into())
        } else {
            None
        },
    }
}
#[cfg(all(test, target_os = "macos"))]
pub fn macos_probe() -> Result<(), String> {
    let backend = Platform::new()?;
    if capability().authorization != "granted" {
        return Err("permission_denied: this native test executable is not AX-trusted; no desktop input executed; installed EchoAgent permission is a separate identity".into());
    }
    let windows = backend.windows(None)?;
    println!(
        "native window enumeration observed count={}, issueCount={}, complete={}",
        windows.items.len(),
        windows.issues.len(),
        windows.issues.is_empty()
    );
    Ok(())
}

#[cfg(all(test, target_os = "macos"))]
mod visual_acceptance;
