use crate::ast::{
    Edge, EdgeStyle, FlowchartGraph, GraphDirection, Node, NodeShape, Statement, StyleStatement,
    Subgraph,
};
use crate::error::MermaidError;

pub fn parse_mermaid(input: &str) -> Result<FlowchartGraph, MermaidError> {
    if let Some(first_line) = first_non_empty_non_comment_line(input) {
        let first_token = first_line.split_whitespace().next().unwrap_or("");

        if first_token != "graph"
            && first_token != "flowchart"
            && is_known_mermaid_type(first_token)
        {
            return Err(MermaidError::UnsupportedDiagramType(
                first_token.to_string(),
            ));
        }
    }

    let mut parser = Parser::new(input);
    parser.parse()
}

fn normalize_label(label: &str) -> String {
    let label = strip_wrapping_quotes(label.trim());
    decode_html_entities(label)
        .replace("\\n", "\n")
        .replace("<br/>", "\n")
        .replace("<br />", "\n")
        .replace("<br>", "\n")
        .replace("<BR/>", "\n")
        .replace("<BR />", "\n")
        .replace("<BR>", "\n")
}

fn decode_html_entities(label: &str) -> String {
    label
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

fn strip_wrapping_quotes(label: &str) -> &str {
    let bytes = label.as_bytes();
    if bytes.len() >= 2
        && ((bytes[0] == b'"' && bytes[bytes.len() - 1] == b'"')
            || (bytes[0] == b'\'' && bytes[bytes.len() - 1] == b'\''))
    {
        &label[1..label.len() - 1]
    } else {
        label
    }
}

fn first_non_empty_non_comment_line(input: &str) -> Option<&str> {
    input
        .lines()
        .map(|l| l.trim())
        .find(|l| !l.is_empty() && !l.starts_with("%%"))
}

fn is_known_mermaid_type(token: &str) -> bool {
    matches!(
        token,
        "sequenceDiagram"
            | "classDiagram"
            | "classDiagram-v2"
            | "stateDiagram"
            | "stateDiagram-v2"
            | "erDiagram"
            | "journey"
            | "gantt"
            | "pie"
            | "mindmap"
            | "timeline"
            | "info"
            | "kanban"
            | "gitGraph"
            | "requirementDiagram"
            | "C4Context"
            | "C4Container"
            | "C4Component"
            | "C4Dynamic"
            | "C4Deployment"
            | "sankey-beta"
            | "packet-beta"
            | "xychart-beta"
            | "radar-beta"
            | "block-beta"
            | "flowchart-elk"
            | "quadrantChart"
    )
}

struct Parser<'a> {
    lines: Vec<&'a str>,
    current_line: usize,
    next_subgraph_index: usize,
}

impl<'a> Parser<'a> {
    fn new(input: &'a str) -> Self {
        let lines: Vec<&str> = input.lines().collect();
        Self {
            lines,
            current_line: 0,
            next_subgraph_index: 0,
        }
    }

    fn parse(&mut self) -> Result<FlowchartGraph, MermaidError> {
        let direction = self.parse_graph_declaration()?;
        let statements = self.parse_statements()?;

        Ok(FlowchartGraph {
            direction,
            statements,
        })
    }

