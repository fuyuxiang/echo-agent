//! Integration tests for the public render path through the default engine.
//!
//! These use only the public API (`default_engine` + `render_checked`) so they
//! exercise the real, always-compiled dagre-based engine end to end.

use echo_agent_mermaid::{
    MermaidError, MermaidTheme, RenderLimits, RenderParams, default_engine, render_checked,
};

#[test]
fn default_engine_renders_a_flowchart() {
    let diagram = render_checked(
        default_engine().as_ref(),
        "flowchart LR\n  A[Start] --> B[Finish]",
        &RenderParams::default(),
        &RenderLimits::default(),
    )
    .expect("the default engine must render a flowchart");
    assert!(diagram.width_px > 0 && diagram.height_px > 0);
    let img = image::load_from_memory(&diagram.png).expect("output must be a valid PNG");
    assert_eq!(img.width(), diagram.width_px);
    assert_eq!(img.height(), diagram.height_px);
}

/// Untrusted-input contract through the real engine: a panic would surface as
/// `MermaidError::Panic`, which we assert against. Unparseable input may return
/// other errors (which degrade to the code-block fallback), never a panic.
#[test]
fn garbage_input_is_isolated_not_panicked() {
    let limits = RenderLimits::default();
    let params = RenderParams::default();
    for garbage in ["", "@@@@", "%% comment only", "????", "\u{0}\u{1}\u{2}"] {
        let out = render_checked(default_engine().as_ref(), garbage, &params, &limits);
        assert!(
            !matches!(out, Err(MermaidError::Panic(_))),
            "engine panicked on {garbage:?}: {out:?}"
        );
    }
}

#[test]
fn sequence_diagram_with_activations_renders_to_png() {
    const SOURCE: &str = "sequenceDiagram\n\
        participant Dev as Developer\n\
        participant CI as CI Pipeline\n\
        participant K8s as Kubernetes\n\
        participant PD as PagerDuty\n\
        participant Cat as Office Cat\n\
        Dev->>CI: git push --force\n\
        activate CI\n\
        CI->>CI: run 4,000 tests\n\
        CI-->>Dev: \u{2705} all green (suspicious)\n\
        deactivate CI\n\
        Dev->>K8s: deploy to prod\n\
        activate K8s\n\
        K8s-->>Dev: 200 OK\n\
        deactivate K8s\n\
        Note over Dev: leaves for lunch\n\
        K8s->>PD: OOMKilled x47\n\
        PD->>Dev: CALL CALL CALL\n\
        Dev->>K8s: kubectl rollout undo\n\
        K8s-->>Dev: phew\n\
        Cat->>Dev: sits on keyboard\n\
        Dev->>K8s: jjjjjjjjjjjjjjjj\n";
    let diagram = render_checked(
        default_engine().as_ref(),
        SOURCE,
        &RenderParams::default(),
        &RenderLimits::default(),
    )
    .expect("a sequence diagram with activations must render");
    let img = image::load_from_memory(&diagram.png).expect("output must be a valid PNG");
    assert_eq!(img.width(), diagram.width_px);
    assert_eq!(img.height(), diagram.height_px);
}

/// Regression: a class diagram using quoted cardinalities, stereotypes,
/// generics, and the full relation set must render instead of erroring
/// (previously failed with "Unrecognized classDiagram line" on
/// `Owner "1" o-- "0..*" Animal`).
#[test]
fn class_diagram_with_cardinalities_renders() {
    let src = "classDiagram\n    direction TB\n    class Animal {\n        <<abstract>>\n        #String name\n        +makeSound()* String\n    }\n    class Owner {\n        +List~Animal~ pets\n        +adopt(Animal pet) void\n    }\n    Animal <|-- Dog : extends\n    Feedable <|.. Animal : implements\n    Owner \"1\" o-- \"0..*\" Animal : owns\n    Dog \"1\" --> \"0..1\" Ball : plays with\n    Veterinarian ..> Animal : examines\n    HealthReport --* Animal : belongs to";
    let engine = default_engine();
    let diagram = render_checked(
        engine.as_ref(),
        src,
        &RenderParams {
            theme: MermaidTheme::Dark,
            ..Default::default()
        },
        &RenderLimits::default(),
    )
    .expect("class diagram with quoted cardinalities must render");
    assert!(!diagram.png.is_empty());
}

