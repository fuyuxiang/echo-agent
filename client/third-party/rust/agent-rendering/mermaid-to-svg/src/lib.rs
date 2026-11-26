use std::borrow::Cow;
mod ast;
mod block_diagram;
mod c4_diagram;
mod class_diagram;
pub mod config;
mod er_diagram;
mod error;
mod gantt_diagram;
mod gitgraph_diagram;
mod info_diagram;
mod journey_diagram;
mod kanban_diagram;
mod layout;
mod mermaid_port;
mod mindmap_diagram;
mod packet_diagram;
mod parser;
mod pie_diagram;
mod quadrant_diagram;
mod radar_diagram;
mod requirement_diagram;
mod sankey_diagram;
mod sequence_diagram;
mod state_diagram;
mod svg_renderer;
mod text_wrap;
mod theme;
mod timeline_diagram;
mod xychart_diagram;

pub use config::{parse_mermaid_frontmatter, FlowchartConfig, ParsedMermaidSource, RenderConfig};
pub use error::MermaidError;
pub use theme::{MermaidTheme, MermaidThemePreset, MermaidThemeVariables};

pub fn render_mermaid_to_svg(
    mermaid_source: &str,
    theme: Option<&MermaidTheme>,
) -> Result<String, MermaidError> {
    let parsed_source = parse_mermaid_frontmatter(mermaid_source);
    let default_theme = MermaidTheme::default();
    let configured_theme = parsed_source.config.to_mermaid_theme();
    let theme = match theme {
        Some(theme) => theme,
        None => configured_theme.as_ref().unwrap_or(&default_theme),
    };
    let mermaid_source = parsed_source.body.as_ref();

    let diagram_type = first_diagram_type_token(mermaid_source);

    if diagram_type == Some("erDiagram") {
        return er_diagram::render_er_diagram_to_svg(mermaid_source, theme);
    }

    if diagram_type == Some("classDiagram") {
        return class_diagram::render_class_diagram_to_svg(mermaid_source, theme);
    }

    if diagram_type == Some("mindmap") {
        return mindmap_diagram::render_mindmap_to_svg(mermaid_source, theme);
    }

    if matches!(diagram_type, Some("stateDiagram") | Some("stateDiagram-v2")) {
        let graph = state_diagram::parse_state_diagram(mermaid_source)?;
        let layout_result = layout::compute_layout(&graph);
        let svg = svg_renderer::render(&layout_result, theme);
        return Ok(svg);
    }

    if diagram_type == Some("pie") {
        return pie_diagram::render_pie_diagram_to_svg(mermaid_source, theme);
    }

    if diagram_type == Some("gantt") {
        return gantt_diagram::render_gantt_diagram_to_svg(mermaid_source, theme);
    }

    if diagram_type == Some("requirementDiagram") {
        return requirement_diagram::render_requirement_diagram_to_svg(mermaid_source, theme);
    }

    if diagram_type == Some("info") {
        return info_diagram::render_info_diagram_to_svg(mermaid_source, theme);
    }

    if diagram_type == Some("packet-beta") {
        return packet_diagram::render_packet_diagram_to_svg(mermaid_source, theme);
    }

    if diagram_type == Some("block-beta") {
        return block_diagram::render_block_diagram_to_svg(mermaid_source, theme);
    }

    if diagram_type == Some("radar-beta") {
        return radar_diagram::render_radar_diagram_to_svg(mermaid_source, theme);
    }

    if diagram_type == Some("sankey-beta") {
        return sankey_diagram::render_sankey_diagram_to_svg(mermaid_source, theme);
    }

    if diagram_type == Some("sequenceDiagram") {
        return sequence_diagram::render_sequence_diagram_to_svg(mermaid_source, theme);
    }

    if diagram_type == Some("gitGraph") {
        return gitgraph_diagram::render_gitgraph_diagram_to_svg(mermaid_source, theme);
    }

    if diagram_type == Some("timeline") {
        return timeline_diagram::render_timeline_diagram_to_svg(mermaid_source, theme);
    }

    if diagram_type == Some("journey") {
        return journey_diagram::render_journey_diagram_to_svg(mermaid_source, theme);
    }

    if diagram_type == Some("kanban") {
        return kanban_diagram::render_kanban_diagram_to_svg(mermaid_source, theme);
    }

    if diagram_type == Some("quadrantChart") {
        return quadrant_diagram::render_quadrant_chart_to_svg(mermaid_source, theme);
    }

    if diagram_type == Some("xychart-beta") {
        return xychart_diagram::render_xychart_diagram_to_svg(mermaid_source, theme);
    }

    if matches!(
        diagram_type,
        Some("C4Context")
            | Some("C4Container")
            | Some("C4Component")
            | Some("C4Dynamic")
            | Some("C4Deployment")
    ) {
        return c4_diagram::render_c4_diagram_to_svg(mermaid_source, theme);
    }

    let is_flowchart = matches!(diagram_type, Some("graph") | Some("flowchart"));
    if is_flowchart && mermaid_port::is_enabled() {
        return mermaid_port::render_mermaid_to_svg_ported(
            mermaid_source,
            theme,
            &parsed_source.config,
        );
    }

    let graph = parser::parse_mermaid(mermaid_source)?;
    let layout_result = if is_flowchart {
        layout::compute_layout_with_config(&graph, &parsed_source.config)
    } else {
        layout::compute_layout(&graph)
    };
    let svg = if is_flowchart {
        svg_renderer::render_with_config(&layout_result, theme, &parsed_source.config)
    } else {
        svg_renderer::render(&layout_result, theme)
    };

    Ok(svg)
}