    fn current_line_content(&self) -> Option<&'a str> {
        self.lines.get(self.current_line).map(|s| s.trim())
    }

    fn advance(&mut self) {
        self.current_line += 1;
    }

    fn skip_empty_lines(&mut self) {
        while let Some(line) = self.current_line_content() {
            if line.is_empty() || line.starts_with("%%") {
                self.advance();
            } else {
                break;
            }
        }
    }

    fn parse_graph_declaration(&mut self) -> Result<GraphDirection, MermaidError> {
        self.skip_empty_lines();

        let line = self
            .current_line_content()
            .ok_or_else(|| MermaidError::ParseError {
                line: self.current_line + 1,
                message: "Expected graph declaration".to_string(),
            })?;

        let direction = if line.starts_with("graph ") || line.starts_with("flowchart ") {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() < 2 {
                return Err(MermaidError::ParseError {
                    line: self.current_line + 1,
                    message: "Expected direction after 'graph' or 'flowchart'".to_string(),
                });
            }
            self.parse_direction(parts[1])?
        } else {
            return Err(MermaidError::ParseError {
                line: self.current_line + 1,
                message: "Expected 'graph' or 'flowchart' declaration".to_string(),
            });
        };

        self.advance();
        Ok(direction)
    }

    fn parse_direction(&self, dir: &str) -> Result<GraphDirection, MermaidError> {
        match dir.to_uppercase().as_str() {
            "TD" | "TB" => Ok(GraphDirection::TopToBottom),
            "BT" => Ok(GraphDirection::BottomToTop),
            "LR" => Ok(GraphDirection::LeftToRight),
            "RL" => Ok(GraphDirection::RightToLeft),
            _ => Err(MermaidError::InvalidDirection(dir.to_string())),
        }
    }

    fn parse_statements(&mut self) -> Result<Vec<Statement>, MermaidError> {
        let mut statements = Vec::new();

        while self.current_line_content().is_some() {
            self.skip_empty_lines();

            let Some(line) = self.current_line_content() else {
                break;
            };

            if line.is_empty() {
                self.advance();
                continue;
            }

            if line == "end" {
                break;
            }

            if line.starts_with("subgraph ") {
                statements.push(Statement::Subgraph(self.parse_subgraph()?));
            } else if line.starts_with("style ") {
                statements.push(Statement::Style(self.parse_style()?));
            } else if self.line_contains_edge(line) {
                let edge_statements = self.parse_edge_chain(line)?;
                statements.extend(edge_statements);
                self.advance();
            } else {
                if let Some(node) = self.try_parse_node(line) {
                    statements.push(Statement::Node(node));
                }
                self.advance();
            }
        }

        Ok(statements)
    }

    fn line_contains_edge(&self, line: &str) -> bool {
        self.find_edge_start(line).is_some()
    }

    fn parse_edge_chain(&mut self, line: &str) -> Result<Vec<Statement>, MermaidError> {
        let mut statements = Vec::new();
        let mut remaining = line.trim();
        let mut collected_nodes: Vec<(String, Option<Node>)> = Vec::new();

        let first_node_end = self.find_edge_start(remaining).unwrap_or(remaining.len());
        let first_node_str = remaining[..first_node_end].trim();
        if let Some(node) = self.try_parse_node(first_node_str) {
            collected_nodes.push((node.id.clone(), Some(node)));
        } else {
            let id = self.extract_node_id(first_node_str);
            collected_nodes.push((id, None));
        }
        remaining = &remaining[first_node_end..];

        while !remaining.is_empty() {
            let (edge_style, label, edge_len) = self.parse_edge_syntax(remaining)?;
            remaining = remaining[edge_len..].trim_start();

            let next_node_end = self.find_edge_start(remaining).unwrap_or(remaining.len());
            let next_node_str = remaining[..next_node_end].trim();

            if next_node_str.is_empty() {
                break;
            }

            let (next_id, next_node) = if let Some(node) = self.try_parse_node(next_node_str) {
                (node.id.clone(), Some(node))
            } else {
                let id = self.extract_node_id(next_node_str);
                (id, None)
            };

            if let Some((from_id, _)) = collected_nodes.last() {
                statements.push(Statement::Edge(Edge {
                    from: from_id.clone(),
                    to: next_id.clone(),
                    label,
                    style: edge_style,
                }));
            }

            collected_nodes.push((next_id, next_node));
            remaining = &remaining[next_node_end..];
        }

        let mut node_statements: Vec<Statement> = collected_nodes
            .into_iter()
            .filter_map(|(_, node_opt)| node_opt.map(Statement::Node))
            .collect();
        node_statements.append(&mut statements);
        statements = node_statements;

        Ok(statements)
    }

    /// Byte index where the first edge token starts, ignoring tokens inside
    /// bracket/quote-delimited node labels (`[..]`, `(..)`, `{..}`, `".."`).
    fn find_edge_start(&self, s: &str) -> Option<usize> {
        const PATTERNS: [&str; 9] = ["-.->", "-.-", "-->", "---", "==>", "===", "--", "==", "-."];
        let bytes = s.as_bytes();
        let mut depth: usize = 0;
        let mut in_quote = false;
        for i in 0..bytes.len() {
            let b = bytes[i];
            if in_quote {
                if b == b'"' {
                    in_quote = false;
                }
                continue;
            }
            match b {
                b'"' => in_quote = true,
                b'[' | b'(' | b'{' => depth += 1,
                b']' | b')' | b'}' => depth = depth.saturating_sub(1),
                _ if depth == 0 => {
                    if PATTERNS
                        .iter()
                        .any(|p| bytes[i..].starts_with(p.as_bytes()))
                    {
                        return Some(i);
                    }
                }
                _ => {}
            }
        }
        None
    }

    fn parse_edge_syntax(
        &self,
        s: &str,
    ) -> Result<(EdgeStyle, Option<String>, usize), MermaidError> {
        let s = s.trim_start();

        let edge_patterns: &[(&str, EdgeStyle, &str)] = &[
            ("-->|", EdgeStyle::Arrow, "|"),
            ("---|", EdgeStyle::Line, "|"),
            ("-.->|", EdgeStyle::DottedArrow, "|"),
            ("-.-|", EdgeStyle::DottedLine, "|"),
            ("==>|", EdgeStyle::ThickArrow, "|"),
            ("===|", EdgeStyle::ThickLine, "|"),
            ("-->", EdgeStyle::Arrow, ""),
            ("---", EdgeStyle::Line, ""),
            ("-.->", EdgeStyle::DottedArrow, ""),
            ("-.-", EdgeStyle::DottedLine, ""),
            ("==>", EdgeStyle::ThickArrow, ""),
            ("===", EdgeStyle::ThickLine, ""),
        ];

        for (pattern, style, label_end) in edge_patterns {
            if let Some(after_pattern) = s.strip_prefix(pattern) {
                if !label_end.is_empty() {
                    if let Some(end_idx) = after_pattern.find(label_end) {
                        let label = normalize_label(&after_pattern[..end_idx]);
                        let total_len = pattern.len() + end_idx + label_end.len();
                        return Ok((*style, Some(label), total_len));
                    }
                } else {
                    return Ok((*style, None, pattern.len()));
                }
            }
        }

        // Open-label forms: `-- text -->`, `-- text ---`, `== text ==>`,
        // `== text ===`, `-. text .->`, `-. text .-`.
        let open_patterns: &[(&str, &[(&str, EdgeStyle)])] = &[
            ("--", &[("-->", EdgeStyle::Arrow), ("---", EdgeStyle::Line)]),
            (
                "==",
                &[
                    ("==>", EdgeStyle::ThickArrow),
                    ("===", EdgeStyle::ThickLine),
                ],
            ),
            (
                "-.",
                &[
                    (".->", EdgeStyle::DottedArrow),
                    (".-", EdgeStyle::DottedLine),
                ],
            ),
        ];
        for (opener, closers) in open_patterns {
            let Some(after) = s.strip_prefix(opener) else {
                continue;
            };
            let mut best: Option<(usize, &str, EdgeStyle)> = None;
            for (closer, style) in *closers {
                if let Some(idx) = after.find(closer) {
                    let better = match best {
                        Some((best_idx, best_closer, _)) => {
                            idx < best_idx || (idx == best_idx && closer.len() > best_closer.len())
                        }
                        None => true,
                    };
                    if better {
                        best = Some((idx, closer, *style));
                    }
                }
            }
            if let Some((idx, closer, style)) = best {
                let label = normalize_label(&after[..idx]);
                let total_len = opener.len() + idx + closer.len();
                return Ok((style, Some(label), total_len));
            }
        }

        Err(MermaidError::ParseError {
            line: self.current_line + 1,
            message: format!("Invalid edge syntax: {}", s),
        })
    }

    fn extract_node_id(&self, s: &str) -> String {
        let s = s.trim();
        for (open, _close) in [('[', ']'), ('(', ')'), ('{', '}'), ('<', '>')] {
            if let Some(idx) = s.find(open) {
                return s[..idx].trim().to_string();
            }
        }
        s.to_string()
    }

    fn try_parse_node(&self, s: &str) -> Option<Node> {
        let s = s.trim();
        if s.is_empty() {
            return None;
        }

        if let Some(paren_paren_start) = s.find("((") {
            if s.ends_with("))") {
                let id = s[..paren_paren_start].trim().to_string();
                let label = normalize_label(&s[paren_paren_start + 2..s.len() - 2]);
                let id = if id.is_empty() {
                    label.chars().filter(|c| c.is_alphanumeric()).collect()
                } else {
                    id
                };
                return Some(Node {
                    id,
                    label: Some(label),
                    shape: NodeShape::Circle,
                });
            }
        }

        if let Some(bracket_paren_start) = s.find("([") {
            if s.ends_with("])") {
                let id = s[..bracket_paren_start].trim().to_string();
                let label = normalize_label(&s[bracket_paren_start + 2..s.len() - 2]);
                let id = if id.is_empty() {
                    label.chars().filter(|c| c.is_alphanumeric()).collect()
                } else {
                    id
                };
                return Some(Node {
                    id,
                    label: Some(label),
                    shape: NodeShape::Stadium,
                });
            }
        }

        if let Some(paren_bracket_start) = s.find("[(") {
            if s.ends_with(")]") {
                let id = s[..paren_bracket_start].trim().to_string();
                let label = normalize_label(&s[paren_bracket_start + 2..s.len() - 2]);
                let id = if id.is_empty() {
                    label.chars().filter(|c| c.is_alphanumeric()).collect()
                } else {
                    id
                };
                return Some(Node {
                    id,
                    label: Some(label),
                    shape: NodeShape::Cylinder,
                });
            }
        }

        if let Some(bracket_bracket_start) = s.find("[[") {
            if s.ends_with("]]") {
                let id = s[..bracket_bracket_start].trim().to_string();
                let label = normalize_label(&s[bracket_bracket_start + 2..s.len() - 2]);
                let id = if id.is_empty() {
                    label.chars().filter(|c| c.is_alphanumeric()).collect()
                } else {
                    id
                };
                return Some(Node {
                    id,
                    label: Some(label),
                    shape: NodeShape::Subroutine,
                });
            }
        }

        if let Some(brace_brace_start) = s.find("{{") {
            if s.ends_with("}}") {
                let id = s[..brace_brace_start].trim().to_string();
                let label = normalize_label(&s[brace_brace_start + 2..s.len() - 2]);
                let id = if id.is_empty() {
                    label.chars().filter(|c| c.is_alphanumeric()).collect()
                } else {
                    id
                };
                return Some(Node {
                    id,
                    label: Some(label),
                    shape: NodeShape::Hexagon,
                });
            }
        }

        if let Some(bracket_start) = s.find('[') {
            if s.ends_with(']') {
                let id = s[..bracket_start].trim().to_string();
                let label = normalize_label(&s[bracket_start + 1..s.len() - 1]);
                let id = if id.is_empty() {
                    label.chars().filter(|c| c.is_alphanumeric()).collect()
                } else {
                    id
                };
                return Some(Node {
                    id,
                    label: Some(label),
                    shape: NodeShape::Rectangle,
                });
            }
        }

        if let Some(paren_start) = s.find('(') {
            if s.ends_with(')') && !s.ends_with("))") {
                let id = s[..paren_start].trim().to_string();
                let label = normalize_label(&s[paren_start + 1..s.len() - 1]);
                let id = if id.is_empty() {
                    label.chars().filter(|c| c.is_alphanumeric()).collect()
                } else {
                    id
                };
                return Some(Node {
                    id,
                    label: Some(label),
                    shape: NodeShape::RoundedRectangle,
                });
            }
        }

        if let Some(brace_start) = s.find('{') {
            if s.ends_with('}') && !s.ends_with("}}") {
                let id = s[..brace_start].trim().to_string();
                let label = normalize_label(&s[brace_start + 1..s.len() - 1]);
                let id = if id.is_empty() {
                    label.chars().filter(|c| c.is_alphanumeric()).collect()
                } else {
                    id
                };
                return Some(Node {
                    id,
                    label: Some(label),
                    shape: NodeShape::Diamond,
                });
            }
        }

        if s.contains('>') && s.ends_with(']') {
            if let Some(gt_idx) = s.find('>') {
                let id = s[..gt_idx].trim().to_string();
                let label = normalize_label(&s[gt_idx + 1..s.len() - 1]);
                return Some(Node {
                    id,
                    label: Some(label),
                    shape: NodeShape::Asymmetric,
                });
            }
        }

        if s.chars().all(|c| c.is_alphanumeric() || c == '_') {
            return Some(Node {
                id: s.to_string(),
                label: None,
                shape: NodeShape::Rectangle,
            });
        }

        None
    }

    fn parse_subgraph(&mut self) -> Result<Subgraph, MermaidError> {
        let line = self
            .current_line_content()
            .ok_or_else(|| MermaidError::ParseError {
                line: self.current_line + 1,
                message: "Expected subgraph".to_string(),
            })?;

        let after_keyword = line.strip_prefix("subgraph ").unwrap_or("").trim();

        let (id, title) = if let Some(bracket_start) = after_keyword.find('[') {
            if after_keyword.ends_with(']') {
                let id = after_keyword[..bracket_start].trim().to_string();
                let title =
                    normalize_label(&after_keyword[bracket_start + 1..after_keyword.len() - 1]);
                (id, Some(title))
            } else {
                (after_keyword.to_string(), None)
            }
        } else if after_keyword.split_whitespace().count() > 1 {
            let id = format!("subGraph{}", self.next_subgraph_index);
            self.next_subgraph_index += 1;
            (id, Some(normalize_label(after_keyword)))
        } else {
            let id = after_keyword.to_string();
            (id, None)
        };

        self.advance();

        let statements = self.parse_statements()?;

        if self.current_line_content() == Some("end") {
            self.advance();
        }

        Ok(Subgraph {
            id,
            title,
            statements,
        })
    }

    fn parse_style(&mut self) -> Result<StyleStatement, MermaidError> {
        let line = self
            .current_line_content()
            .ok_or_else(|| MermaidError::ParseError {
                line: self.current_line + 1,
                message: "Expected style statement".to_string(),
            })?;

        let after_keyword = line.strip_prefix("style ").unwrap_or("").trim();
        let parts: Vec<&str> = after_keyword.splitn(2, ' ').collect();

        if parts.is_empty() {
            return Err(MermaidError::ParseError {
                line: self.current_line + 1,
                message: "Expected node id after 'style'".to_string(),
            });
        }

        let node_id = parts[0].to_string();
        let properties = if parts.len() > 1 {
            parts[1]
                .split(',')
                .filter_map(|prop| {
                    let kv: Vec<&str> = prop.splitn(2, ':').collect();
                    if kv.len() == 2 {
                        Some((kv[0].trim().to_string(), kv[1].trim().to_string()))
                    } else {
                        None
                    }
                })
                .collect()
        } else {
            Vec::new()
        };

        self.advance();

        Ok(StyleStatement {
            node_id,
            properties,
        })
    }
}