/// Same source + params must produce identical PNG bytes (the engine's text
/// metrics are font-free, so rendering is deterministic).
#[test]
fn rendering_is_deterministic() {
    let engine = default_engine();
    let params = RenderParams::default();
    let a = render_checked(
        engine.as_ref(),
        "flowchart LR\nA-->B-->C",
        &params,
        &RenderLimits::default(),
    )
    .expect("render a");
    let b = render_checked(
        engine.as_ref(),
        "flowchart LR\nA-->B-->C",
        &params,
        &RenderLimits::default(),
    )
    .expect("render b");
    assert_eq!(a.png, b.png, "same input must yield identical PNG bytes");
}

/// End-to-end guard for the word-wrap fix: the real user diagram (a flowchart of
/// long Python identifiers) must reach the rendered SVG with its identifiers
/// intact, never hard-sliced mid-identifier. Renders through the same
/// `mermaid_to_svg::render_mermaid_to_svg` path the pager uses.
#[test]
fn long_identifier_node_labels_survive_intact_in_svg() {
    let src = "flowchart TB
    subgraph main [resi_local.py / resi.py]
        nav[st.navigation]
        mark[mark_filter_restore_context]
        global[render_global_sidebar]
        sidebar[render_page_sidebar_filters]
        page[page.run]
    end
    nav --> mark --> global --> sidebar --> page";
    let svg = mermaid_to_svg::render_mermaid_to_svg(src, None)
        .expect("the real user flowchart must render to SVG");
    // Each long identifier appears as a COMPLETE single tspan; a slice (at any
    // offset) could never produce the whole identifier as one tspan's content.
    assert!(
        svg.contains(">mark_filter_restore_context</tspan>"),
        "{svg}"
    );
    assert!(
        svg.contains(">render_page_sidebar_filters</tspan>"),
        "{svg}"
    );
}

/// A categorical-x-axis xychart with two `line` series must render to a decodable
/// PNG through the full `[Open Image]` path (source -> SVG -> raster), on both
/// themes. The categorical x-axis (no `-->`) previously failed to open.
#[test]
fn categorical_xychart_with_two_series_renders_to_png() {
    const SOURCE: &str = "xychart-beta\n    \
        title \"Weekly active users by region\"\n    \
        x-axis [\"Jan\", \"Feb\", \"Mar\", \"Apr\", \"May\", \"Jun\", \"Jul\", \"Aug\", \"Sep\", \"Oct\", \"Nov\", \"Dec\"]\n    \
        y-axis \"% of users\" 0 --> 40\n    \
        line [20.3, 22.6, 24.2, 24.3, 26.2, 27.2, 32.4, 31.9, 31.4, 31.1, 33.6, 34.3]\n    \
        line [3.2, 6.3, 10.0, 9.4, 11.1, 10.7, 15.3, 13.4, 13.5, 12.5, 15.4, 15.8]";
    let engine = default_engine();
    for theme in [MermaidTheme::Light, MermaidTheme::Dark] {
        let diagram = render_checked(
            engine.as_ref(),
            SOURCE,
            &RenderParams {
                theme,
                ..Default::default()
            },
            &RenderLimits::default(),
        )
        .unwrap_or_else(|e| panic!("categorical xychart must render ({theme:?}): {e}"));
        let img = image::load_from_memory(&diagram.png).expect("output must be a valid PNG");
        assert_eq!(img.width(), diagram.width_px);
        assert_eq!(img.height(), diagram.height_px);
        assert!(diagram.width_px > 0 && diagram.height_px > 0);
    }
}
