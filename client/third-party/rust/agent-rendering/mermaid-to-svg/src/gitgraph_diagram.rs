use std::collections::HashMap;

use crate::error::MermaidError;
use crate::text_wrap::line_width;
use crate::theme::MermaidTheme;

// Matches Mermaid 11.12.2 gitGraphRenderer.ts constants.
const LAYOUT_OFFSET: f64 = 10.0;
const COMMIT_STEP: f64 = 40.0;
const PX: f64 = 4.0;
const PY: f64 = 2.0;

// Gitgraph-specific char width for "trebuchet ms" at 16px.
// Browser getBBox measures ~10.0 px/char for "main" and ~9.1 for "develop";
// 9.5 is a good average that closes the gap vs the global DEFAULT_CHAR_WIDTH=8.0.
const GITGRAPH_CHAR_WIDTH: f64 = 9.5;

// Branch spacing: 50 + 40 (rotateCommitLabel) = 90.
const BRANCH_Y_GAP: f64 = 90.0;

const COMMIT_RADIUS: f64 = 10.0;
const MERGE_OUTER_RADIUS: f64 = 9.0;
const MERGE_INNER_RADIUS: f64 = 6.0;

const ARROW_STROKE_WIDTH: f64 = 8.0;
const TURN_RADIUS: f64 = 20.0;
const THEME_COLOR_LIMIT: usize = 8;

// Branch label: rect width = bbox.width + 18, x = -(bbox.width + 34).
const BRANCH_LABEL_BG_PADDING: f64 = 18.0;
const BRANCH_LABEL_BG_X_TRANSLATE: f64 = -19.0;
const BRANCH_LABEL_BG_Y: f64 = -1.5;
const BRANCH_LABEL_BG_HEIGHT: f64 = 23.0;

const VIEWBOX_MARGIN: f64 = 8.0;

// Commit label constants from Mermaid 11.12.2.
const COMMIT_LABEL_FONT_SIZE: f64 = 10.0;
const COMMIT_LABEL_RECT_HEIGHT: f64 = 15.0;
const COMMIT_LABEL_RECT_Y_OFFSET: f64 = 13.5;
const COMMIT_LABEL_TEXT_Y_OFFSET: f64 = 25.0;

