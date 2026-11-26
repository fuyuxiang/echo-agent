use crate::error::MermaidError;
use crate::theme::MermaidTheme;

use std::collections::BTreeMap;

const SEQUENCE_EVENT_ROW_HEIGHT: f64 = 37.0;
const SEQUENCE_FRAGMENT_HEADER_HEIGHT: f64 = 28.0;
const SEQUENCE_FRAGMENT_FOOTER_HEIGHT: f64 = 12.0;
const SEQUENCE_FRAGMENT_INSET_X: f64 = 18.0;
const SEQUENCE_FRAGMENT_MARGIN_X: f64 = 20.0;
const SEQUENCE_FRAGMENT_TAB_PADDING_X: f64 = 10.0;
const SEQUENCE_FRAGMENT_STROKE: &str = "#d7c8f8";
const SEQUENCE_FRAGMENT_TEXT: &str = "#4c3f6f";
const SEQUENCE_ACTIVATION_W: f64 = 8.0;
const SEQUENCE_ACTIVATION_NEST_OFFSET_X: f64 = 4.0;

pub fn render_sequence_diagram_to_svg(
    mermaid_source: &str,
    theme: &MermaidTheme,
) -> Result<String, MermaidError> {
    let diagram = parse_sequence_diagram(mermaid_source)?;

    let title_h = if diagram.title.is_some() {
        26.0_f64
    } else {
        0.0
    };
    let header_y = 16.0_f64 + title_h;
    let box_margin = 10.0_f64;
    let edge_pad = 10.0_f64;

    let box_w = diagram
        .participants
        .iter()
        .map(|p| estimate_label_box_width(&p.label))
        .fold(100.0_f64, f64::max);
    let left_margin = box_w / 2.0 + edge_pad;

    let n = diagram.participants.len().max(1);
    let pair_spacings = compute_sequence_pair_spacings(&diagram, box_w);
    let mut participant_xs = Vec::with_capacity(n);
    let mut next_x = left_margin;
    participant_xs.push(next_x);
    for spacing in pair_spacings {
        next_x += spacing;
        participant_xs.push(next_x);
    }
    let mut width =
        (participant_xs.last().copied().unwrap_or(left_margin) + box_w / 2.0 + edge_pad).max(360.0);
    width = width.max(required_sequence_width(
        &diagram,
        &participant_xs,
        box_w,
        edge_pad,
    ));
    if let Some(title) = &diagram.title {
        width = width.max(estimate_sequence_text_width(title) * 1.25 + edge_pad * 2.0);
    }
    let max_label_lines = diagram
        .participants
        .iter()
        .map(|p| p.label.split("<br/>").count())
        .max()
        .unwrap_or(1);
    let header_h = if max_label_lines > 1 {
        44.0_f64
    } else {
        32.0_f64
    };

    let events_top = header_y + header_h + 28.0;
    let (event_y_positions, fragment_layouts, activation_layouts, content_bottom) =
        layout_sequence_events(&diagram.events, events_top);
    let footer_h = header_h;
    let footer_y = content_bottom + box_margin;
    let height = (footer_y + footer_h + header_y).max(220.0);

    let mut x_for: BTreeMap<&str, f64> = BTreeMap::new();
    for (participant, x) in diagram.participants.iter().zip(participant_xs.iter()) {
        x_for.insert(participant.id.as_str(), *x);
    }

    let mut svg = String::new();

    svg.push_str(&format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 {width} {height}\">"
    ));
    svg.push_str(&format!(
        "<rect x=\"0\" y=\"0\" width=\"{width}\" height=\"{height}\" fill=\"{}\"/>",
        theme.background
    ));

    if let Some(title) = &diagram.title {
        svg.push_str(&format!(
            "<text x=\"{tx:.3}\" y=\"20\" text-anchor=\"middle\" font-family=\"Trebuchet MS,Verdana,Arial,sans-serif\" font-size=\"13\" font-weight=\"bold\" fill=\"{}\">{}</text>",
            theme.text_color,
            escape_xml(title),
            tx = width / 2.0,
        ));
    }

    svg.push_str(&format!(
        "<defs>\
<marker id=\"seq_arrow\" markerWidth=\"8\" markerHeight=\"8\" refX=\"7\" refY=\"4\" orient=\"auto\"><path d=\"M0,0 L8,4 L0,8 Z\" fill=\"{ec}\"/></marker>\
<marker id=\"seq_arrow_rev\" markerWidth=\"8\" markerHeight=\"8\" refX=\"1\" refY=\"4\" orient=\"auto\"><path d=\"M8,0 L0,4 L8,8 Z\" fill=\"{ec}\"/></marker>\
<marker id=\"seq_cross\" markerWidth=\"11\" markerHeight=\"11\" refX=\"5\" refY=\"5\" orient=\"auto\"><path d=\"M1,1 L9,9 M9,1 L1,9\" stroke=\"{ec}\" stroke-width=\"1.5\" fill=\"none\"/></marker>\
<marker id=\"seq_open\" markerWidth=\"10\" markerHeight=\"10\" refX=\"7\" refY=\"4\" orient=\"auto\"><path d=\"M0,0 L8,4 L0,8\" stroke=\"{ec}\" stroke-width=\"1.2\" fill=\"none\"/></marker>\
</defs>",
        ec = theme.edge_color
    ));

    for participant in &diagram.participants {
        let x = x_for
            .get(participant.id.as_str())
            .copied()
            .unwrap_or(left_margin);
        let box_x = x - box_w / 2.0;

        svg.push_str(&format!(
            "<rect x=\"{box_x:.3}\" y=\"{header_y:.3}\" width=\"{box_w:.3}\" height=\"{header_h:.3}\" rx=\"3\" ry=\"3\" fill=\"{}\" stroke=\"{}\" stroke-width=\"1\"/>",
            theme.node_fill, theme.node_stroke
        ));
        svg.push_str(&render_participant_label(
            x,
            header_y + header_h / 2.0,
            &participant.label,
            &theme.text_color,
        ));

        let y0 = header_y + header_h;
        let y1 = footer_y;
        svg.push_str(&format!(
            "<line x1=\"{x:.3}\" y1=\"{y0:.3}\" x2=\"{x:.3}\" y2=\"{y1:.3}\" stroke=\"{}\" stroke-width=\"0.5\"/>",
            theme.edge_color
        ));

        svg.push_str(&format!(
            "<rect x=\"{box_x:.3}\" y=\"{footer_y:.3}\" width=\"{box_w:.3}\" height=\"{footer_h:.3}\" rx=\"3\" ry=\"3\" fill=\"{}\" stroke=\"{}\" stroke-width=\"1\"/>",
            theme.node_fill, theme.node_stroke
        ));
        svg.push_str(&render_participant_label(
            x,
            footer_y + footer_h / 2.0,
            &participant.label,
            &theme.text_color,
        ));
    }

    for bar in &activation_layouts {
        let Some(x) = x_for.get(bar.participant.as_str()).copied() else {
            continue;
        };
        let bx =
            x - SEQUENCE_ACTIVATION_W / 2.0 + bar.depth as f64 * SEQUENCE_ACTIVATION_NEST_OFFSET_X;
        svg.push_str(&format!(
            "<rect x=\"{bx:.3}\" y=\"{y:.3}\" width=\"{SEQUENCE_ACTIVATION_W:.3}\" height=\"{h:.3}\" fill=\"{}\" stroke=\"{}\" stroke-width=\"0.8\"/>",
            theme.node_fill,
            theme.node_stroke,
            y = bar.start_y,
            h = bar.end_y - bar.start_y,
        ));
    }

    if let Some((min_participant_x, max_participant_x)) = participant_span(&diagram, &x_for) {
        for fragment in &fragment_layouts {
            render_fragment(
                &mut svg,
                fragment,
                min_participant_x,
                max_participant_x,
                theme,
            );
        }
    }

    for (idx, ev) in diagram.events.iter().enumerate() {
        let Some(y) = event_y_positions.get(idx).copied().flatten() else {
            continue;
        };

        match ev {
            SequenceEvent::Message {
                from,
                to,
                text,
                dashed,
                head,
                bidirectional,
            } => {
                let x1 = x_for.get(from.as_str()).copied().unwrap_or(left_margin);
                let x2 = x_for.get(to.as_str()).copied().unwrap_or(left_margin);

                let dash = if *dashed {
                    " stroke-dasharray=\"5,4\""
                } else {
                    ""
                };
                let marker_end = match head {
                    SequenceArrowHead::Filled => " marker-end=\"url(#seq_arrow)\"",
                    SequenceArrowHead::Cross => " marker-end=\"url(#seq_cross)\"",
                    SequenceArrowHead::Open => " marker-end=\"url(#seq_open)\"",
                    SequenceArrowHead::None => "",
                };
                let marker_start = if *bidirectional {
                    " marker-start=\"url(#seq_arrow_rev)\""
                } else {
                    ""
                };

                if (x1 - x2).abs() < 1.0 {
                    let loop_w = 26.0_f64;
                    let loop_h = SEQUENCE_EVENT_ROW_HEIGHT * 0.42;
                    let xr = x1 + loop_w;
                    let ye = y + loop_h;
                    svg.push_str(&format!(
                        "<path d=\"M {x1:.3},{y:.3} C {xr:.3},{y:.3} {xr:.3},{ye:.3} {x1:.3},{ye:.3}\" stroke=\"{}\" stroke-width=\"1.5\" fill=\"none\"{dash}{marker_end}/>",
                        theme.edge_color
                    ));
                    if !text.is_empty() {
                        svg.push_str(&format!(
                            "<text x=\"{tx:.3}\" y=\"{ty:.3}\" text-anchor=\"start\" font-family=\"Trebuchet MS,Verdana,Arial,sans-serif\" font-size=\"11\" fill=\"{}\">{}</text>",
                            theme.text_color,
                            escape_xml(text),
                            tx = x1 + 4.0,
                            ty = y - 6.0
                        ));
                    }
                } else {
                    svg.push_str(&format!(
                        "<line x1=\"{x1:.3}\" y1=\"{y:.3}\" x2=\"{x2:.3}\" y2=\"{y:.3}\" stroke=\"{}\" stroke-width=\"1.5\"{marker_end}{marker_start}{dash}/>",
                        theme.edge_color
                    ));
                    if !text.is_empty() {
                        let mx = (x1 + x2) / 2.0;
                        svg.push_str(&format!(
                            "<text x=\"{mx:.3}\" y=\"{ty:.3}\" text-anchor=\"middle\" font-family=\"Trebuchet MS,Verdana,Arial,sans-serif\" font-size=\"11\" fill=\"{}\">{}</text>",
                            theme.text_color,
                            escape_xml(text),
                            ty = y - 6.0
                        ));
                    }
                }
            }
            SequenceEvent::Note { placement, text } => match placement {
                SequenceNotePlacement::Over { from, to } => {
                    let x1 = x_for.get(from.as_str()).copied().unwrap_or(left_margin);
                    let x2 = x_for.get(to.as_str()).copied().unwrap_or(left_margin);

                    let (lx, rx) = if x1 <= x2 { (x1, x2) } else { (x2, x1) };
                    let pad = 50.0_f64;
                    let note_x = (lx - pad).max(8.0);
                    let note_w = (rx - lx + pad * 2.0).max(120.0);
                    render_note(&mut svg, note_x, y, note_w, text, theme);
                }
                SequenceNotePlacement::Beside { participant, side } => {
                    let participant_x = x_for
                        .get(participant.as_str())
                        .copied()
                        .unwrap_or(left_margin);
                    let note_w = estimate_note_width(text).min(380.0);
                    let note_x = match side {
                        SequenceNoteSide::Left => (participant_x - note_w - 12.0).max(8.0),
                        SequenceNoteSide::Right => {
                            (participant_x + 12.0).min((width - note_w - 8.0).max(8.0))
                        }
                    };
                    render_note(&mut svg, note_x, y, note_w, text, theme);
                }
            },
            SequenceEvent::Activate { .. }
            | SequenceEvent::Deactivate { .. }
            | SequenceEvent::FragmentStart { .. }
            | SequenceEvent::FragmentElse { .. }
            | SequenceEvent::FragmentEnd => {}
        }
    }

    svg.push_str("</svg>");
    Ok(svg)
}

