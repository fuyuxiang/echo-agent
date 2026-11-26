//! Per-session guest path virtualization and bind-time mount hooks.
//!
//! The model sees [`VISIBLE_ROOT`]; the guest tree is `real_root`
//! (`/workspace/<conversation_id>`). Inbound also accepts today's
//! [`ARTIFACTS_ALIAS`]. A `..` walk out of `/workspace`, the artifacts
//! alias, or the already-guest real root is clipped to `real_root`.
//! True non-workspace absolutes (`/tmp`, `/home`) are left unchanged.

use std::borrow::Cow;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use echo_agent_tool_runtime::{
    ContentBlock, ToolChatCompletionResponse, ToolError, ToolProgress, TypedToolOutput,
};
use serde_json::Value;

/// Model-visible workspace root.
pub const VISIBLE_ROOT: &str = "/workspace";
/// Legacy model cwd; inbound alias of the session root.
pub const ARTIFACTS_ALIAS: &str = "/workspace/artifacts";

/// `visible_root` ↔ `real_root` mapping for one hub session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathVirtualization {
    visible_root: String,
    real_root: String,
}

impl PathVirtualization {
    /// Build a mapping if `session_root` is a usable absolute guest path.
    ///
    /// Returns `None` for empty, relative, or `.`/`..` paths so a malformed
    /// bind field cannot enable a broken rewrite.
    pub fn try_from_session_root(session_root: impl AsRef<Path>) -> Option<Self> {
        let real = normalize_session_root(session_root.as_ref().to_str()?)?;
        Some(Self {
            visible_root: VISIBLE_ROOT.to_owned(),
            real_root: real,
        })
    }

    pub fn visible_root(&self) -> &str {
        &self.visible_root
    }

    pub fn real_root(&self) -> &str {
        &self.real_root
    }

    pub fn real_root_path(&self) -> PathBuf {
        PathBuf::from(&self.real_root)
    }

    /// Outbound: guest path → model-visible path. Unrelated paths unchanged.
    pub fn to_model_visible<'a>(&self, path: &'a str) -> Cow<'a, str> {
        replace_path_prefix(path, &self.real_root, &self.visible_root)
    }

    /// Inbound: model path → guest path. `/workspace` and `/workspace/artifacts`
    /// both map to `real_root`. Already-guest paths under `real_root` stay put.
    ///
    /// A `..` walk that would leave `real_root` is clipped to `real_root`.
    /// Passing the original through would still resolve on the kernel to a
    /// sibling tree. Absolute escapes such as `/tmp` and `/home` are left as-is.
    pub fn to_guest<'a>(&self, path: &'a str) -> Cow<'a, str> {
        let mapped = if path_prefix_match(path, &self.real_root).is_some() {
            Cow::Borrowed(path)
        } else if let Some(rest) = path_prefix_match(path, ARTIFACTS_ALIAS) {
            Cow::Owned(join_root_suffix(&self.real_root, rest))
        } else if let Some(rest) = path_prefix_match(path, &self.visible_root) {
            Cow::Owned(join_root_suffix(&self.real_root, rest))
        } else {
            return Cow::Borrowed(path);
        };
        match stay_under_real_root(&mapped, &self.real_root) {
            Some(resolved) if resolved == path => Cow::Borrowed(path),
            Some(resolved) => Cow::Owned(resolved),
            None => Cow::Owned(self.real_root.clone()),
        }
    }

    pub fn rewrite_text_outbound<'a>(&self, text: &'a str) -> Cow<'a, str> {
        replace_path_prefix_in_text(text, &self.real_root, &self.visible_root)
    }

    pub fn rewrite_text_inbound<'a>(&self, text: &'a str) -> Cow<'a, str> {
        rewrite_text_inbound(self, text)
    }

    pub fn rewrite_json_outbound(&self, value: Value) -> Value {
        rewrite_json_strings(value, &|s| self.rewrite_text_outbound(s))
    }

    pub fn rewrite_json_inbound(&self, value: Value) -> Value {
        rewrite_json_inbound_value(value, self)
    }

    pub fn rewrite_typed_output(&self, mut output: TypedToolOutput) -> TypedToolOutput {
        output.value = self.rewrite_json_outbound(output.value);
        for block in &mut output.model_output {
            rewrite_content_block(block, |s| self.rewrite_text_outbound(s));
        }
        if let Some(cco) = output.chat_completion_output.as_mut() {
            rewrite_chat_completion_output(cco, &|s| self.rewrite_text_outbound(s));
        }
        output
    }

    pub fn rewrite_error(&self, mut error: ToolError) -> ToolError {
        if let Cow::Owned(detail) = self.rewrite_text_outbound(&error.detail) {
            error.detail = detail;
        }
        if let Some(details) = error.details.take() {
            error.details = Some(self.rewrite_json_outbound(details));
        }
        error
    }

    pub fn rewrite_progress(&self, progress: ToolProgress) -> ToolProgress {
        match progress {
            ToolProgress::Text { text } => ToolProgress::Text {
                text: self.rewrite_text_outbound(&text).into_owned(),
            },
            ToolProgress::Content { mut blocks } => {
                for block in &mut blocks {
                    rewrite_content_block(block, |s| self.rewrite_text_outbound(s));
                }
                ToolProgress::Content { blocks }
            }
            ToolProgress::Custom { subkind, payload } => ToolProgress::Custom {
                subkind,
                payload: self.rewrite_json_outbound(payload),
            },
        }
    }
}

