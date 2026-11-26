use std::collections::{HashMap, HashSet};
use std::path::Path;

use super::{ManagedConfigError, ManagedConfigRequest, ManagedItem, ManagedItemState};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommentSyntax {
    pub(super) prefix: String,
}

impl CommentSyntax {
    pub fn new(prefix: impl Into<String>) -> Result<Self, ManagedConfigError> {
        let prefix = prefix.into();
        if prefix.is_empty() || prefix.contains(['\r', '\n']) {
            return Err(ManagedConfigError::InvalidRequest(
                "comment prefix must be one non-empty line".to_owned(),
            ));
        }
        Ok(Self { prefix })
    }

    pub fn hash() -> Self {
        Self {
            prefix: "#".to_owned(),
        }
    }
}

pub(super) struct RenderedUpdate {
    pub updated: String,
    pub unmanaged_text: String,
}

pub(super) fn validate_request(request: &ManagedConfigRequest) -> Result<(), ManagedConfigError> {
    validate_name(&request.namespace, "namespace")?;
    validate_name(&request.owned_item_prefix, "owned item prefix")?;
    if request.items.is_empty() {
        return Err(ManagedConfigError::InvalidRequest(
            "at least one managed item is required".to_owned(),
        ));
    }
    let mut names = HashSet::new();
    for item in &request.items {
        validate_name(&item.name, "item name")?;
        if !names.insert(&item.name) {
            return Err(ManagedConfigError::InvalidRequest(format!(
                "duplicate requested item {}",
                item.name
            )));
        }
        if item.body.contains('\r') {
            return Err(ManagedConfigError::InvalidRequest(format!(
                "item {} contains a carriage return",
                item.name
            )));
        }
        if item
            .body
            .lines()
            .any(|line| marker_candidate(line, &request.comments.prefix).is_some())
        {
            return Err(ManagedConfigError::InvalidRequest(format!(
                "item {} contains marker-like content",
                item.name
            )));
        }
    }
    Ok(())
}

fn validate_name(name: &str, label: &str) -> Result<(), ManagedConfigError> {
    if name.is_empty()
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b' '))
    {
        return Err(ManagedConfigError::InvalidRequest(format!(
            "{label} contains unsupported characters"
        )));
    }
    Ok(())
}

pub(super) fn outer_block(
    text: &str,
    namespace: &str,
    owned_item_prefix: &str,
    comments: &CommentSyntax,
    path: &Path,
) -> Result<Option<String>, ManagedConfigError> {
    let parsed = parse_block(text, namespace, owned_item_prefix, comments, path)?;
    Ok(parsed
        .outer_range
        .map(|(start, end)| text[start..end].trim_end_matches(['\r', '\n']).to_owned()))
}

pub(super) fn item_state(
    original: &str,
    namespace: &str,
    owned_item_prefix: &str,
    item: &ManagedItem,
    comments: &CommentSyntax,
    path: &Path,
) -> Result<ManagedItemState, ManagedConfigError> {
    let parsed = parse_block(original, namespace, owned_item_prefix, comments, path)?;
    let Some(range) = parsed.items.get(&item.name) else {
        return Ok(ManagedItemState::Absent);
    };
    let expected = item_section(item, comments, parsed.newline);
    let actual = original[range.start..range.end].trim_end_matches(['\r', '\n']);
    Ok(if actual == expected {
        ManagedItemState::Exact
    } else {
        ManagedItemState::NeedsUpdate
    })
}