#[derive(Debug, Clone)]
struct SequenceParticipant {
    id: String,
    label: String,
    aliases: Vec<String>,
}

#[derive(Debug, Clone)]
struct SequenceDiagram {
    participants: Vec<SequenceParticipant>,
    events: Vec<SequenceEvent>,
    title: Option<String>,
}

impl SequenceParticipant {
    fn matches(&self, reference: &str) -> bool {
        self.id == reference
            || self.label == reference
            || self.aliases.iter().any(|alias| alias == reference)
    }
}

#[derive(Debug, Clone, Copy)]
enum SequenceFragmentKind {
    Alt,
    Loop,
    Opt,
    Par,
    Critical,
    Break,
    Rect,
}

impl SequenceFragmentKind {
    fn tab_label(self) -> Option<&'static str> {
        match self {
            Self::Alt => Some("alt"),
            Self::Loop => Some("loop"),
            Self::Opt => Some("opt"),
            Self::Par => Some("par"),
            Self::Critical => Some("critical"),
            Self::Break => Some("break"),
            Self::Rect => None,
        }
    }
}

#[derive(Debug, Clone)]
enum SequenceNoteSide {
    Left,
    Right,
}

#[derive(Debug, Clone)]
enum SequenceNotePlacement {
    Over {
        from: String,
        to: String,
    },
    Beside {
        participant: String,
        side: SequenceNoteSide,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SequenceArrowHead {
    Filled,
    Cross,
    Open,
    None,
}

#[derive(Debug, Clone)]
enum SequenceEvent {
    Message {
        from: String,
        to: String,
        text: String,
        dashed: bool,
        head: SequenceArrowHead,
        bidirectional: bool,
    },
    Note {
        placement: SequenceNotePlacement,
        text: String,
    },
    Activate {
        participant: String,
    },
    Deactivate {
        participant: String,
    },
    FragmentStart {
        kind: SequenceFragmentKind,
        label: String,
    },
    FragmentElse {
        label: String,
    },
    FragmentEnd,
}

#[derive(Debug, Clone)]
struct SequenceFragmentLayout {
    kind: SequenceFragmentKind,
    label: String,
    depth: usize,
    start_y: f64,
    end_y: f64,
    else_markers: Vec<SequenceElseMarker>,
}

#[derive(Debug, Clone)]
struct SequenceElseMarker {
    separator_y: f64,
    label: String,
}

struct OpenSequenceFragment {
    kind: SequenceFragmentKind,
    label: String,
    depth: usize,
    start_y: f64,
    else_markers: Vec<SequenceElseMarker>,
}

#[derive(Debug, Clone)]
struct SequenceActivationLayout {
    participant: String,
    depth: usize,
    start_y: f64,
    end_y: f64,
}

struct AutoNumber {
    active: bool,
    next: u64,
    step: u64,
}

impl Default for AutoNumber {
    fn default() -> Self {
        Self {
            active: false,
            next: 1,
            step: 1,
        }
    }
}

impl AutoNumber {
    fn apply(&mut self, rest: &str) {
        let mut tokens = rest.split_whitespace();
        match tokens.next() {
            None => self.active = true,
            Some(t) if t.eq_ignore_ascii_case("off") => self.active = false,
            Some(t) => {
                self.active = true;
                if let Ok(start) = t.parse::<u64>() {
                    self.next = start;
                }
                if let Some(step) = tokens.next().and_then(|s| s.parse::<u64>().ok()) {
                    self.step = step;
                }
            }
        }
    }

    fn number(&mut self, text: String) -> String {
        if !self.active {
            return text;
        }
        let n = self.next;
        self.next = self.next.saturating_add(self.step);
        if text.is_empty() {
            format!("{n}.")
        } else {
            format!("{n}. {text}")
        }
    }
}

fn strip_keyword_ci<'a>(line: &'a str, kw: &str) -> Option<&'a str> {
    let head = line.get(..kw.len())?;
    if !head.eq_ignore_ascii_case(kw) {
        return None;
    }
    let rest = &line[kw.len()..];
    if rest.is_empty() {
        return Some(rest);
    }
    if !rest.starts_with([' ', '\t']) {
        return None;
    }
    Some(rest.trim_start())
}

