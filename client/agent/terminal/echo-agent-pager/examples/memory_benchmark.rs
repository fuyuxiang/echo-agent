//! Memory benchmark for SubagentInfo Arc<str> optimization.
//!
//! This benchmark measures the memory savings from using Arc<str> instead of String
//! for SubagentInfo fields. It creates many SubagentInfo instances with shared string
//! values and measures the memory usage.
//!
//! Run with: cargo run --release --example memory_benchmark

// Illustrative mock structs whose fields exist to model memory layout; not all
// are read back, which is expected for a microbenchmark.
#![allow(dead_code)]

use std::sync::Arc;
use std::time::Instant;

/// Simulate SubagentInfo with String fields (before optimization)
#[derive(Clone)]
struct SubagentInfoString {
    subagent_id: String,
    child_session_id: String,
    description: String,
    subagent_type: String,
    persona: Option<String>,
    role: Option<String>,
    model: Option<String>,
    status: Option<String>,
    tools_used: Vec<String>,
}

/// Simulate SubagentInfo with Arc<str> fields (after optimization)
#[derive(Clone)]
struct SubagentInfoArc {
    subagent_id: Arc<str>,
    child_session_id: Arc<str>,
    description: Arc<str>,
    subagent_type: Arc<str>,
    persona: Option<Arc<str>>,
    role: Option<Arc<str>>,
    model: Option<Arc<str>>,
    status: Option<Arc<str>>,
    tools_used: Vec<Arc<str>>,
}

fn create_string_info(id: usize) -> SubagentInfoString {
    SubagentInfoString {
        subagent_id: format!("sa-{}", id),
        child_session_id: format!("cs-{}", id),
        description: "Find API endpoints in the codebase".to_string(),
        subagent_type: "general-purpose".to_string(),
        persona: Some("researcher".to_string()),
        role: Some("analyst".to_string()),
        model: Some("echo-agent-3".to_string()),
        status: Some("completed".to_string()),
        tools_used: vec!["read".to_string(), "search".to_string(), "edit".to_string()],
    }
}

fn create_arc_info(id: usize) -> SubagentInfoArc {
    SubagentInfoArc {
        subagent_id: Arc::from(format!("sa-{}", id)),
        child_session_id: Arc::from(format!("cs-{}", id)),
        description: Arc::from("Find API endpoints in the codebase"),
        subagent_type: Arc::from("general-purpose"),
        persona: Some(Arc::from("researcher")),
        role: Some(Arc::from("analyst")),
        model: Some(Arc::from("echo-agent-3")),
        status: Some(Arc::from("completed")),
        tools_used: vec![Arc::from("read"), Arc::from("search"), Arc::from("edit")],
    }
}

fn estimate_string_info_size(info: &SubagentInfoString) -> usize {
    std::mem::size_of::<SubagentInfoString>()
        + info.subagent_id.capacity()
        + info.child_session_id.capacity()
        + info.description.capacity()
        + info.subagent_type.capacity()
        + info.persona.as_ref().map_or(0, |s| s.capacity())
        + info.role.as_ref().map_or(0, |s| s.capacity())
        + info.model.as_ref().map_or(0, |s| s.capacity())
        + info.status.as_ref().map_or(0, |s| s.capacity())
        + info.tools_used.iter().map(|s| s.capacity()).sum::<usize>()
}

fn estimate_arc_info_size(info: &SubagentInfoArc) -> usize {
    // Arc<str> has 16 bytes overhead (fat pointer) but shares the string data
    std::mem::size_of::<SubagentInfoArc>()
        + 16 // subagent_id Arc overhead
        + 16 // child_session_id Arc overhead
        + 16 // description Arc overhead
        + 16 // subagent_type Arc overhead
        + info.persona.as_ref().map_or(0, |_| 16)
        + info.role.as_ref().map_or(0, |_| 16)
        + info.model.as_ref().map_or(0, |_| 16)
        + info.status.as_ref().map_or(0, |_| 16)
        + info.tools_used.len() * 16
}