pub(super) fn render_update(
    original: &str,
    namespace: &str,
    owned_item_prefix: &str,
    items: &[ManagedItem],
    comments: &CommentSyntax,
    path: &Path,
) -> Result<RenderedUpdate, ManagedConfigError> {
    let initial = parse_block(original, namespace, owned_item_prefix, comments, path)?;
    let unmanaged_text = initial.unmanaged_text(original);
    let mut updated = original.to_owned();
    for item in items {
        let parsed = parse_block(&updated, namespace, owned_item_prefix, comments, path)?;
        let section = item_section(item, comments, parsed.newline);
        updated = if let Some(range) = parsed.items.get(&item.name) {
            let keep_eol = updated[range.start..range.end].ends_with('\n');
            let replacement = if keep_eol {
                format!("{section}{}", parsed.newline.as_str())
            } else {
                section
            };
            replace_range(&updated, range.start, range.end, &replacement)
        } else if let Some(close) = parsed.outer_close {
            let insertion = format!("{section}{}", parsed.newline.as_str());
            replace_range(&updated, close.start, close.start, &insertion)
        } else {
            append_outer(
                &updated,
                namespace,
                &section,
                comments,
                parsed.newline,
                parsed.final_newline,
            )
        };
    }
    parse_block(&updated, namespace, owned_item_prefix, comments, path)?;
    Ok(RenderedUpdate {
        updated,
        unmanaged_text,
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Newline {
    Lf,
    CrLf,
}

impl Newline {
    fn as_str(self) -> &'static str {
        match self {
            Self::Lf => "\n",
            Self::CrLf => "\r\n",
        }
    }
}

#[derive(Clone, Debug)]
struct Line {
    start: usize,
    content_end: usize,
    end: usize,
}

impl Line {
    fn content<'a>(&self, text: &'a str) -> &'a str {
        &text[self.start..self.content_end]
    }
}

#[derive(Clone, Debug)]
struct ItemRange {
    start: usize,
    end: usize,
}

#[derive(Clone, Debug)]
struct ParsedBlock {
    newline: Newline,
    final_newline: bool,
    outer_range: Option<(usize, usize)>,
    outer_close: Option<Line>,
    items: HashMap<String, ItemRange>,
}

impl ParsedBlock {
    fn unmanaged_text(&self, text: &str) -> String {
        let Some((start, end)) = self.outer_range else {
            return text.to_owned();
        };
        let mut unmanaged = String::with_capacity(text.len() - (end - start));
        unmanaged.push_str(&text[..start]);
        unmanaged.push_str(&text[end..]);
        unmanaged
    }
}

fn replace_range(text: &str, start: usize, end: usize, replacement: &str) -> String {
    let mut result = String::with_capacity(text.len() - (end - start) + replacement.len());
    result.push_str(&text[..start]);
    result.push_str(replacement);
    result.push_str(&text[end..]);
    result
}

fn append_outer(
    original: &str,
    namespace: &str,
    section: &str,
    comments: &CommentSyntax,
    newline: Newline,
    final_newline: bool,
) -> String {
    let eol = newline.as_str();
    let block = format!(
        "{} >>> {} >>>{eol}{section}{eol}{} <<< {} <<<",
        comments.prefix, namespace, comments.prefix, namespace
    );
    if original.is_empty() {
        return block;
    }
    if final_newline {
        format!("{original}{block}{eol}")
    } else {
        format!("{original}{eol}{block}")
    }
}

fn item_section(item: &ManagedItem, comments: &CommentSyntax, newline: Newline) -> String {
    let eol = newline.as_str();
    let body = item.body.trim_end_matches('\n').replace('\n', eol);
    format!(
        "{} >>> {} >>>{eol}{body}{eol}{} <<< {} <<<",
        comments.prefix, item.name, comments.prefix, item.name
    )
}