fn parse_sequence_diagram(input: &str) -> Result<SequenceDiagram, MermaidError> {
    let lines: Vec<&str> = input.lines().collect();

    let mut i = 0_usize;
    while i < lines.len() {
        let line = lines[i].trim();
        if line.is_empty() || line.starts_with("%%") {
            i += 1;
            continue;
        }

        if line.split_whitespace().next() == Some("sequenceDiagram") {
            i += 1;
            break;
        }

        return Err(MermaidError::ParseError {
            line: i + 1,
            message: "Expected 'sequenceDiagram' declaration".to_string(),
        });
    }

    let mut participants: Vec<SequenceParticipant> = Vec::new();
    let mut events: Vec<SequenceEvent> = Vec::new();
    let mut title: Option<String> = None;
    let mut autonumber = AutoNumber::default();
    let mut in_box = false;
    let mut open_fragment_depth: usize = 0;
    let mut in_acc_block = false;

    while i < lines.len() {
        let raw = lines[i];
        let line = raw.trim();
        let line_no = i + 1;
        i += 1;

        if line.is_empty() || line.starts_with("%%") {
            continue;
        }

        if in_acc_block {
            if line.contains('}') {
                in_acc_block = false;
            }
            continue;
        }

        if let Some(rest) =
            strip_keyword_ci(line, "participant").or_else(|| strip_keyword_ci(line, "actor"))
        {
            let participant = parse_participant_declaration(rest, line_no)?;
            register_participant(&mut participants, participant);
            continue;
        }

        if let Some(rest) = strip_keyword_ci(line, "create") {
            let decl = strip_keyword_ci(rest, "participant")
                .or_else(|| strip_keyword_ci(rest, "actor"))
                .unwrap_or(rest);
            let participant = parse_participant_declaration(decl, line_no)?;
            register_participant(&mut participants, participant);
            continue;
        }
        if let Some(rest) = strip_keyword_ci(line, "destroy") {
            if !rest.is_empty() {
                resolve_participant_ref(&mut participants, rest);
                continue;
            }
        }

        if strip_keyword_ci(line, "box").is_some() {
            in_box = true;
            continue;
        }
        if in_box && open_fragment_depth == 0 && line.eq_ignore_ascii_case("end") {
            in_box = false;
            continue;
        }

        if let Some(rest) = strip_keyword_ci(line, "note") {
            events.push(parse_note_line(rest, line, line_no, &mut participants)?);
            continue;
        }

        if let Some(rest) = strip_keyword_ci(line, "activate") {
            if !rest.is_empty() {
                let participant = resolve_participant_ref(&mut participants, rest);
                events.push(SequenceEvent::Activate { participant });
                continue;
            }
        }
        if let Some(rest) = strip_keyword_ci(line, "deactivate") {
            if !rest.is_empty() {
                let participant = resolve_participant_ref(&mut participants, rest);
                events.push(SequenceEvent::Deactivate { participant });
                continue;
            }
        }

        if let Some(rest) = strip_keyword_ci(line, "autonumber") {
            autonumber.apply(rest);
            continue;
        }

        if let Some(rest) = strip_keyword_ci(line, "title") {
            let rest = rest.trim_start_matches(':').trim();
            if !rest.is_empty() {
                title = Some(decode_sequence_text(rest));
            }
            continue;
        }

        let lower = line.to_ascii_lowercase();
        if lower.starts_with("acctitle") || lower.starts_with("accdescr") {
            if lower.contains('{') && !lower.contains('}') {
                in_acc_block = true;
            }
            continue;
        }
        if lower.starts_with("links ")
            || lower.starts_with("link ")
            || lower.starts_with("properties ")
        {
            continue;
        }

        if let Some(msg) = parse_message_line(line) {
            let from = resolve_participant_ref(&mut participants, &msg.from);
            let to = resolve_participant_ref(&mut participants, &msg.to);

            let activate = if msg.activate_target {
                Some(to.clone())
            } else {
                None
            };
            let deactivate = if msg.deactivate_source {
                Some(from.clone())
            } else {
                None
            };

            events.push(SequenceEvent::Message {
                from,
                to,
                text: autonumber.number(msg.text),
                dashed: msg.dashed,
                head: msg.head,
                bidirectional: msg.bidirectional,
            });
            if let Some(participant) = activate {
                events.push(SequenceEvent::Activate { participant });
            }
            if let Some(participant) = deactivate {
                events.push(SequenceEvent::Deactivate { participant });
            }
            continue;
        }

        if let Some(fragment) = parse_fragment_line(line) {
            match &fragment {
                SequenceEvent::FragmentStart { .. } => open_fragment_depth += 1,
                SequenceEvent::FragmentEnd => {
                    open_fragment_depth = open_fragment_depth.saturating_sub(1)
                }
                _ => {}
            }
            events.push(fragment);
            continue;
        }

        return Err(MermaidError::ParseError {
            line: line_no,
            message: format!("Unrecognized sequenceDiagram line: {line}"),
        });
    }

    if participants.is_empty() {
        participants.push(SequenceParticipant {
            id: "Participant".to_string(),
            label: "Participant".to_string(),
            aliases: Vec::new(),
        });
    }

    Ok(SequenceDiagram {
        participants,
        events,
        title,
    })
}