/// Guest POSIX root: absolute, ≥1 segment, no `.`/`..`. Trailing `/` and
/// empty segments (`//`) are dropped so prefix match cannot miss.
fn normalize_session_root(path: &str) -> Option<String> {
    if !path.starts_with('/') {
        return None;
    }
    let mut parts: Vec<&str> = Vec::new();
    for part in path.split('/') {
        if part.is_empty() {
            continue;
        }
        if part == "." || part == ".." {
            return None;
        }
        parts.push(part);
    }
    if parts.is_empty() {
        return None;
    }
    Some(format!("/{}", parts.join("/")))
}

/// Collapse `.` / `..` without touching the filesystem. `None` if `..`
/// walks above `/`.
fn lexically_normalize(path: &str) -> Option<String> {
    if !path.starts_with('/') {
        return None;
    }
    let trailing_slash = path.ends_with('/') && path.len() > 1;
    let mut parts: Vec<&str> = Vec::new();
    for part in path.split('/') {
        if part.is_empty() || part == "." {
            continue;
        }
        if part == ".." {
            parts.pop()?;
            continue;
        }
        parts.push(part);
    }
    if parts.is_empty() {
        return Some("/".to_owned());
    }
    let mut out = format!("/{}", parts.join("/"));
    if trailing_slash {
        out.push('/');
    }
    Some(out)
}

/// `Some(normalized)` when `path` lexically stays under `real_root`.
fn stay_under_real_root(path: &str, real_root: &str) -> Option<String> {
    let normalized = lexically_normalize(path)?;
    (normalized == real_root || normalized.starts_with(&format!("{real_root}/")))
        .then_some(normalized)
}

/// `Some(suffix)` when `path` is `prefix` or `prefix/...` (not `prefix-foo`).
fn path_prefix_match<'a>(path: &'a str, prefix: &str) -> Option<&'a str> {
    if path == prefix {
        return Some("");
    }
    let rest = path.strip_prefix(prefix)?;
    rest.starts_with('/').then_some(rest)
}

fn join_root_suffix(root: &str, suffix: &str) -> String {
    if suffix.is_empty() {
        root.to_owned()
    } else if suffix.starts_with('/') {
        format!("{root}{suffix}")
    } else {
        format!("{root}/{suffix}")
    }
}

fn replace_path_prefix<'a>(path: &'a str, from: &str, to: &str) -> Cow<'a, str> {
    match path_prefix_match(path, from) {
        Some(rest) => Cow::Owned(join_root_suffix(to, rest)),
        None => Cow::Borrowed(path),
    }
}

fn is_path_token_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-')
}

/// Replace `from` when it is a path-prefix token (not `/workspace-foo`).
fn replace_path_prefix_in_text<'a>(text: &'a str, from: &str, to: &str) -> Cow<'a, str> {
    if from.is_empty() || !text.contains(from) {
        return Cow::Borrowed(text);
    }
    let bytes = text.as_bytes();
    let mut out: Option<String> = None;
    let mut i = 0;
    while i < bytes.len() {
        if text[i..].starts_with(from) {
            let end = i + from.len();
            let ok_before = i == 0 || !is_path_token_char(bytes[i - 1]);
            let ok_after = end == bytes.len() || !is_path_token_char(bytes[end]);
            if ok_before && ok_after {
                let buf = out.get_or_insert_with(|| text[..i].to_owned());
                buf.push_str(to);
                i = end;
                continue;
            }
        }
        if let Some(buf) = &mut out {
            i += push_char_at(text, i, Some(buf));
        } else {
            i += push_char_at(text, i, None);
        }
    }
    match out {
        Some(s) => Cow::Owned(s),
        None => Cow::Borrowed(text),
    }
}