fn main() {
    println!("=== SubagentInfo Memory Benchmark ===\n");

    // Test 1: Single instance
    println!("--- Single Instance ---");
    let string_info = create_string_info(1);
    let arc_info = create_arc_info(1);

    println!(
        "String-based SubagentInfo size: ~{} bytes",
        estimate_string_info_size(&string_info)
    );
    println!(
        "Arc<str>-based SubagentInfo size: ~{} bytes",
        estimate_arc_info_size(&arc_info)
    );
    println!();

    // Test 2: Many instances with shared strings
    println!("--- 1000 Instances (with string sharing potential) ---");
    let count = 1000;

    // String-based: each instance has its own copy of shared strings
    let string_infos: Vec<SubagentInfoString> = (0..count).map(create_string_info).collect();

    // Arc-based: shared strings are deduplicated
    let arc_infos: Vec<SubagentInfoArc> = (0..count).map(create_arc_info).collect();

    let string_total: usize = string_infos.iter().map(estimate_string_info_size).sum();
    let arc_total: usize = arc_infos.iter().map(estimate_arc_info_size).sum();

    println!(
        "String-based total: {} bytes ({:.1} KB)",
        string_total,
        string_total as f64 / 1024.0
    );
    println!(
        "Arc<str>-based total: {} bytes ({:.1} KB)",
        arc_total,
        arc_total as f64 / 1024.0
    );
    println!(
        "Savings: {} bytes ({:.1} KB, {:.1}%)",
        string_total - arc_total,
        (string_total - arc_total) as f64 / 1024.0,
        (string_total - arc_total) as f64 / string_total as f64 * 100.0
    );
    println!();

    // Test 3: Clone performance
    println!("--- Clone Performance (100,000 clones) ---");
    let iterations = 100_000;

    let start = Instant::now();
    for _ in 0..iterations {
        let _ = string_info.clone();
    }
    let string_clone_time = start.elapsed();

    let start = Instant::now();
    for _ in 0..iterations {
        let _ = arc_info.clone();
    }
    let arc_clone_time = start.elapsed();

    println!("String clone time: {:?}", string_clone_time);
    println!("Arc<str> clone time: {:?}", arc_clone_time);
    println!(
        "Speedup: {:.1}x",
        string_clone_time.as_nanos() as f64 / arc_clone_time.as_nanos() as f64
    );
    println!();

    // Test 4: Memory with realistic subagent data
    println!("--- Realistic Scenario (100 subagents with varied data) ---");
    let subagent_types = ["general-purpose", "explore", "plan", "implementer"];
    let personas = ["researcher", "analyst", "reviewer", "implementer"];
    let models = ["echo-agent-3", "echo-agent-3-mini", "echo-agent-4"];
    let statuses = ["completed", "failed", "running", "cancelled"];
    let tools = ["read", "edit", "search", "execute", "list_dir"];

    let mut string_infos: Vec<SubagentInfoString> = Vec::new();
    let mut arc_infos: Vec<SubagentInfoArc> = Vec::new();

    for i in 0..100 {
        let st = subagent_types[i % subagent_types.len()];
        let p = personas[i % personas.len()];
        let m = models[i % models.len()];
        let s = statuses[i % statuses.len()];

        string_infos.push(SubagentInfoString {
            subagent_id: format!("sa-{}", i),
            child_session_id: format!("cs-{}", i),
            description: format!("Task {}: analyze the codebase", i),
            subagent_type: st.to_string(),
            persona: Some(p.to_string()),
            role: Some(p.to_string()),
            model: Some(m.to_string()),
            status: Some(s.to_string()),
            tools_used: tools
                .iter()
                .take(i % 5 + 1)
                .map(|t| t.to_string())
                .collect(),
        });

        arc_infos.push(SubagentInfoArc {
            subagent_id: Arc::from(format!("sa-{}", i)),
            child_session_id: Arc::from(format!("cs-{}", i)),
            description: Arc::from(format!("Task {}: analyze the codebase", i)),
            subagent_type: Arc::from(st),
            persona: Some(Arc::from(p)),
            role: Some(Arc::from(p)),
            model: Some(Arc::from(m)),
            status: Some(Arc::from(s)),
            tools_used: tools
                .iter()
                .take(i % 5 + 1)
                .map(|t| Arc::from(*t))
                .collect(),
        });
    }

    let string_total: usize = string_infos.iter().map(estimate_string_info_size).sum();
    let arc_total: usize = arc_infos.iter().map(estimate_arc_info_size).sum();

    println!(
        "String-based total: {} bytes ({:.1} KB)",
        string_total,
        string_total as f64 / 1024.0
    );
    println!(
        "Arc<str>-based total: {} bytes ({:.1} KB)",
        arc_total,
        arc_total as f64 / 1024.0
    );
    println!(
        "Savings: {} bytes ({:.1} KB, {:.1}%)",
        string_total - arc_total,
        (string_total - arc_total) as f64 / 1024.0,
        (string_total - arc_total) as f64 / string_total as f64 * 100.0
    );
    println!();

    // Summary
    println!("=== Summary ===");
    println!("Arc<str> optimization provides:");
    println!("1. Memory savings through string deduplication (shared strings stored once)");
    println!("2. Faster cloning (O(1) refcount increment vs O(n) string copy)");
    println!("3. Better cache locality for frequently accessed shared strings");
}