fn parse_note_line(
    rest: &str,
    line: &str,
    line_no: usize,
    participants: &mut Vec<SequenceParticipant>,
) -> Result<SequenceEvent, MermaidError> {
    let invalid = || MermaidError::ParseError {
        line: line_no,
        message: format!("Invalid Note syntax: {line}"),
    };

    let (side, rest) = if let Some(rest) = strip_keyword_ci(rest, "over") {
        (None, rest)
    } else if let Some(rest) = strip_keyword_ci(rest, "right") {
        (
            Some(SequenceNoteSide::Right),
            strip_keyword_ci(rest, "of").ok_or_else(invalid)?,
        )
    } else if let Some(rest) = strip_keyword_ci(rest, "left") {
        (
            Some(SequenceNoteSide::Left),
            strip_keyword_ci(rest, "of").ok_or_else(invalid)?,
        )
    } else {
        return Err(invalid());
    };

    let (who, text) = rest.split_once(':').ok_or_else(invalid)?;
    let who = who.trim();
    let text = decode_sequence_text(text.trim());

    let placement = match side {
        Some(side) => SequenceNotePlacement::Beside {
            participant: resolve_participant_ref(participants, who),
            side,
        },
        None => {
            let (from, to) = match who.split_once(',') {
                Some((a, b)) => (
                    resolve_participant_ref(participants, a.trim()),
                    resolve_participant_ref(participants, b.trim()),
                ),
                None => {
                    let participant = resolve_participant_ref(participants, who);
                    (participant.clone(), participant)
                }
            };
            SequenceNotePlacement::Over { from, to }
        }
    };

    Ok(SequenceEvent::Note { placement, text })
}