fn parse_block(
    text: &str,
    namespace: &str,
    owned_item_prefix: &str,
    comments: &CommentSyntax,
    path: &Path,
) -> Result<ParsedBlock, ManagedConfigError> {
    let newline = detect_newline(text).map_err(|reason| ManagedConfigError::InvalidMarkers {
        path: path.to_path_buf(),
        reason,
    })?;
    let lines = lines(text);
    let final_newline = text.ends_with('\n');
    let outer_open_text = format!("{} >>> {} >>>", comments.prefix, namespace);
    let outer_close_text = format!("{} <<< {} <<<", comments.prefix, namespace);

    for line in &lines {
        let content = line.content(text);
        let Some(candidate) = marker_candidate(content, &comments.prefix) else {
            continue;
        };
        let owns_marker = candidate.contains(namespace) || candidate.contains(owned_item_prefix);
        if owns_marker
            && content != outer_open_text
            && content != outer_close_text
            && parse_marker(content, &comments.prefix).is_none()
        {
            return Err(ManagedConfigError::InvalidMarkers {
                path: path.to_path_buf(),
                reason: format!("malformed owned marker `{content}`"),
            });
        }
    }

    let opens = lines
        .iter()
        .filter(|line| line.content(text) == outer_open_text)
        .cloned()
        .collect::<Vec<_>>();
    let closes = lines
        .iter()
        .filter(|line| line.content(text) == outer_close_text)
        .cloned()
        .collect::<Vec<_>>();
    if opens.len() != closes.len() || opens.len() > 1 {
        return Err(ManagedConfigError::InvalidMarkers {
            path: path.to_path_buf(),
            reason: "duplicate or unmatched outer markers".to_owned(),
        });
    }
    let (open, close) = match (opens.first(), closes.first()) {
        (None, None) => {
            reject_owned_markers_outside(text, &lines, None, owned_item_prefix, comments, path)?;
            return Ok(ParsedBlock {
                newline,
                final_newline,
                outer_range: None,
                outer_close: None,
                items: HashMap::new(),
            });
        }
        (Some(open), Some(close)) if open.start < close.start => (open.clone(), close.clone()),
        (Some(_), Some(_)) => {
            return Err(ManagedConfigError::InvalidMarkers {
                path: path.to_path_buf(),
                reason: "outer markers are reversed".to_owned(),
            });
        }
        _ => unreachable!("outer marker counts were checked"),
    };

    reject_owned_markers_outside(
        text,
        &lines,
        Some((open.start, close.end)),
        owned_item_prefix,
        comments,
        path,
    )?;

    let mut items = HashMap::new();
    let mut active: Option<(String, Line)> = None;
    for line in lines
        .iter()
        .filter(|line| line.start > open.start && line.start < close.start)
    {
        let content = line.content(text);
        if content.trim().is_empty() && active.is_none() {
            continue;
        }
        let Some((direction, name)) = parse_marker(content, &comments.prefix) else {
            if active.is_none() {
                return Err(ManagedConfigError::InvalidMarkers {
                    path: path.to_path_buf(),
                    reason: "content outside a named item section".to_owned(),
                });
            }
            continue;
        };
        match (direction, active.take()) {
            (MarkerDirection::Open, None)
                if name != namespace && name.starts_with(owned_item_prefix) =>
            {
                active = Some((name, line.clone()));
            }
            (MarkerDirection::Open, None) => {
                return Err(ManagedConfigError::InvalidMarkers {
                    path: path.to_path_buf(),
                    reason: format!("unowned item marker {name} inside managed block"),
                });
            }
            (MarkerDirection::Open, _) => {
                return Err(ManagedConfigError::InvalidMarkers {
                    path: path.to_path_buf(),
                    reason: "nested or duplicate item opening marker".to_owned(),
                });
            }
            (MarkerDirection::Close, Some((open_name, open))) if open_name == name => {
                if items
                    .insert(
                        name.clone(),
                        ItemRange {
                            start: open.start,
                            end: line.end,
                        },
                    )
                    .is_some()
                {
                    return Err(ManagedConfigError::InvalidMarkers {
                        path: path.to_path_buf(),
                        reason: format!("duplicate item section {name}"),
                    });
                }
            }
            (MarkerDirection::Close, Some(_)) => {
                return Err(ManagedConfigError::InvalidMarkers {
                    path: path.to_path_buf(),
                    reason: format!("reversed or mismatched item marker {name}"),
                });
            }
            (MarkerDirection::Close, None) => {
                return Err(ManagedConfigError::InvalidMarkers {
                    path: path.to_path_buf(),
                    reason: format!("unmatched item closing marker {name}"),
                });
            }
        }
    }
    if let Some((name, _)) = active {
        return Err(ManagedConfigError::InvalidMarkers {
            path: path.to_path_buf(),
            reason: format!("unmatched item opening marker {name}"),
        });
    }

    Ok(ParsedBlock {
        newline,
        final_newline,
        outer_range: Some((open.start, close.end)),
        outer_close: Some(close),
        items,
    })
}