/// Rewrite each absolute path token through [`PathVirtualization::to_guest`]
/// so a `..` walk-out inside prose is clipped the same way as a lone path.
fn rewrite_text_inbound<'a>(virt: &PathVirtualization, text: &'a str) -> Cow<'a, str> {
    if !text.contains('/') {
        return Cow::Borrowed(text);
    }
    let bytes = text.as_bytes();
    let mut out: Option<String> = None;
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'/' && (i == 0 || !is_path_token_char(bytes[i - 1])) {
            let end = path_token_end(bytes, i);
            let token = &text[i..end];
            match virt.to_guest(token) {
                Cow::Borrowed(_) => {
                    if let Some(buf) = &mut out {
                        buf.push_str(token);
                    }
                }
                Cow::Owned(mapped) => {
                    out.get_or_insert_with(|| text[..i].to_owned())
                        .push_str(&mapped);
                }
            }
            i = end;
            continue;
        }
        if let Some(buf) = &mut out {
            i += push_char_at(text, i, Some(buf));
        } else {
            i += push_char_at(text, i, None);
        }
    }
    match out {
        Some(s) => Cow::Owned(s),
        None => Cow::Borrowed(text),
    }
}

/// Advance one UTF-8 scalar at `i`. `i` is a char boundary because the
/// scanners start at 0 and step by `char::len_utf8`.
fn push_char_at(text: &str, i: usize, buf: Option<&mut String>) -> usize {
    let Some(ch) = text.get(i..).and_then(|s| s.chars().next()) else {
        return 1;
    };
    if let Some(buf) = buf {
        buf.push(ch);
    }
    ch.len_utf8()
}

fn path_token_end(bytes: &[u8], start: usize) -> usize {
    let mut end = start + 1;
    while end < bytes.len() && (bytes[end] == b'/' || is_path_token_char(bytes[end])) {
        end += 1;
    }
    end
}

fn rewrite_json_strings(value: Value, rewrite: &dyn Fn(&str) -> Cow<str>) -> Value {
    match value {
        Value::String(s) => match rewrite(&s) {
            Cow::Borrowed(_) => Value::String(s),
            Cow::Owned(owned) => Value::String(owned),
        },
        Value::Array(items) => Value::Array(
            items
                .into_iter()
                .map(|v| rewrite_json_strings(v, rewrite))
                .collect(),
        ),
        Value::Object(map) => Value::Object(
            map.into_iter()
                .map(|(k, v)| (k, rewrite_json_strings(v, rewrite)))
                .collect(),
        ),
        other => other,
    }
}

/// Write/edit bodies and search/monitor regexes mention `/workspace` as
/// file content, not as a path argument. Rewriting them would persist the
/// guest root on disk, break `search_replace` against the original bytes,
/// and make grep / `notify_on_output` miss on-disk text that still uses
/// the visible root.
fn is_content_like_json_key(key: &str) -> bool {
    matches!(
        key,
        "contents" | "old_string" | "new_string" | "patch" | "pattern"
    )
}

fn rewrite_json_inbound_value(value: Value, virt: &PathVirtualization) -> Value {
    match value {
        Value::String(s) => match virt.rewrite_text_inbound(&s) {
            Cow::Borrowed(_) => Value::String(s),
            Cow::Owned(owned) => Value::String(owned),
        },
        Value::Array(items) => Value::Array(
            items
                .into_iter()
                .map(|v| rewrite_json_inbound_value(v, virt))
                .collect(),
        ),
        Value::Object(map) => Value::Object(
            map.into_iter()
                .map(|(k, v)| {
                    if is_content_like_json_key(&k) {
                        (k, v)
                    } else {
                        (k, rewrite_json_inbound_value(v, virt))
                    }
                })
                .collect(),
        ),
        other => other,
    }
}

fn rewrite_content_block(block: &mut ContentBlock, rewrite: impl Fn(&str) -> Cow<str>) {
    match block {
        ContentBlock::Text { text } => {
            if let Cow::Owned(owned) = rewrite(text) {
                *text = owned;
            }
        }
        ContentBlock::Image { path, .. } => {
            if let Some(p) = path
                && let Cow::Owned(owned) = rewrite(p)
            {
                *p = owned;
            }
        }
        ContentBlock::Resource { uri, text, .. } => {
            if let Cow::Owned(owned) = rewrite(uri) {
                *uri = owned;
            }
            if let Some(t) = text
                && let Cow::Owned(owned) = rewrite(t)
            {
                *t = owned;
            }
        }
    }
}