struct ParsedMessage {
    from: String,
    to: String,
    text: String,
    dashed: bool,
    head: SequenceArrowHead,
    bidirectional: bool,
    activate_target: bool,
    deactivate_source: bool,
}

fn parse_message_line(line: &str) -> Option<ParsedMessage> {
    let (head_raw, text) = line
        .split_once(':')
        .map_or((line, ""), |(a, b)| (a, b.trim()));

    let head_raw = head_raw.trim();
    const ARROWS: &[(&str, bool, SequenceArrowHead, bool)] = &[
        ("<<-->>", true, SequenceArrowHead::Filled, true),
        ("<<->>", false, SequenceArrowHead::Filled, true),
        ("-->>", true, SequenceArrowHead::Filled, false),
        ("->>", false, SequenceArrowHead::Filled, false),
        ("--x", true, SequenceArrowHead::Cross, false),
        ("-x", false, SequenceArrowHead::Cross, false),
        ("--)", true, SequenceArrowHead::Open, false),
        ("-)", false, SequenceArrowHead::Open, false),
        ("-->", true, SequenceArrowHead::None, false),
        ("->", false, SequenceArrowHead::None, false),
    ];
    let (op, dashed, head, bidirectional) = ARROWS
        .iter()
        .copied()
        .find(|(op, ..)| head_raw.contains(op))?;

    let (from_raw, to_raw) = head_raw.split_once(op)?;
    let from = from_raw.trim().to_string();
    let mut to_raw = to_raw.trim();
    let mut activate_target = false;
    let mut deactivate_source = false;
    if let Some(stripped) = to_raw.strip_prefix('+') {
        activate_target = true;
        to_raw = stripped;
    } else if let Some(stripped) = to_raw.strip_prefix('-') {
        deactivate_source = true;
        to_raw = stripped;
    }

    Some(ParsedMessage {
        from,
        to: to_raw.trim().to_string(),
        text: text.to_string(),
        dashed,
        head,
        bidirectional,
        activate_target,
        deactivate_source,
    })
}

fn parse_fragment_line(line: &str) -> Option<SequenceEvent> {
    let trimmed = line.trim();

    const STARTS: &[(&str, SequenceFragmentKind)] = &[
        ("alt", SequenceFragmentKind::Alt),
        ("loop", SequenceFragmentKind::Loop),
        ("opt", SequenceFragmentKind::Opt),
        ("par", SequenceFragmentKind::Par),
        ("critical", SequenceFragmentKind::Critical),
        ("break", SequenceFragmentKind::Break),
        ("rect", SequenceFragmentKind::Rect),
    ];
    for (kw, kind) in STARTS.iter().copied() {
        if let Some(label) = strip_keyword_ci(trimmed, kw) {
            return Some(SequenceEvent::FragmentStart {
                kind,
                label: label.to_string(),
            });
        }
    }

    for kw in ["else", "and", "option"] {
        if let Some(label) = strip_keyword_ci(trimmed, kw) {
            return Some(SequenceEvent::FragmentElse {
                label: label.to_string(),
            });
        }
    }

    if trimmed.eq_ignore_ascii_case("end") {
        return Some(SequenceEvent::FragmentEnd);
    }

    None
}

fn parse_participant_declaration(
    raw: &str,
    line_no: usize,
) -> Result<SequenceParticipant, MermaidError> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err(MermaidError::ParseError {
            line: line_no,
            message: "Expected participant name".to_string(),
        });
    }

    let (id, label, aliases) = match raw.split_once(" as ") {
        Some((lhs, rhs)) => {
            let lhs = normalize_participant_token(lhs);
            let rhs = normalize_participant_token(rhs);
            if lhs.is_empty() || rhs.is_empty() {
                return Err(MermaidError::ParseError {
                    line: line_no,
                    message: "Expected participant name".to_string(),
                });
            }
            let (id, label) = choose_participant_id_and_label(&lhs, &rhs);
            let mut aliases = Vec::new();
            push_unique_alias(&mut aliases, lhs);
            push_unique_alias(&mut aliases, rhs);
            (id, label, aliases)
        }
        None => {
            let name = normalize_participant_token(raw);
            if name.is_empty() {
                return Err(MermaidError::ParseError {
                    line: line_no,
                    message: "Expected participant name".to_string(),
                });
            }
            (name.clone(), name, Vec::new())
        }
    };

    Ok(SequenceParticipant { id, label, aliases })
}

fn choose_participant_id_and_label(lhs: &str, rhs: &str) -> (String, String) {
    let lhs_has_whitespace = lhs.chars().any(char::is_whitespace);
    let rhs_has_whitespace = rhs.chars().any(char::is_whitespace);

    match (lhs_has_whitespace, rhs_has_whitespace) {
        (true, false) => (rhs.to_string(), lhs.to_string()),
        (false, true) => (lhs.to_string(), rhs.to_string()),
        _ if lhs.len() <= rhs.len() => (lhs.to_string(), rhs.to_string()),
        _ => (rhs.to_string(), lhs.to_string()),
    }
}

fn normalize_participant_token(token: &str) -> String {
    token
        .trim()
        .trim_matches(|ch| matches!(ch, '"' | '\''))
        .trim()
        .to_string()
}