pub fn render_gitgraph_diagram_to_svg(
    mermaid_source: &str,
    theme: &MermaidTheme,
) -> Result<String, MermaidError> {
    let graph = parse_gitgraph(mermaid_source)?;

    let mut branch_order: Vec<String> = graph.branch_order.clone();
    if branch_order.is_empty() {
        branch_order.push(graph.main_branch.clone());
    }

    // Mermaid 11.12.2 setBranchPosition: pos += 50 + (rotateCommitLabel ? 40 : 0).
    let mut y_for_branch: HashMap<&str, f64> = HashMap::new();
    for (idx, b) in branch_order.iter().enumerate() {
        y_for_branch.insert(b.as_str(), idx as f64 * BRANCH_Y_GAP);
    }

    // Mermaid 11.12.2 drawCommits: pos starts at 0, increments by COMMIT_STEP + LAYOUT_OFFSET.
    // posWithOffset = pos + LAYOUT_OFFSET. So commit x values: 10, 60, 110, 160, ...
    // After last commit, pos increments once more, giving maxPos.
    let num_commits = graph.commits.len();
    let max_pos = if num_commits == 0 {
        0.0
    } else {
        num_commits as f64 * (COMMIT_STEP + LAYOUT_OFFSET)
    };

    // Compute branch label bbox widths (approximation of browser getBBox).
    let bbox_height = 19.0; // Typical text bbox height at 16px.
    let branch_bbox_widths: Vec<f64> = branch_order
        .iter()
        .map(|b| line_width(b, GITGRAPH_CHAR_WIDTH))
        .collect();

    // Compute viewBox bounds from branch labels.
    let mut min_x: f64 = 0.0;
    let mut min_y: f64 = 0.0;

    for (idx, branch) in branch_order.iter().enumerate() {
        let y = y_for_branch.get(branch.as_str()).copied().unwrap_or(0.0);
        let text_w = branch_bbox_widths[idx];
        // Mermaid 11.12.2 drawBranches: bkg rect x = -(bbox.width + 4 + 30),
        // transform = translate(-19, pos - bbox.height/2).
        let bg_x = -(text_w + PX + 30.0);
        let bg_translate_y = y - bbox_height / 2.0;
        let label_left = BRANCH_LABEL_BG_X_TRANSLATE + bg_x;
        let label_top = bg_translate_y + BRANCH_LABEL_BG_Y;
        min_x = min_x.min(label_left);
        min_y = min_y.min(label_top);
    }

    let max_x = max_pos;
    let y_end = (branch_order.len().saturating_sub(1) as f64) * BRANCH_Y_GAP;
    let mut max_y = y_end + COMMIT_RADIUS;

    // Account for commit labels below commits.
    for commit in &graph.commits {
        if commit.kind != CommitKind::Normal {
            continue;
        }
        let Some(y) = y_for_branch.get(commit.branch.as_str()).copied() else {
            continue;
        };
        let label_text = commit_label_text(commit.seq);
        let text_w = line_width(&label_text, GITGRAPH_CHAR_WIDTH) * (COMMIT_LABEL_FONT_SIZE / 16.0);
        let r_y = 10.0 + text_w / 25.0 * 8.5;
        let label_bottom = y + r_y + COMMIT_LABEL_RECT_Y_OFFSET + COMMIT_LABEL_RECT_HEIGHT;
        max_y = max_y.max(label_bottom);
    }

    let vb_x = min_x - VIEWBOX_MARGIN;
    let vb_y = min_y - VIEWBOX_MARGIN;
    let vb_w = (max_x - min_x) + VIEWBOX_MARGIN * 2.0;
    let vb_h = (max_y - min_y) + VIEWBOX_MARGIN * 2.0;

    let mut svg = String::new();
    svg.push_str(&format!(
        "<svg id=\"my-svg\" width=\"100%\" xmlns=\"http://www.w3.org/2000/svg\" xmlns:xlink=\"http://www.w3.org/1999/xlink\" style=\"max-width: {vb_w}px; background-color: {};\" viewBox=\"{vb_x} {vb_y} {vb_w} {vb_h}\" role=\"graphics-document document\" aria-roledescription=\"gitGraph\">",
        theme.background
    ));

    // Emit style block matching Mermaid 11.12.2 CSS.
    emit_style_block(&mut svg, theme);

    svg.push_str("<g/>");
    svg.push_str("<g class=\"commit-bullets\"/>");
    svg.push_str("<g class=\"commit-labels\"/>");

    // --- Branches + labels ---
    svg.push_str("<g>");
    for (idx, branch) in branch_order.iter().enumerate() {
        let y = y_for_branch.get(branch.as_str()).copied().unwrap_or(0.0);

        svg.push_str(&format!(
            "<line x1=\"0\" y1=\"{y}\" x2=\"{max_pos}\" y2=\"{y}\" class=\"branch branch{idx}\"/>",
        ));

        let text_w = branch_bbox_widths[idx];
        // Mermaid 11.12.2: rect x = -(bbox.width + 4 + 30), width = bbox.width + 18,
        // y = -bbox.height/2 + 8, height = bbox.height + 4,
        // transform = translate(-19, pos - bbox.height/2).
        let bg_w = text_w + BRANCH_LABEL_BG_PADDING;
        let bg_x = -(text_w + PX + 30.0);
        let bg_translate_y = y - bbox_height / 2.0;
        svg.push_str(&format!(
            "<rect class=\"branchLabelBkg label{idx}\" rx=\"4\" ry=\"4\" x=\"{bg_x}\" y=\"{BRANCH_LABEL_BG_Y}\" width=\"{bg_w}\" height=\"{BRANCH_LABEL_BG_HEIGHT}\" transform=\"translate({BRANCH_LABEL_BG_X_TRANSLATE}, {bg_translate_y})\"/>",
        ));

        // Mermaid 11.12.2: label translate(-(bbox.width + 14 + 30), pos - bbox.height/2 - 1).
        let label_x = -(text_w + 14.0 + 30.0);
        let label_y = y - bbox_height / 2.0 - 1.0;
        svg.push_str("<g class=\"branchLabel\">");
        svg.push_str(&format!(
            "<g class=\"label branch-label{idx}\" transform=\"translate({label_x}, {label_y})\"><text><tspan xml:space=\"preserve\" dy=\"1em\" x=\"0\" class=\"row\">{}</tspan></text></g>",
            escape_xml(branch)
        ));
        svg.push_str("</g>");
    }
    svg.push_str("</g>");

    // --- Arrows ---
    svg.push_str("<g class=\"commit-arrows\">");
    for commit in &graph.commits {
        let x = commit_x(commit.seq);
        let y = y_for_branch
            .get(commit.branch.as_str())
            .copied()
            .unwrap_or(0.0);

        let commit_branch_idx = branch_order
            .iter()
            .position(|b| b == &commit.branch)
            .unwrap_or(0);

        for parent in &commit.parents {
            let Some(parent_commit) = graph
                .commit_by_id
                .get(parent)
                .and_then(|idx| graph.commits.get(*idx))
            else {
                continue;
            };

            let px = commit_x(parent_commit.seq);
            let py = y_for_branch
                .get(parent_commit.branch.as_str())
                .copied()
                .unwrap_or(0.0);

            let arrow_idx = if commit.kind == CommitKind::Merge {
                branch_order
                    .iter()
                    .position(|b| b == &parent_commit.branch)
                    .unwrap_or(0)
            } else {
                commit_branch_idx
            };

            let class = format!("arrow arrow{arrow_idx}");

            if (py - y).abs() < 0.1 {
                svg.push_str(&format!(
                    "<path d=\"M {px} {py} L {x} {y}\" class=\"{class}\"/>",
                ));
            } else if y > py {
                // Branch down: from parent → vertical → arc → horizontal to commit.
                let bend_y = y - TURN_RADIUS;
                let arc_end_x = px + TURN_RADIUS;
                svg.push_str(&format!(
                    "<path d=\"M {px} {py} L {px} {bend_y} A {TURN_RADIUS} {TURN_RADIUS}, 0, 0, 0, {arc_end_x} {y} L {x} {y}\" class=\"{class}\"/>",
                ));
            } else {
                // Merge up: from parent → horizontal → arc → vertical to commit.
                let bend_x = x - TURN_RADIUS;
                let arc_end_y = py - TURN_RADIUS;
                svg.push_str(&format!(
                    "<path d=\"M {px} {py} L {bend_x} {py} A {TURN_RADIUS} {TURN_RADIUS}, 0, 0, 0, {x} {arc_end_y} L {x} {y}\" class=\"{class}\"/>",
                ));
            }
        }
    }
    svg.push_str("</g>");

    // --- Commit bullets ---
    svg.push_str("<g class=\"commit-bullets\">");
    for commit in &graph.commits {
        let x = commit_x(commit.seq);
        let y = y_for_branch
            .get(commit.branch.as_str())
            .copied()
            .unwrap_or(0.0);

        let branch_idx = branch_order
            .iter()
            .position(|b| b == &commit.branch)
            .unwrap_or(0);

        let id_class = commit_id_class(commit.seq);

        match commit.kind {
            CommitKind::Merge => {
                svg.push_str(&format!(
                    "<circle cx=\"{x}\" cy=\"{y}\" r=\"{MERGE_OUTER_RADIUS}\" class=\"commit {id_class} commit{branch_idx}\"/>",
                ));
                svg.push_str(&format!(
                    "<circle cx=\"{x}\" cy=\"{y}\" r=\"{MERGE_INNER_RADIUS}\" class=\"commit commit-merge {id_class} commit{branch_idx}\"/>",
                ));
            }
            CommitKind::Normal => {
                svg.push_str(&format!(
                    "<circle cx=\"{x}\" cy=\"{y}\" r=\"{COMMIT_RADIUS}\" class=\"commit {id_class} commit{branch_idx}\"/>",
                ));
            }
        }
    }
    svg.push_str("</g>");

    // --- Commit labels ---
    svg.push_str("<g class=\"commit-labels\">");
    for commit in &graph.commits {
        if commit.kind != CommitKind::Normal {
            continue;
        }

        let x = commit_x(commit.seq);
        let pos = commit.seq as f64 * (COMMIT_STEP + LAYOUT_OFFSET);
        let Some(y) = y_for_branch.get(commit.branch.as_str()).copied() else {
            continue;
        };

        let label = commit_label_text(commit.seq);

        // Approximate bbox of commit label text at font-size 10px.
        let text_w = line_width(&label, GITGRAPH_CHAR_WIDTH) * (COMMIT_LABEL_FONT_SIZE / 16.0);
        let pos_with_offset = x;

        // Mermaid 11.12.2: rect x = posWithOffset - bbox.width/2 - PY,
        // rect width = bbox.width + 2*PY, rect height = bbox.height + 2*PY.
        let rect_w = text_w + 2.0 * PY;
        let rect_x = pos_with_offset - text_w / 2.0 - PY;
        let rect_y = y + COMMIT_LABEL_RECT_Y_OFFSET;
        let text_x = pos_with_offset - text_w / 2.0;
        let text_y = y + COMMIT_LABEL_TEXT_Y_OFFSET;

        // Mermaid 11.12.2: r_x = -7.5 - (bbox.width + 10) / 25 * 9.5,
        // r_y = 10 + bbox.width / 25 * 8.5,
        // wrapper transform = translate(r_x, r_y) rotate(-45, pos, y).
        let r_x = -7.5 - (text_w + 10.0) / 25.0 * 9.5;
        let r_y = 10.0 + text_w / 25.0 * 8.5;

        svg.push_str(&format!(
            "<g transform=\"translate({r_x}, {r_y}) rotate(-45, {pos}, {y})\">",
        ));
        svg.push_str(&format!(
            "<rect class=\"commit-label-bkg\" x=\"{rect_x}\" y=\"{rect_y}\" width=\"{rect_w}\" height=\"{COMMIT_LABEL_RECT_HEIGHT}\"/>",
        ));
        svg.push_str(&format!(
            "<text x=\"{text_x}\" y=\"{text_y}\" class=\"commit-label\">{}</text>",
            escape_xml(&label)
        ));
        svg.push_str("</g>");
    }
    svg.push_str("</g>");

    svg.push_str("</svg>");

    Ok(svg)
}