fn rewrite_chat_completion_output(
    cco: &mut ToolChatCompletionResponse,
    rewrite: &dyn Fn(&str) -> Cow<str>,
) {
    if let Some(result) = cco.result.as_mut() {
        if let Cow::Owned(owned) = rewrite(&result.message) {
            result.message = owned;
        }
        if let Some(cer) = result.code_execution_result.as_mut() {
            if let Cow::Owned(owned) = rewrite(&cer.stdout) {
                cer.stdout = owned;
            }
            if let Cow::Owned(owned) = rewrite(&cer.stderr) {
                cer.stderr = owned;
            }
        }
        if let Some(card) = result.card_attachment.as_mut()
            && let Cow::Owned(owned) = rewrite(card)
        {
            *card = owned;
        }
        if !result.extra.is_empty() {
            let rewritten =
                rewrite_json_strings(Value::Object(std::mem::take(&mut result.extra)), rewrite);
            if let Value::Object(map) = rewritten {
                result.extra = map;
            }
        }
    }
    if let Some(err) = cco.stream_error.as_mut() {
        if let Cow::Owned(owned) = rewrite(&err.message) {
            err.message = owned;
        }
        if let Some(typed) = err.typed_error.take() {
            err.typed_error = Some(rewrite_json_strings(typed, rewrite));
        }
    }
}

// ---------------------------------------------------------------------------
// Bind-time mount hook (probe-then-mount; unbind must not unmount)
// ---------------------------------------------------------------------------

/// Context passed to bind/unbind lifecycle hooks.
#[derive(Debug, Clone, Copy)]
pub struct BindLifecycleCtx<'a> {
    pub session_id: &'a str,
    pub real_root: &'a Path,
}

/// Error from a configured bind-time mount command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BindMountError(pub String);

impl std::fmt::Display for BindMountError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "bind mount failed: {}", self.0)
    }
}

impl std::error::Error for BindMountError {}

type ProbeFn = Arc<dyn Fn(&Path) -> bool + Send + Sync>;
type MountFn = Arc<dyn Fn(&Path) -> Result<(), BindMountError> + Send + Sync>;
type UnbindFn = Arc<dyn Fn(&str, &Path) + Send + Sync>;

/// Probe-then-mount seam. Default is a no-op until a command is configured;
/// `on_unbind` must not unmount (the guest mount outlives the hub session).
pub struct BindMountHook {
    probe: Option<ProbeFn>,
    mount: Option<MountFn>,
    on_unbind: Option<UnbindFn>,
}

impl std::fmt::Debug for BindMountHook {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BindMountHook")
            .field("probe", &self.probe.is_some())
            .field("mount", &self.mount.is_some())
            .field("on_unbind", &self.on_unbind.is_some())
            .finish()
    }
}

impl Default for BindMountHook {
    fn default() -> Self {
        Self::noop()
    }
}

impl BindMountHook {
    /// No command configured: bind and unbind are no-ops.
    pub fn noop() -> Self {
        Self {
            probe: None,
            mount: None,
            on_unbind: None,
        }
    }

    /// Probe-then-mount. `probe` true means a live mount (skip `mount`).
    /// Unbind still does not unmount.
    pub fn probe_then_mount(
        probe: impl Fn(&Path) -> bool + Send + Sync + 'static,
        mount: impl Fn(&Path) -> Result<(), BindMountError> + Send + Sync + 'static,
    ) -> Self {
        Self {
            probe: Some(Arc::new(probe)),
            mount: Some(Arc::new(mount)),
            on_unbind: None,
        }
    }

    pub fn with_on_unbind(
        mut self,
        on_unbind: impl Fn(&str, &Path) + Send + Sync + 'static,
    ) -> Self {
        self.on_unbind = Some(Arc::new(on_unbind));
        self
    }

    /// Probe, then mount on a miss. No-op when no mount command is set.
    pub fn on_bind(&self, ctx: BindLifecycleCtx<'_>) -> Result<(), BindMountError> {
        let Some(mount) = &self.mount else {
            return Ok(());
        };
        if self
            .probe
            .as_ref()
            .is_some_and(|probe| probe(ctx.real_root))
        {
            return Ok(());
        }
        mount(ctx.real_root)
    }

    /// Session teardown notification. Must not unmount.
    pub fn on_unbind(&self, ctx: BindLifecycleCtx<'_>) {
        if let Some(cb) = &self.on_unbind {
            cb(ctx.session_id, ctx.real_root);
        }
    }
}

#[cfg(test)]
#[path = "path_virtualization_tests.rs"]
mod tests;