fn register_participant(list: &mut Vec<SequenceParticipant>, participant: SequenceParticipant) {
    let match_index = list.iter().position(|existing| {
        existing.matches(&participant.id)
            || existing.matches(&participant.label)
            || participant
                .aliases
                .iter()
                .any(|alias| existing.matches(alias))
    });

    match match_index {
        Some(index) => {
            let existing = &mut list[index];
            if existing.label == existing.id && participant.label != participant.id {
                existing.label = participant.label.clone();
            }
            push_unique_alias(&mut existing.aliases, participant.id.clone());
            push_unique_alias(&mut existing.aliases, participant.label.clone());
            for alias in participant.aliases {
                push_unique_alias(&mut existing.aliases, alias);
            }
        }
        None => list.push(participant),
    }
}

fn resolve_participant_ref(list: &mut Vec<SequenceParticipant>, reference: &str) -> String {
    let reference = normalize_participant_token(reference);

    if let Some(participant) = list
        .iter()
        .find(|participant| participant.matches(&reference))
    {
        return participant.id.clone();
    }

    let participant = SequenceParticipant {
        id: reference.clone(),
        label: reference,
        aliases: Vec::new(),
    };
    let id = participant.id.clone();
    list.push(participant);
    id
}

fn push_unique_alias(aliases: &mut Vec<String>, value: String) {
    if !value.is_empty() && !aliases.iter().any(|alias| alias == &value) {
        aliases.push(value);
    }
}

fn participant_span(diagram: &SequenceDiagram, x_for: &BTreeMap<&str, f64>) -> Option<(f64, f64)> {
    let mut xs = diagram
        .participants
        .iter()
        .filter_map(|participant| x_for.get(participant.id.as_str()).copied());
    let first = xs.next()?;
    let mut min_x = first;
    let mut max_x = first;
    for x in xs {
        min_x = min_x.min(x);
        max_x = max_x.max(x);
    }
    Some((min_x, max_x))
}

fn layout_sequence_events(
    events: &[SequenceEvent],
    events_top: f64,
) -> (
    Vec<Option<f64>>,
    Vec<SequenceFragmentLayout>,
    Vec<SequenceActivationLayout>,
    f64,
) {
    let mut event_y_positions = vec![None; events.len()];
    let mut fragment_layouts = Vec::new();
    let mut open_fragments: Vec<OpenSequenceFragment> = Vec::new();
    let mut cursor_y = events_top;
    let mut last_row_center: Option<f64> = None;

    for (idx, event) in events.iter().enumerate() {
        match event {
            SequenceEvent::Message { .. } | SequenceEvent::Note { .. } => {
                let center = cursor_y + SEQUENCE_EVENT_ROW_HEIGHT / 2.0;
                event_y_positions[idx] = Some(center);
                last_row_center = Some(center);
                cursor_y += SEQUENCE_EVENT_ROW_HEIGHT;
            }
            SequenceEvent::Activate { .. } | SequenceEvent::Deactivate { .. } => {
                event_y_positions[idx] = Some(last_row_center.unwrap_or(cursor_y));
            }
            SequenceEvent::FragmentStart { kind, label } => {
                open_fragments.push(OpenSequenceFragment {
                    kind: *kind,
                    label: label.clone(),
                    depth: open_fragments.len(),
                    start_y: cursor_y,
                    else_markers: Vec::new(),
                });
                cursor_y += SEQUENCE_FRAGMENT_HEADER_HEIGHT;
            }
            SequenceEvent::FragmentElse { label } => {
                if let Some(fragment) = open_fragments.last_mut() {
                    fragment.else_markers.push(SequenceElseMarker {
                        separator_y: cursor_y,
                        label: label.clone(),
                    });
                }
                cursor_y += SEQUENCE_FRAGMENT_HEADER_HEIGHT;
            }
            SequenceEvent::FragmentEnd => {
                if let Some(fragment) = open_fragments.pop() {
                    fragment_layouts.push(SequenceFragmentLayout {
                        kind: fragment.kind,
                        label: fragment.label,
                        depth: fragment.depth,
                        start_y: fragment.start_y,
                        end_y: cursor_y + SEQUENCE_FRAGMENT_FOOTER_HEIGHT,
                        else_markers: fragment.else_markers,
                    });
                    cursor_y += SEQUENCE_FRAGMENT_FOOTER_HEIGHT;
                }
            }
        }
    }

    while let Some(fragment) = open_fragments.pop() {
        fragment_layouts.push(SequenceFragmentLayout {
            kind: fragment.kind,
            label: fragment.label,
            depth: fragment.depth,
            start_y: fragment.start_y,
            end_y: cursor_y + SEQUENCE_FRAGMENT_FOOTER_HEIGHT,
            else_markers: fragment.else_markers,
        });
        cursor_y += SEQUENCE_FRAGMENT_FOOTER_HEIGHT;
    }

    fragment_layouts.sort_by(|a, b| {
        a.depth
            .cmp(&b.depth)
            .then_with(|| a.start_y.total_cmp(&b.start_y))
    });

    let activations = compute_activation_layouts(events, &event_y_positions, cursor_y);
    (event_y_positions, fragment_layouts, activations, cursor_y)
}

fn compute_activation_layouts(
    events: &[SequenceEvent],
    event_y_positions: &[Option<f64>],
    content_bottom: f64,
) -> Vec<SequenceActivationLayout> {
    const MIN_BAR_H: f64 = 6.0;

    let mut open: Vec<(String, f64, usize)> = Vec::new();
    let mut bars = Vec::new();
    for (idx, event) in events.iter().enumerate() {
        let y = event_y_positions.get(idx).copied().flatten();
        match event {
            SequenceEvent::Activate { participant } => {
                let depth = open.iter().filter(|(p, ..)| p == participant).count();
                open.push((participant.clone(), y.unwrap_or(content_bottom), depth));
            }
            SequenceEvent::Deactivate { participant } => {
                if let Some(pos) = open.iter().rposition(|(p, ..)| p == participant) {
                    let (participant, start_y, depth) = open.remove(pos);
                    let end_y = y.unwrap_or(content_bottom);
                    bars.push(SequenceActivationLayout {
                        participant,
                        depth,
                        start_y,
                        end_y: end_y.max(start_y + MIN_BAR_H),
                    });
                }
            }
            _ => {}
        }
    }
    for (participant, start_y, depth) in open {
        bars.push(SequenceActivationLayout {
            participant,
            depth,
            start_y,
            end_y: content_bottom.max(start_y + MIN_BAR_H),
        });
    }
    bars.sort_by(|a, b| {
        a.depth
            .cmp(&b.depth)
            .then_with(|| a.start_y.total_cmp(&b.start_y))
    });
    bars
}