fn reject_owned_markers_outside(
    text: &str,
    lines: &[Line],
    outer: Option<(usize, usize)>,
    owned_item_prefix: &str,
    comments: &CommentSyntax,
    path: &Path,
) -> Result<(), ManagedConfigError> {
    for line in lines {
        if outer.is_some_and(|(start, end)| line.start >= start && line.start < end) {
            continue;
        }
        if let Some((_, name)) = parse_marker(line.content(text), &comments.prefix)
            && name.starts_with(owned_item_prefix)
        {
            return Err(ManagedConfigError::InvalidMarkers {
                path: path.to_path_buf(),
                reason: format!("owned item marker {name} appears outside the outer block"),
            });
        }
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MarkerDirection {
    Open,
    Close,
}

fn marker_candidate<'a>(line: &'a str, prefix: &str) -> Option<&'a str> {
    let rest = line.strip_prefix(prefix)?;
    let marker = rest.trim_start_matches([' ', '\t']);
    if marker.len() == rest.len() {
        return None;
    }
    (marker.starts_with(">>>") || marker.starts_with("<<<")).then_some(marker)
}

fn parse_marker(line: &str, prefix: &str) -> Option<(MarkerDirection, String)> {
    marker_candidate(line, prefix)?;
    let open_prefix = format!("{prefix} >>> ");
    if let Some(name) = line
        .strip_prefix(&open_prefix)
        .and_then(|rest| rest.strip_suffix(" >>>"))
        .filter(|name| !name.is_empty())
    {
        return Some((MarkerDirection::Open, name.to_owned()));
    }
    let close_prefix = format!("{prefix} <<< ");
    line.strip_prefix(&close_prefix)
        .and_then(|rest| rest.strip_suffix(" <<<"))
        .filter(|name| !name.is_empty())
        .map(|name| (MarkerDirection::Close, name.to_owned()))
}

fn detect_newline(text: &str) -> Result<Newline, String> {
    let bytes = text.as_bytes();
    let mut saw_lf = false;
    let mut saw_crlf = false;
    for (index, byte) in bytes.iter().enumerate() {
        if *byte == b'\r' && bytes.get(index + 1) != Some(&b'\n') {
            return Err("bare carriage return in config".to_owned());
        }
        if *byte == b'\n' {
            if index > 0 && bytes[index - 1] == b'\r' {
                saw_crlf = true;
            } else {
                saw_lf = true;
            }
        }
    }
    match (saw_lf, saw_crlf) {
        (true, true) => Err("mixed line endings in config".to_owned()),
        (false, true) => Ok(Newline::CrLf),
        _ => Ok(Newline::Lf),
    }
}

fn lines(text: &str) -> Vec<Line> {
    let bytes = text.as_bytes();
    let mut result = Vec::new();
    let mut start = 0;
    for (index, byte) in bytes.iter().enumerate() {
        if *byte == b'\n' {
            let content_end = if index > start && bytes[index - 1] == b'\r' {
                index - 1
            } else {
                index
            };
            result.push(Line {
                start,
                content_end,
                end: index + 1,
            });
            start = index + 1;
        }
    }
    if start < text.len() {
        result.push(Line {
            start,
            content_end: text.len(),
            end: text.len(),
        });
    }
    result
}