/// Compute the x position of a commit (posWithOffset in Mermaid 11.12.2).
fn commit_x(seq: usize) -> f64 {
    seq as f64 * (COMMIT_STEP + LAYOUT_OFFSET) + LAYOUT_OFFSET
}

/// Generate the commit label text (deterministic hash from seq).
fn commit_label_text(seq: usize) -> String {
    let hash = ((seq as u32).wrapping_mul(0x9E37_79B9) ^ 0x079A_D076) & 0x0FFF_FFFF;
    format!("{seq}-{hash:07x}")
}

/// Generate the commit CSS id class.
fn commit_id_class(seq: usize) -> String {
    commit_label_text(seq)
}

/// Emit the CSS style block matching Mermaid 11.12.2.
fn emit_style_block(svg: &mut String, theme: &MermaidTheme) {
    svg.push_str(&format!(
        "<style>#my-svg{{font-family:\"trebuchet ms\",verdana,arial,sans-serif;font-size:16px;fill:{};}}",
        theme.text_color
    ));
    // Mermaid 11.12.2 always emits commit-id, commit-msg, branch-label base styles.
    svg.push_str(
        "#my-svg .commit-id,#my-svg .commit-msg,#my-svg .branch-label{fill:lightgrey;color:lightgrey;font-family:'trebuchet ms',verdana,arial,sans-serif;font-family:var(--mermaid-font-family);}"
    );
    // Always emit all 8 branch color sets (THEME_COLOR_LIMIT) to match the reference.
    for i in 0..THEME_COLOR_LIMIT {
        let color = branch_color(i);
        let label_color = branch_label_color(i);
        svg.push_str(&format!("#my-svg .branch-label{i}{{fill:{label_color};}}"));
        svg.push_str(&format!(
            "#my-svg .commit{i}{{stroke:{color};fill:{color};}}"
        ));
        svg.push_str(&format!(
            "#my-svg .commit-highlight{i}{{stroke:{color};fill:{color};}}"
        ));
        svg.push_str(&format!("#my-svg .label{i}{{fill:{color};}}"));
        svg.push_str(&format!("#my-svg .arrow{i}{{stroke:{color};}}"));
    }
    svg.push_str(&format!(
        "#my-svg .branch{{stroke-width:1;stroke:{};stroke-dasharray:2;}}",
        theme.edge_color
    ));
    svg.push_str("#my-svg .commit-label{font-size:10px;fill:#000021;}");
    svg.push_str("#my-svg .commit-label-bkg{font-size:10px;fill:#ffffde;opacity:0.5;}");
    svg.push_str(&format!(
        "#my-svg .tag-label{{font-size:10px;fill:{tag_label_color};}}",
        tag_label_color = "#131300"
    ));
    svg.push_str(&format!(
        "#my-svg .tag-label-bkg{{fill:{};stroke:hsl(240, 60%, 86.2745098039%);}}",
        theme.node_fill
    ));
    svg.push_str(&format!("#my-svg .tag-hole{{fill:{};}}", theme.text_color));
    svg.push_str(&format!(
        "#my-svg .commit-merge{{stroke:{};fill:{};}}",
        theme.node_fill, theme.node_fill
    ));
    svg.push_str(&format!(
        "#my-svg .commit-reverse{{stroke:{};fill:{};stroke-width:3;}}",
        theme.node_fill, theme.node_fill
    ));
    svg.push_str(&format!(
        "#my-svg .commit-highlight-inner{{stroke:{};fill:{};}}",
        theme.node_fill, theme.node_fill
    ));
    svg.push_str(&format!(
        "#my-svg .arrow{{stroke-width:{ARROW_STROKE_WIDTH};stroke-linecap:round;fill:none;}}"
    ));
    svg.push_str(&format!(
        "#my-svg .gitTitleText{{text-anchor:middle;font-size:18px;fill:{};}}",
        theme.text_color
    ));
    svg.push_str("</style>");
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CommitKind {
    Normal,
    Merge,
}

#[derive(Debug, Clone)]
struct Commit {
    seq: usize,
    branch: String,
    parents: Vec<String>,
    kind: CommitKind,
}

#[derive(Debug, Clone)]
struct GitGraph {
    main_branch: String,
    branch_order: Vec<String>,
    commits: Vec<Commit>,
    commit_by_id: HashMap<String, usize>,
}

fn parse_gitgraph(input: &str) -> Result<GitGraph, MermaidError> {
    let mut found_header = false;

    let main_branch = "main".to_string();
    let mut branch_order: Vec<String> = vec![main_branch.clone()];

    let mut branches: HashMap<String, Option<String>> = HashMap::new();
    branches.insert(main_branch.clone(), None);

    let mut current_branch = main_branch.clone();
    let mut head: Option<String> = None;

    let mut commits: Vec<Commit> = Vec::new();
    let mut commit_by_id: HashMap<String, usize> = HashMap::new();

    for (idx, raw) in input.lines().enumerate() {
        let line_no = idx + 1;
        let line = raw.trim();

        if line.is_empty() || line.starts_with("%%") {
            continue;
        }

        if !found_header {
            if line.split_whitespace().next() != Some("gitGraph") {
                return Err(MermaidError::ParseError {
                    line: line_no,
                    message: "Expected 'gitGraph' declaration".to_string(),
                });
            }
            found_header = true;
            continue;
        }

        let mut parts = line.split_whitespace();
        let Some(cmd) = parts.next() else {
            continue;
        };

        match cmd {
            "commit" => {
                let seq = commits.len();
                let id = format!("{seq}");
                let parents = head.clone().into_iter().collect();
                let commit = Commit {
                    seq,
                    branch: current_branch.clone(),
                    parents,
                    kind: CommitKind::Normal,
                };
                commit_by_id.insert(id.clone(), seq);
                commits.push(commit);
                head = Some(id.clone());
                branches.insert(current_branch.clone(), Some(id));
            }
            "branch" => {
                let Some(name) = parts.next() else {
                    return Err(MermaidError::ParseError {
                        line: line_no,
                        message: "Expected branch name".to_string(),
                    });
                };
                let name = name.to_string();
                if !branches.contains_key(&name) {
                    branches.insert(name.clone(), head.clone());
                    branch_order.push(name.clone());
                }
            }
            "checkout" => {
                let Some(name) = parts.next() else {
                    return Err(MermaidError::ParseError {
                        line: line_no,
                        message: "Expected branch name".to_string(),
                    });
                };
                let name = name.to_string();
                current_branch = name.clone();
                head = branches.get(&name).cloned().flatten();
                if !branches.contains_key(&name) {
                    branches.insert(name.clone(), head.clone());
                    branch_order.push(name);
                }
            }
            "merge" => {
                let Some(other) = parts.next() else {
                    return Err(MermaidError::ParseError {
                        line: line_no,
                        message: "Expected branch name".to_string(),
                    });
                };
                let other = other.to_string();
                let other_head = branches.get(&other).cloned().flatten();
                let mut parents: Vec<String> = Vec::new();
                if let Some(h) = head.clone() {
                    parents.push(h);
                }
                if let Some(oh) = other_head {
                    parents.push(oh);
                }
                let seq = commits.len();
                let id = format!("{seq}");
                let commit = Commit {
                    seq,
                    branch: current_branch.clone(),
                    parents,
                    kind: CommitKind::Merge,
                };
                commit_by_id.insert(id.clone(), seq);
                commits.push(commit);
                head = Some(id.clone());
                branches.insert(current_branch.clone(), Some(id));
            }
            _ => {
                return Err(MermaidError::ParseError {
                    line: line_no,
                    message: format!("Unrecognized gitGraph command: {cmd}"),
                });
            }
        }
    }

    if !found_header {
        return Err(MermaidError::ParseError {
            line: 1,
            message: "Expected 'gitGraph' declaration".to_string(),
        });
    }

    Ok(GitGraph {
        main_branch,
        branch_order,
        commits,
        commit_by_id,
    })
}

fn branch_color(order: usize) -> &'static str {
    match order {
        0 => "hsl(240, 100%, 46.2745098039%)",
        1 => "hsl(60, 100%, 43.5294117647%)",
        2 => "hsl(80, 100%, 46.2745098039%)",
        3 => "hsl(210, 100%, 46.2745098039%)",
        4 => "hsl(180, 100%, 46.2745098039%)",
        5 => "hsl(150, 100%, 46.2745098039%)",
        6 => "hsl(300, 100%, 46.2745098039%)",
        7 => "hsl(0, 100%, 46.2745098039%)",
        _ => "hsl(180, 100%, 46.2745098039%)",
    }
}

fn branch_label_color(order: usize) -> &'static str {
    // Mermaid 11.12.2 default theme: branch-label0 = #ffffff, rest = black.
    match order {
        0 => "#ffffff",
        3 => "#ffffff",
        _ => "black",
    }
}

fn escape_xml(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}