fn render_fragment(
    svg: &mut String,
    fragment: &SequenceFragmentLayout,
    min_participant_x: f64,
    max_participant_x: f64,
    theme: &MermaidTheme,
) {
    let inset = fragment.depth as f64 * SEQUENCE_FRAGMENT_INSET_X;
    let x = min_participant_x - SEQUENCE_FRAGMENT_MARGIN_X + inset;
    let width =
        (max_participant_x - min_participant_x) + SEQUENCE_FRAGMENT_MARGIN_X * 2.0 - inset * 2.0;
    let height = (fragment.end_y - fragment.start_y).max(SEQUENCE_FRAGMENT_HEADER_HEIGHT);
    let y = fragment.start_y;

    svg.push_str(&format!(
        "<rect x=\"{x:.3}\" y=\"{y:.3}\" width=\"{width:.3}\" height=\"{height:.3}\" fill=\"none\" stroke=\"{SEQUENCE_FRAGMENT_STROKE}\" stroke-width=\"1\" stroke-dasharray=\"3,3\"/>"
    ));

    if let Some(tab_text) = fragment.kind.tab_label() {
        let tab_text_width = crate::text_wrap::display_width_units(tab_text) * 6.5;
        let tab_width = tab_text_width + SEQUENCE_FRAGMENT_TAB_PADDING_X * 2.0 + 10.0;
        let tab_height = 18.0;
        let tab_x = x + 8.0;
        let tab_y = y + 4.0;
        let tab_body_width = (tab_width - 10.0).max(10.0);

        svg.push_str(&format!(
            "<path d=\"M {tab_x:.3},{tab_y:.3} h {tab_body_width:.3} l 10,0 l 0,10 l -10,8 h -{tab_body_width:.3} z\" fill=\"{}\" stroke=\"{SEQUENCE_FRAGMENT_STROKE}\" stroke-width=\"1\"/>",
            theme.node_fill
        ));
        svg.push_str(&format!(
            "<text x=\"{text_x:.3}\" y=\"{text_y:.3}\" text-anchor=\"middle\" dominant-baseline=\"central\" font-family=\"Trebuchet MS,Verdana,Arial,sans-serif\" font-size=\"11\" fill=\"{SEQUENCE_FRAGMENT_TEXT}\">{}</text>",
            escape_xml(tab_text),
            text_x = tab_x + tab_body_width / 2.0,
            text_y = tab_y + tab_height / 2.0
        ));

        if !fragment.label.is_empty() {
            let label_x = (tab_x + tab_body_width + 18.0).min(x + width - 8.0);
            svg.push_str(&format!(
                "<text x=\"{text_x:.3}\" y=\"{text_y:.3}\" text-anchor=\"start\" dominant-baseline=\"central\" font-family=\"Trebuchet MS,Verdana,Arial,sans-serif\" font-size=\"11\" fill=\"{}\">[{}]</text>",
                theme.text_color,
                escape_xml(&fragment.label),
                text_x = label_x,
                text_y = y + SEQUENCE_FRAGMENT_HEADER_HEIGHT / 2.0
            ));
        }
    }

    for else_marker in &fragment.else_markers {
        svg.push_str(&format!(
            "<line x1=\"{x:.3}\" y1=\"{y:.3}\" x2=\"{x2:.3}\" y2=\"{y:.3}\" stroke=\"{SEQUENCE_FRAGMENT_STROKE}\" stroke-width=\"1\" stroke-dasharray=\"3,3\"/>",
            x = x,
            y = else_marker.separator_y,
            x2 = x + width
        ));
        if !else_marker.label.is_empty() {
            svg.push_str(&format!(
                "<text x=\"{text_x:.3}\" y=\"{text_y:.3}\" text-anchor=\"middle\" dominant-baseline=\"central\" font-family=\"Trebuchet MS,Verdana,Arial,sans-serif\" font-size=\"11\" fill=\"{}\">[{}]</text>",
                theme.text_color,
                escape_xml(&else_marker.label),
                text_x = x + width / 2.0,
                text_y = else_marker.separator_y + SEQUENCE_FRAGMENT_HEADER_HEIGHT / 2.0
            ));
        }
    }
}

fn render_note(
    svg: &mut String,
    note_x: f64,
    center_y: f64,
    note_w: f64,
    text: &str,
    theme: &MermaidTheme,
) {
    let note_h = 26.0_f64;
    svg.push_str(&format!(
        "<rect x=\"{note_x:.3}\" y=\"{ny:.3}\" width=\"{note_w:.3}\" height=\"{note_h:.3}\" rx=\"4\" ry=\"4\" fill=\"#fff2b0\" stroke=\"{}\" stroke-width=\"1\"/>",
        theme.edge_color,
        ny = center_y - note_h / 2.0
    ));
    svg.push_str(&format!(
        "<text x=\"{x:.3}\" y=\"{center_y:.3}\" text-anchor=\"middle\" dominant-baseline=\"middle\" font-family=\"Trebuchet MS,Verdana,Arial,sans-serif\" font-size=\"11\" fill=\"#333333\">{}</text>",
        escape_xml(text),
        x = note_x + note_w / 2.0
    ));
}