/// Strip a leading Mermaid YAML frontmatter block delimited by `---` lines.
///
/// Mermaid supports frontmatter at the start of a diagram for per-diagram
/// metadata and configuration. Use `parse_mermaid_frontmatter` to access parsed
/// metadata and config values.
pub fn strip_mermaid_frontmatter(source: &str) -> Cow<'_, str> {
    parse_mermaid_frontmatter(source).body
}

fn first_diagram_type_token(input: &str) -> Option<&str> {
    input
        .lines()
        .map(|l| l.trim())
        .find(|l| !l.is_empty() && !l.starts_with("%%"))
        .and_then(|l| l.split_whitespace().next())
}

pub fn is_mermaid_diagram(lang: &str) -> bool {
    let lang_lower = lang.to_lowercase();
    lang_lower == "mermaid" || lang_lower.starts_with("mermaid ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_open_edge_label_syntax_parses_as_labels_not_nodes() {
        // `B -- 是 --> C`: the `-- text -->` open-label form must produce an
        // edge label, not a literal node named "B -- 是".
        let mermaid = "flowchart TD
    A[开始] --> B{是否登录?}
    B -- 是 --> C[进入主页]
    B -- 否 --> D[跳转登录页]
    D --> E[输入用户名和密码]
    E --> B
    C --> F[结束]";
        let svg = render_mermaid_to_svg(mermaid, None).expect("open-label flow renders");
        assert!(!svg.contains("B -- "), "no literal `B -- x` node: {svg}");
        for label in ["是", "否"] {
            assert!(svg.contains(label), "edge label {label:?} missing");
        }
        for node in ["开始", "是否登录?", "进入主页", "跳转登录页", "结束"] {
            assert!(svg.contains(node), "node label {node:?} missing");
        }
        let arrowheads = svg.matches(r#"marker-end="url(#arrowhead)""#).count();
        assert_eq!(arrowheads, 6, "all six edges must survive parsing");
    }

    #[test]
    fn test_open_edge_label_variants() {
        for (mermaid, needle) in [
            ("flowchart LR\n    A -- plain --- B", "plain"),
            ("flowchart LR\n    A == fast ==> B", "fast"),
            ("flowchart LR\n    A == heavy === B", "heavy"),
            ("flowchart LR\n    A -. dotted .-> B", "dotted"),
            ("flowchart LR\n    A -. faint .- B", "faint"),
            ("flowchart LR\n    A--tight-->B", "tight"),
        ] {
            let svg = render_mermaid_to_svg(mermaid, None).expect(mermaid);
            assert!(
                svg.contains(needle),
                "label {needle:?} missing for {mermaid:?}"
            );
            assert!(!svg.contains("A --"), "bogus node for {mermaid:?}");
        }
    }

    #[test]
    fn test_edge_tokens_inside_node_labels_are_ignored() {
        let mermaid = "flowchart TD\n    A[uses --> arrows] --> B{\"x --> y?\"}";
        let svg = render_mermaid_to_svg(mermaid, None).expect("bracketed arrows render");
        assert!(
            svg.contains("uses --&gt; arrows"),
            "label kept intact: {svg}"
        );
        assert!(
            svg.contains("x --&gt; y?"),
            "quoted label kept intact: {svg}"
        );
        let arrowheads = svg.matches(r#"marker-end="url(#arrowhead)""#).count();
        assert_eq!(arrowheads, 1, "exactly one real edge");
    }

    #[test]
    fn test_cjk_display_width_counts_wide_chars_as_two_units() {
        use crate::text_wrap::{display_width_units, line_width};
        assert_eq!(display_width_units("abcd"), 4.0);
        assert_eq!(line_width("abcd", 8.0), 32.0);
        assert_eq!(display_width_units("提交代码"), 8.0);
        assert_eq!(line_width("提交代码", 8.0), 64.0);
        assert_eq!(display_width_units("修复bug"), 7.0);
        assert_eq!(line_width("", 8.0), 0.0);
    }

    #[test]
    fn test_cjk_flowchart_node_is_wider_than_ascii_same_char_count() {
        fn root_svg_width(label: &str) -> f64 {
            let svg = render_mermaid_to_svg(&format!("flowchart TD\n    A[{label}]"), None)
                .expect("flowchart renders");
            let width_attr = svg
                .split("width=\"")
                .nth(1)
                .and_then(|rest| rest.split('"').next())
                .expect("svg has a width attribute");
            width_attr.parse::<f64>().expect("width parses")
        }
        let cjk = root_svg_width("提交代码审查流程");
        let ascii = root_svg_width("abcdefgh");
        assert!(
            cjk > ascii + 40.0,
            "CJK node must be measured ~2x wider: cjk={cjk}, ascii={ascii}"
        );
    }

    #[test]
    fn class_relationship_with_quoted_cardinalities_parses() {
        let mermaid = "classDiagram\n    Owner \"1\" o-- \"0..*\" Animal : owns\n    Dog \"1\" --> \"0..1\" Ball : plays with\n    Animal <|-- Dog : extends";
        let svg = render_mermaid_to_svg(mermaid, None).expect("cardinalities must parse");
        assert!(svg.contains("1 owns 0..*"), "cardinalities fold into label");
        assert!(svg.contains("1 plays with 0..1"));
        assert!(svg.contains("extends"));
    }

    #[test]
    fn class_relationship_rejects_malformed_cardinality_lines() {
        assert!(render_mermaid_to_svg("classDiagram\n    \"1\" o-- Animal", None).is_err());
        assert!(render_mermaid_to_svg("classDiagram\n    Owner o-- \"0..*\"", None).is_err());
        assert!(render_mermaid_to_svg("classDiagram\n    Owner x \"1\" o-- Animal", None).is_err());
    }

    #[test]
    fn er_attribute_rows_follow_theme_darkness() {
        let mermaid =
            "erDiagram\n    CUSTOMER {\n        string name\n        int custNumber\n    }";
        let dark = render_mermaid_to_svg(mermaid, Some(&MermaidTheme::dark()))
            .expect("er must render dark");
        assert!(
            !dark.contains("style=\"fill:#ffffff"),
            "dark theme must not paint white attribute rows"
        );
        let light = render_mermaid_to_svg(mermaid, Some(&MermaidTheme::light()))
            .expect("er must render light");
        assert!(
            light.contains("style=\"fill:#ffffff"),
            "light theme keeps upstream white odd rows"
        );
    }

    #[test]
    fn test_is_mermaid_diagram() {
        assert!(is_mermaid_diagram("mermaid"));
        assert!(is_mermaid_diagram("Mermaid"));
        assert!(is_mermaid_diagram("MERMAID"));
        assert!(is_mermaid_diagram("mermaid "));
        assert!(!is_mermaid_diagram("rust"));
        assert!(!is_mermaid_diagram(""));
    }

    #[test]
    fn test_simple_flowchart() {
        let mermaid = r#"graph TD
    A[Start] --> B[End]"#;

        let result = render_mermaid_to_svg(mermaid, None);
        assert!(result.is_ok());
        let svg = result.unwrap();
        assert!(svg.contains("<svg"));
        assert!(svg.contains("</svg>"));
    }

    #[test]
    fn test_empty_flowchart_does_not_panic() {
        let result = render_mermaid_to_svg("graph TD\n", None);

        assert!(result.is_ok());
        if let Ok(svg) = result {
            assert!(svg.contains("<svg"));
            assert!(svg.contains("</svg>"));
        }
    }

    #[test]
    fn test_flowchart_with_theme() {
        let mermaid = r#"graph LR
    A --> B"#;

        let theme = MermaidTheme::dark();
        let result = render_mermaid_to_svg(mermaid, Some(&theme));
        assert!(result.is_ok());
    }

    #[test]
    fn test_flowchart_with_decision() {
        let mermaid = r#"graph TD
    A[Start] --> B{Decision}
    B -->|Yes| C[Action 1]
    B -->|No| D[Action 2]
    C --> E[End]
    D --> E"#;

        let result = render_mermaid_to_svg(mermaid, None);
        assert!(result.is_ok());
    }

    #[test]
    fn test_strip_mermaid_frontmatter_removes_leading_config_block() {
        let source = r#"---
config:
  theme: default
---
xychart-beta
  title "x"
"#;
        let stripped = strip_mermaid_frontmatter(source);
        assert_eq!(
            stripped,
            r#"xychart-beta
  title "x"
"#
        );
        assert!(matches!(stripped, std::borrow::Cow::Owned(_)));
    }

    #[test]
    fn test_strip_mermaid_frontmatter_preserves_source_without_frontmatter() {
        let source = "graph TD\nA --> B\n";
        let stripped = strip_mermaid_frontmatter(source);
        assert_eq!(stripped, source);
        assert!(matches!(stripped, std::borrow::Cow::Borrowed(_)));
    }

    #[test]
    fn test_strip_mermaid_frontmatter_skips_leading_blank_lines() {
        let source = r#"

   
---
config:
  theme: dark
---
pie
  "a" : 1
"#;
        let stripped = strip_mermaid_frontmatter(source);
        assert_eq!(
            stripped,
            r#"pie
  "a" : 1
"#
        );
    }

    #[test]
    fn test_strip_mermaid_frontmatter_handles_crlf_line_endings() {
        let source = "---\r\nconfig:\r\n  theme: default\r\n---\r\nflowchart TD\r\nA --> B\r\n";
        let stripped = strip_mermaid_frontmatter(source);
        assert_eq!(stripped, "flowchart TD\r\nA --> B\r\n");
    }

    #[test]
    fn test_strip_mermaid_frontmatter_leaves_unterminated_block() {
        let source = "---\nconfig:\n  theme: default\nflowchart TD\nA --> B\n";
        let stripped = strip_mermaid_frontmatter(source);
        assert_eq!(stripped, source);
        assert!(matches!(stripped, std::borrow::Cow::Borrowed(_)));
    }

    #[test]
    fn test_strip_mermaid_frontmatter_handles_only_frontmatter_no_body() {
        let source = "---\nconfig: {}\n---\n";
        let stripped = strip_mermaid_frontmatter(source);
        assert_eq!(stripped, "");
    }

    #[test]
    fn test_strip_mermaid_frontmatter_handles_frontmatter_without_trailing_newline() {
        let source = "---\nfoo: bar\n---";
        let stripped = strip_mermaid_frontmatter(source);
        assert_eq!(stripped, "");
    }

    #[test]
    fn test_strip_mermaid_frontmatter_treats_indented_dashes_as_delimiter() {
        let source = "\t---\n  config: x\n  ---\nflowchart TD\nA --> B\n";
        let stripped = strip_mermaid_frontmatter(source);
        assert_eq!(stripped, "flowchart TD\nA --> B\n");
    }

    #[test]
    fn test_render_mermaid_to_svg_dispatches_after_frontmatter() {
        let mermaid = r#"---
config:
  theme: default
---
xychart-beta
    title Demo
    x-axis 0 --> 10
    y-axis 0 --> 100
    line [5, 10, 20, 40]"#;

        let result = render_mermaid_to_svg(mermaid, None);
        let svg = match result {
            Ok(svg) => svg,
            Err(err) => panic!("expected ok result, got error: {err}"),
        };
        assert!(svg.contains("<svg"));
        assert!(svg.contains("Demo"));
    }

    #[test]
    fn test_render_mermaid_to_svg_applies_frontmatter_theme_values() {
        let mermaid = r#"---
config:
  theme: dark
---
graph TD
    A --> B"#;

        let result = render_mermaid_to_svg(mermaid, None);
        let svg = match result {
            Ok(svg) => svg,
            Err(err) => panic!("expected ok result, got error: {err}"),
        };
        assert!(svg.contains("background-color: #1e1e1e"));
        assert!(svg.contains(r##"fill="#1e1e1e" stroke="none"/>"##));
    }

    #[test]
    fn test_render_mermaid_to_svg_explicit_theme_overrides_frontmatter_theme() {
        let mermaid = r##"---
config:
  theme: dark
  themeVariables:
    background: "#101010"
---
graph TD
    A --> B"##;

        let theme = MermaidTheme::light();
        let result = render_mermaid_to_svg(mermaid, Some(&theme));
        let svg = match result {
            Ok(svg) => svg,
            Err(err) => panic!("expected ok result, got error: {err}"),
        };
        assert!(svg.contains("background-color: #ffffff"));
        assert!(!svg.contains("background-color: #101010"));
    }

    #[test]
    fn test_render_mermaid_to_svg_applies_flowchart_frontmatter_render_config() {
        let mermaid = r#"---
config:
  fontFamily: Inter
  fontSize: 21px
  flowchart:
    curve: linear
---
flowchart TD
    A[Start] --> B[End]"#;

        let result = render_mermaid_to_svg(mermaid, None);
        let svg = match result {
            Ok(svg) => svg,
            Err(err) => panic!("expected ok result, got error: {err}"),
        };
        assert!(svg.contains("font-family=\"Inter\""));
        assert!(svg.contains("font-size=\"21\""));
        assert!(svg.contains("<path d=\"M"));
        assert!(svg.contains("L"));
    }

    #[test]
    fn test_flowchart_decodes_html_entities_before_svg_escaping() {
        let mermaid = r#"graph TD
    A["shared_ptr&lt;Connection&gt; &amp; weak_ptr&lt;Player&gt;"]"#;

        let svg = render_mermaid_to_svg(mermaid, None).expect("should render");
        assert!(svg.contains("shared_ptr&lt;Connection&gt;"));
        assert!(svg.contains("weak_ptr&lt;Player&gt;"));
        assert!(!svg.contains("&amp;lt;Connection&amp;gt;"));
        assert!(!svg.contains("&amp;amp;"));
    }
    #[test]
    fn test_flowchart_renders_escaped_newline_label_as_multiple_lines() {
        let mermaid = r#"graph TD
    A["Source\nTarget"]"#;

        let svg = render_mermaid_to_svg(mermaid, None).expect("should render");
        assert!(svg.contains(">Source</tspan>"));
        assert!(svg.contains(">Target</tspan>"));
        assert!(!svg.contains("Source\\nTarget"));
    }

    #[test]
    fn test_invalid_mermaid() {
        let mermaid = "not a valid mermaid diagram";
        let result = render_mermaid_to_svg(mermaid, None);
        assert!(result.is_err());
    }

    #[test]
    fn test_simple_er_diagram() {
        let mermaid = r#"erDiagram
    CUSTOMER ||--o{ ORDER : places
    ORDER ||--|{ LINE_ITEM : contains

    CUSTOMER {
        string name
        string custNumber
    }

    ORDER {
        int orderNumber
        date orderDate
    }

    LINE_ITEM {
        int quantity
        float price
    }"#;

        let result = render_mermaid_to_svg(mermaid, None);
        assert!(result.is_ok());
        let svg = result.unwrap();
        assert!(svg.contains("<svg"));
        assert!(svg.contains("</svg>"));
    }

    #[test]
    fn test_simple_packet_diagram() {
        let mermaid = r#"packet-beta
0-3: "Header"
4-7: "Payload"
8: "CRC""#;

        let result = render_mermaid_to_svg(mermaid, None);
        assert!(result.is_ok());
        let svg = result.unwrap();
        assert!(svg.contains("<svg"));
        assert!(svg.contains("packetBlock"));
    }

    #[test]
    fn test_simple_info_diagram() {
        let mermaid = "info";

        let result = render_mermaid_to_svg(mermaid, None);
        assert!(result.is_ok());
        let svg = result.unwrap();
        assert!(svg.contains("v11.12.2"));
    }

    #[test]
    fn test_simple_class_diagram() {
        let mermaid = r#"classDiagram
    class Animal
    class Duck

    Animal : +int age
    Animal <|-- Duck"#;

        let result = render_mermaid_to_svg(mermaid, None);
        assert!(result.is_ok());
        let svg = result.unwrap();
        assert!(svg.contains("<svg"));
        assert!(svg.contains("</svg>"));
    }

    #[test]
    fn test_simple_state_diagram() {
        let mermaid = r#"stateDiagram-v2
    [*] --> Idle
    Idle --> Working : start
    Working --> Idle : done
    Working --> [*]"#;

        let result = render_mermaid_to_svg(mermaid, None);
        assert!(result.is_ok());
        let svg = result.unwrap();
        assert!(svg.contains("<svg"));
        assert!(svg.contains("</svg>"));
    }

    #[test]
    fn test_simple_pie_diagram() {
        let mermaid = r#"pie
    title Pets adopted by volunteers
    \"Dogs\" : 386
    \"Cats\" : 85
    \"Rats\" : 15"#;

        let result = render_mermaid_to_svg(mermaid, None);
        assert!(result.is_ok());
        let svg = result.unwrap();
        assert!(svg.contains("<svg"));
        assert!(svg.contains("</svg>"));
    }

    #[test]
    fn test_simple_gantt_diagram() {
        let mermaid = r#"gantt
    title Simple Gantt
    dateFormat  YYYY-MM-DD

    section Build
    Setup        :a1, 2026-01-01, 2d
    Implement    :a2, after a1, 5d
    Test         :a3, after a2, 3d"#;

        let result = render_mermaid_to_svg(mermaid, None);
        assert!(result.is_ok());
        let svg = result.unwrap();
        assert!(svg.contains("<svg"));
        assert!(svg.contains("</svg>"));
    }

    #[test]
    fn test_simple_sequence_diagram() {
        let mermaid = r#"sequenceDiagram
    participant Alice
    participant Bob

    Alice->>Bob: Hello
    Note over Alice,Bob: Hello back
    Bob-->>Alice: Hi"#;

        let result = render_mermaid_to_svg(mermaid, None);
        assert!(result.is_ok());
        let svg = result.unwrap();
        assert!(svg.contains("<svg"));
        assert!(svg.contains("</svg>"));
    }

    #[test]
    fn test_sequence_diagram_with_activations() {
        let mermaid = r#"sequenceDiagram
    participant Dev as Developer
    participant CI as CI Pipeline
    participant K8s as Kubernetes
    participant PD as PagerDuty
    participant Cat as Office Cat

    Dev->>CI: git push --force
    activate CI
    CI->>CI: run 4,000 tests
    CI-->>Dev: ✅ all green (suspicious)
    deactivate CI
    Dev->>K8s: deploy to prod
    activate K8s
    K8s-->>Dev: 200 OK
    deactivate K8s
    Note over Dev: leaves for lunch
    K8s->>PD: OOMKilled x47
    PD->>Dev: CALL CALL CALL
    Dev->>K8s: kubectl rollout undo
    K8s-->>Dev: phew
    Cat->>Dev: sits on keyboard
    Dev->>K8s: jjjjjjjjjjjjjjjj
"#;

        let result = render_mermaid_to_svg(mermaid, None);
        let svg = match result {
            Ok(svg) => svg,
            Err(err) => panic!("expected ok result, got error: {err}"),
        };
        assert!(svg.contains("<svg"));
        assert!(svg.contains("Developer"));
        assert!(svg.contains("Office Cat"));
        assert_eq!(
            svg.matches("stroke-width=\"0.8\"").count(),
            2,
            "both activation bars are drawn"
        );
    }

    #[test]
    fn test_sequence_diagram_extended_grammar() {
        let mermaid = r#"sequenceDiagram
    autonumber
    title Order lifecycle
    actor U as User
    box Payments
        participant API
        participant Bank
    end
    create participant Audit
    U->>+API: place order
    API-)Audit: log async
    API-xBank: charge (may be lost)
    Bank-->>-API: receipt
    U<<->>API: handshake
    note right of U: lowercase note keyword
    par fanout
        API->Audit: solid line no head
    and other branch
        API-->Bank: dashed line no head
    end
    critical settle
        API->>Bank: capture
    option timeout
        API->>U: retry later
    end
    break on failure
        API->>U: abort
    end
    rect rgb(200, 220, 255)
        U->>API: highlighted
    end
    destroy Audit
    autonumber off
    U->>API: unnumbered
"#;

        let result = render_mermaid_to_svg(mermaid, None);
        let svg = match result {
            Ok(svg) => svg,
            Err(err) => panic!("expected ok result, got error: {err}"),
        };
        assert!(svg.contains("Order lifecycle"));
        assert!(svg.contains("1. place order"));
        assert!(svg.contains(">unnumbered<"), "numbering stops after off");
        assert!(!svg.contains(". unnumbered"));
        for label in ["User", "API", "Bank", "Audit"] {
            assert!(svg.contains(label), "missing participant {label:?}");
        }
        for tab in [">par<", ">critical<", ">break<"] {
            assert!(svg.contains(tab), "missing fragment tab {tab:?}");
        }
        assert!(svg.contains("url(#seq_cross)"));
        assert!(svg.contains("url(#seq_open)"));
        assert!(svg.contains("url(#seq_arrow_rev)"));
    }

    #[test]
    fn test_sequence_fragment_inside_unclosed_box_keeps_its_end() {
        let mermaid = r#"sequenceDiagram
    box Group
    participant A
    alt c
    A->>A: inside
    end
    A->>A: after one
    A->>A: after two
    A->>A: after three
    end
"#;
        let svg = render_mermaid_to_svg(mermaid, None).expect("renders");
        let frame = svg
            .split("<rect ")
            .find(|r| r.contains("stroke-dasharray=\"3,3\""))
            .expect("fragment frame present");
        let height: f64 = frame
            .split("height=\"")
            .nth(1)
            .and_then(|s| s.split('"').next())
            .and_then(|s| s.parse().ok())
            .expect("frame height");
        assert!(
            height < 100.0,
            "fragment must close at its own `end`, got height {height}"
        );
    }

    #[test]
    fn test_sequence_activation_shorthand_and_keyword_boundaries() {
        let svg = render_mermaid_to_svg(
            "sequenceDiagram\n    Alice->>+John: hello\n    John-->>-Alice: hi",
            None,
        )
        .expect("activation shorthand renders");
        assert_eq!(
            svg.matches("stroke-width=\"0.8\"").count(),
            1,
            "the +/- pair draws one activation bar"
        );

        let err = render_mermaid_to_svg("sequenceDiagram\n    optimize the flow", None)
            .expect_err("prose starting with a fragment keyword is not a fragment");
        assert!(err.to_string().contains("optimize the flow"));
    }

    #[test]
    fn test_simple_timeline_diagram() {
        let mermaid = r#"timeline
    title History of Social Platforms
    2002 : LinkedIn
    2004 : Facebook
    2006 : Twitter"#;

        let result = render_mermaid_to_svg(mermaid, None);
        assert!(result.is_ok());
        let svg = result.unwrap();
        assert!(svg.contains("<svg"));
        assert!(svg.contains("</svg>"));
    }

    #[test]
    fn test_simple_journey_diagram() {
        let mermaid = r#"journey
    title My working day

    section Go to work
      Make tea: 5: Me
      Go upstairs: 3: Me

    section Go home
      Go downstairs: 5: Me"#;

        let result = render_mermaid_to_svg(mermaid, None);
        assert!(result.is_ok());
        let svg = result.unwrap();
        assert!(svg.contains("<svg"));
        assert!(svg.contains("</svg>"));
    }

    #[test]
    fn test_simple_mindmap_diagram() {
        let mermaid = r#"mindmap
  root((mindmap))
    Origins
      Long history
    Tooling
      Mermaid
      Rust"#;

        let result = render_mermaid_to_svg(mermaid, None);
        assert!(result.is_ok());
        let svg = result.unwrap();
        assert!(svg.contains("<svg"));
        assert!(svg.contains("</svg>"));
    }

    #[test]
    fn test_simple_quadrant_chart() {
        let mermaid = r#"quadrantChart
    title Reach and engagement
    x-axis Low Reach --> High Reach
    y-axis Low Engagement --> High Engagement
    quadrant-1 High impact
    quadrant-2 Viral
    quadrant-3 Niche
    quadrant-4 Broad but shallow
    \"Post A\": [0.3, 0.6]
    \"Post B\": [0.8, 0.2]"#;

        let result = render_mermaid_to_svg(mermaid, None);
        assert!(result.is_ok());
        let svg = result.unwrap();
        assert!(svg.contains("<svg"));
        assert!(svg.contains("</svg>"));
    }

    #[test]
    fn test_simple_xychart() {
        let mermaid = r#"xychart-beta
    title Demo
    x-axis 0 --> 10
    y-axis 0 --> 100
    line [5, 10, 20, 40]"#;

        let result = render_mermaid_to_svg(mermaid, None);
        assert!(result.is_ok());
        let svg = result.unwrap();
        assert!(svg.contains("<svg"));
        assert!(svg.contains("Demo"));
    }

    #[test]
    fn test_simple_sankey() {
        let mermaid = r#"sankey-beta
    A,B,10
    B,C,5
    B,D,5"#;

        let result = render_mermaid_to_svg(mermaid, None);
        assert!(result.is_ok());
        let svg = result.unwrap();
        assert!(svg.contains("<svg"));
        assert!(svg.contains("linearGradient"));
    }

    #[test]
    fn test_simple_radar() {
        let mermaid = r#"radar-beta
axis A, B, C
curve Series1 { 1, 2, 3 }"#;

        let result = render_mermaid_to_svg(mermaid, None);
        assert!(result.is_ok());
        let svg = result.unwrap();
        assert!(svg.contains("<svg"));
        assert!(svg.contains("radarGraticule"));
        assert!(svg.contains("Series1"));
    }

    #[test]
    fn test_simple_requirement_diagram() {
        let mermaid = r#"requirementDiagram
direction LR

requirement req1 {
    id: 1
    text: \"The system shall do something\"
    risk: high
    verifyMethod: test
}

element el1 {
    type: \"Subsystem\"
    docref: \"DOC-1\"
}

req1 - satisfies -> el1"#;

        let result = render_mermaid_to_svg(mermaid, None);
        let svg = match result {
            Ok(svg) => svg,
            Err(err) => panic!("expected ok result, got error: {err}"),
        };
        assert!(svg.contains("requirementDiagram"));
        assert!(svg.contains("req1"));
        assert!(svg.contains("el1"));
        assert!(svg.contains("&lt;&lt;satisfies&gt;&gt;") || svg.contains("<<satisfies>>"));
    }

    #[test]
    fn test_simple_kanban_diagram() {
        let mermaid = r#"kanban
Todo
    Task 1
    Task 2
Doing
    Task 3"#;

        let result = render_mermaid_to_svg(mermaid, None);
        let svg = match result {
            Ok(svg) => svg,
            Err(err) => panic!("expected ok result, got error: {err}"),
        };
        assert!(svg.contains("aria-roledescription=\"kanban\""));
        assert!(svg.contains("Todo"));
        assert!(svg.contains("Task 1"));
        assert!(svg.contains("Doing"));
        assert!(svg.contains("Task 3"));
    }

    #[test]
    fn test_simple_block_diagram() {
        let mermaid = r#"block-beta
A[\"A\"] --> B[\"B\"]"#;

        let result = render_mermaid_to_svg(mermaid, None);
        let svg = match result {
            Ok(svg) => svg,
            Err(err) => panic!("expected ok result, got error: {err}"),
        };
        assert!(svg.contains("aria-roledescription=\"block\""));
        assert!(svg.contains("id=\"A\""));
        assert!(svg.contains("id=\"B\""));
    }

    #[test]
    fn test_simple_gitgraph_diagram() {
        let mermaid = r#"gitGraph
    commit
    commit
    branch develop
    checkout develop
    commit
    checkout main
    merge develop"#;

        let result = render_mermaid_to_svg(mermaid, None);
        let svg = match result {
            Ok(svg) => svg,
            Err(err) => panic!("expected ok result, got error: {err}"),
        };
        assert!(svg.contains("aria-roledescription=\"gitGraph\""));
        assert!(svg.contains("main"));
        assert!(svg.contains("develop"));
    }

    // LOCAL ADDITION (vendoring): c4 is dispatched (lib.rs) but was the one
    // diagram type without a smoke test next to its siblings above. A c4 render
    // failure only degrades to the code-block fallback, but the dispatch+render
    // path should still be exercised so a regression that panics is caught.
    #[test]
    fn test_simple_c4_diagram() {
        let mermaid = r#"C4Context
title System Context diagram
Person(customer, "Customer", "A user of the system.")
System(system, "Internet Banking", "Lets customers view information.")
Rel(customer, system, "Uses")"#;

        let result = render_mermaid_to_svg(mermaid, None);
        let svg = match result {
            Ok(svg) => svg,
            Err(err) => panic!("expected ok result, got error: {err}"),
        };
        assert!(svg.contains("<svg"));
        assert!(svg.contains("</svg>"));
        assert!(svg.contains("Customer"));
    }
}