fn estimate_note_width(text: &str) -> f64 {
    (crate::text_wrap::display_width_units(text) * 6.5 + 18.0).max(120.0)
}
fn compute_sequence_pair_spacings(diagram: &SequenceDiagram, box_w: f64) -> Vec<f64> {
    let mut pair_spacings = vec![box_w + 30.0; diagram.participants.len().saturating_sub(1)];
    for event in &diagram.events {
        let SequenceEvent::Message { from, to, text, .. } = event else {
            continue;
        };
        let (Some(from_idx), Some(to_idx)) = (
            participant_index(diagram, from),
            participant_index(diagram, to),
        ) else {
            continue;
        };
        let a = from_idx.min(to_idx);
        let b = from_idx.max(to_idx);
        if a == b {
            continue;
        }
        let total_gap = (estimate_sequence_text_width(text) * 0.95).max(box_w + 30.0);
        let per_gap = total_gap / (b - a) as f64;
        for spacing in &mut pair_spacings[a..b] {
            *spacing = spacing.max(per_gap);
        }
    }
    pair_spacings
}

fn required_sequence_width(
    diagram: &SequenceDiagram,
    participant_xs: &[f64],
    box_w: f64,
    edge_pad: f64,
) -> f64 {
    let mut width = participant_xs.last().copied().unwrap_or(0.0) + box_w / 2.0 + edge_pad;
    for event in &diagram.events {
        match event {
            SequenceEvent::Message { from, to, text, .. } => {
                let (Some(from_idx), Some(to_idx)) = (
                    participant_index(diagram, from),
                    participant_index(diagram, to),
                ) else {
                    continue;
                };
                let x1 = participant_xs.get(from_idx).copied().unwrap_or(0.0);
                let x2 = participant_xs.get(to_idx).copied().unwrap_or(0.0);
                if from_idx == to_idx {
                    width = width.max(x1 + 50.0 + edge_pad);
                    continue;
                }
                let mx = (x1 + x2) / 2.0;
                width = width.max(mx + estimate_sequence_text_width(text) / 2.0 + edge_pad);
            }
            SequenceEvent::Note { placement, text } => match placement {
                SequenceNotePlacement::Over { from, to } => {
                    let (Some(from_idx), Some(to_idx)) = (
                        participant_index(diagram, from),
                        participant_index(diagram, to),
                    ) else {
                        continue;
                    };
                    let x1 = participant_xs.get(from_idx).copied().unwrap_or(0.0);
                    let x2 = participant_xs.get(to_idx).copied().unwrap_or(0.0);
                    let (lx, rx) = if x1 <= x2 { (x1, x2) } else { (x2, x1) };
                    let note_x = (lx - 50.0).max(8.0);
                    let note_w = (rx - lx + 100.0).max(120.0);
                    width = width.max(note_x + note_w + edge_pad);
                }
                SequenceNotePlacement::Beside { participant, side } => {
                    if !matches!(side, SequenceNoteSide::Right) {
                        continue;
                    }
                    let Some(idx) = participant_index(diagram, participant) else {
                        continue;
                    };
                    let x = participant_xs.get(idx).copied().unwrap_or(0.0);
                    width = width.max(x + 12.0 + estimate_note_width(text).min(380.0) + edge_pad);
                }
            },
            SequenceEvent::Activate { .. }
            | SequenceEvent::Deactivate { .. }
            | SequenceEvent::FragmentStart { .. }
            | SequenceEvent::FragmentElse { .. }
            | SequenceEvent::FragmentEnd => {}
        }
    }
    width
}

fn participant_index(diagram: &SequenceDiagram, id: &str) -> Option<usize> {
    diagram
        .participants
        .iter()
        .position(|participant| participant.id == id)
}

fn estimate_sequence_text_width(text: &str) -> f64 {
    crate::text_wrap::display_width_units(text) * 6.5
}

fn estimate_label_box_width(label: &str) -> f64 {
    let char_w = 7.2_f64;
    let padding = 16.0_f64;
    let text_w = label
        .split("<br/>")
        .map(|line| crate::text_wrap::display_width_units(line) * char_w)
        .fold(0.0_f64, f64::max);
    (text_w + padding).max(100.0)
}

fn render_participant_label(x: f64, cy: f64, label: &str, color: &str) -> String {
    let font = "Trebuchet MS,Verdana,Arial,sans-serif";
    let size = 12_f64;
    let lines: Vec<&str> = label.split("<br/>").collect();

    if lines.len() == 1 {
        return format!(
            "<text x=\"{x:.3}\" y=\"{cy:.3}\" text-anchor=\"middle\" dominant-baseline=\"middle\" font-family=\"{font}\" font-size=\"{size}\" fill=\"{color}\">{}</text>",
            escape_xml(lines[0])
        );
    }

    let line_h = 15.0_f64;
    let total_h = line_h * lines.len() as f64;
    let y0 = cy - total_h / 2.0 + line_h / 2.0;

    let mut out = format!(
        "<text x=\"{x:.3}\" text-anchor=\"middle\" font-family=\"{font}\" font-size=\"{size}\" fill=\"{color}\">"
    );
    for (i, line_text) in lines.iter().enumerate() {
        if i == 0 {
            out.push_str(&format!(
                "<tspan x=\"{x:.3}\" y=\"{y0:.3}\">{}</tspan>",
                escape_xml(line_text)
            ));
        } else {
            out.push_str(&format!(
                "<tspan x=\"{x:.3}\" dy=\"{line_h:.3}\">{}</tspan>",
                escape_xml(line_text)
            ));
        }
    }
    out.push_str("</text>");
    out
}

fn decode_sequence_text(s: &str) -> String {
    s.replace("#59;", ";")
}

fn escape_xml(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}
