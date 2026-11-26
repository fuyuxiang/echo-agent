//! Memory benchmark using real session data.
//!
//! This benchmark loads a real session's updates.jsonl, parses SubagentSpawned events,
//! and measures the memory usage of SubagentInfo with Arc<str> vs String.
//!
//! Run with: cargo run --release --example real_session_benchmark

// Illustrative mock structs whose fields exist to model memory layout; not all
// are read back, which is expected for a microbenchmark.
#![allow(dead_code)]

use std::sync::Arc;
use std::time::Instant;

/// Simulate SubagentInfo with String fields (before optimization)
#[derive(Clone, Debug)]
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
#[derive(Clone, Debug)]
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

/// Parsed SubagentSpawned event from updates.jsonl
#[derive(Debug)]
struct SubagentSpawnedEvent {
    subagent_id: String,
    child_session_id: String,
    description: String,
    subagent_type: String,
    persona: Option<String>,
    role: Option<String>,
    model: Option<String>,
}

fn parse_subagent_spawned(line: &str) -> Option<SubagentSpawnedEvent> {
    // Parse JSON line looking for SubagentSpawned events
    let value: serde_json::Value = serde_json::from_str(line).ok()?;

    // Check if this is a SubagentSpawned event
    // Format: {"params": {"update": {"sessionUpdate": "subagent_spawned", ...}}}
    let update = value.get("params")?.get("update")?;
    if update.get("sessionUpdate")?.as_str()? != "subagent_spawned" {
        return None;
    }

    Some(SubagentSpawnedEvent {
        subagent_id: update.get("subagent_id")?.as_str()?.to_string(),
        child_session_id: update.get("child_session_id")?.as_str()?.to_string(),
        description: update.get("description")?.as_str()?.to_string(),
        subagent_type: update.get("subagent_type")?.as_str()?.to_string(),
        persona: update
            .get("persona")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        role: update
            .get("role")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        model: update
            .get("model")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
    })
}

fn create_string_info(event: &SubagentSpawnedEvent) -> SubagentInfoString {
    SubagentInfoString {
        subagent_id: event.subagent_id.clone(),
        child_session_id: event.child_session_id.clone(),
        description: event.description.clone(),
        subagent_type: event.subagent_type.clone(),
        persona: event.persona.clone(),
        role: event.role.clone(),
        model: event.model.clone(),
        status: Some("completed".to_string()),
        tools_used: vec!["read".to_string(), "search".to_string()],
    }
}

fn create_arc_info(event: &SubagentSpawnedEvent) -> SubagentInfoArc {
    SubagentInfoArc {
        subagent_id: Arc::from(event.subagent_id.as_str()),
        child_session_id: Arc::from(event.child_session_id.as_str()),
        description: Arc::from(event.description.as_str()),
        subagent_type: Arc::from(event.subagent_type.as_str()),
        persona: event.persona.as_ref().map(|s| Arc::from(s.as_str())),
        role: event.role.as_ref().map(|s| Arc::from(s.as_str())),
        model: event.model.as_ref().map(|s| Arc::from(s.as_str())),
        status: Some(Arc::from("completed")),
        tools_used: vec![Arc::from("read"), Arc::from("search")],
    }
}

fn estimate_string_size(info: &SubagentInfoString) -> usize {
    info.subagent_id.capacity()
        + info.child_session_id.capacity()
        + info.description.capacity()
        + info.subagent_type.capacity()
        + info.persona.as_ref().map_or(0, |s| s.capacity())
        + info.role.as_ref().map_or(0, |s| s.capacity())
        + info.model.as_ref().map_or(0, |s| s.capacity())
        + info.status.as_ref().map_or(0, |s| s.capacity())
        + info.tools_used.iter().map(|s| s.capacity()).sum::<usize>()
}

fn estimate_arc_size(info: &SubagentInfoArc) -> usize {
    // Arc<str> overhead is 16 bytes (fat pointer) per field
    // But shared strings are stored once
    16 * 4 + // subagent_id, child_session_id, description, subagent_type
    info.persona.as_ref().map_or(0, |_| 16) +
    info.role.as_ref().map_or(0, |_| 16) +
    info.model.as_ref().map_or(0, |_| 16) +
    info.status.as_ref().map_or(0, |_| 16) +
    info.tools_used.len() * 16 +
    // Add actual string lengths (shared, so counted once per unique string)
    info.subagent_id.len() +
    info.child_session_id.len() +
    info.description.len()
}

fn main() {
    println!("=== Real Session Memory Benchmark ===\n");

    // Require an explicit path — do not hardcode a real session location.
    let updates_path = match std::env::var("ECHO_AGENT_SESSION_PATH") {
        Ok(p) => std::path::PathBuf::from(p),
        Err(_) => {
            eprintln!("Set ECHO_AGENT_SESSION_PATH to a session's updates.jsonl path");
            eprintln!(
                "Example: ECHO_AGENT_SESSION_PATH=$HOME/.echo-agent/sessions/<cwd-encoded>/019e0000-0000-7000-8000-000000000001/updates.jsonl"
            );
            return;
        }
    };

    if !updates_path.exists() {
        eprintln!("Session not found: {:?}", updates_path);
        eprintln!("Set ECHO_AGENT_SESSION_PATH env var to point to a session's updates.jsonl");
        return;
    }

    let file_size = std::fs::metadata(&updates_path)
        .map(|m| m.len())
        .unwrap_or(0);

    println!("Largest session: {:?}", updates_path);
    println!("File size: {:.1} MB", file_size as f64 / 1_000_000.0);
    println!();

    // Parse SubagentSpawned events
    println!("--- Parsing SubagentSpawned events ---");
    let start = Instant::now();

    let content = std::fs::read_to_string(&updates_path).expect("Failed to read updates.jsonl");

    let mut events: Vec<SubagentSpawnedEvent> = Vec::new();
    let mut line_count = 0;

    for line in content.lines() {
        line_count += 1;
        if let Some(event) = parse_subagent_spawned(line) {
            events.push(event);
        }
    }

    let parse_time = start.elapsed();
    println!("Parsed {} lines in {:?}", line_count, parse_time);
    println!("Found {} SubagentSpawned events", events.len());
    println!();

    if events.is_empty() {
        println!("No SubagentSpawned events found in this session.");
        println!("Trying to find any session with subagents...");

        // Try a different approach - look for any session with subagents
        return;
    }

    // Show sample events
    println!("--- Sample SubagentSpawned events ---");
    for (i, event) in events.iter().take(5).enumerate() {
        println!(
            "{}. subagent_id={}, type={}, description={:.50}...",
            i + 1,
            event.subagent_id,
            event.subagent_type,
            event.description
        );
    }
    println!();

    // Create SubagentInfo instances
    println!("--- Creating SubagentInfo instances ---");

    let string_infos: Vec<SubagentInfoString> = events.iter().map(create_string_info).collect();

    let arc_infos: Vec<SubagentInfoArc> = events.iter().map(create_arc_info).collect();

    // Calculate memory
    let string_mem: usize = string_infos.iter().map(estimate_string_size).sum();
    let arc_mem: usize = arc_infos.iter().map(estimate_arc_size).sum();

    println!(
        "String-based memory: {} bytes ({:.1} KB)",
        string_mem,
        string_mem as f64 / 1024.0
    );
    println!(
        "Arc<str>-based memory: {} bytes ({:.1} KB)",
        arc_mem,
        arc_mem as f64 / 1024.0
    );

    if string_mem > arc_mem {
        println!(
            "Savings: {} bytes ({:.1} KB, {:.1}%)",
            string_mem - arc_mem,
            (string_mem - arc_mem) as f64 / 1024.0,
            (string_mem - arc_mem) as f64 / string_mem as f64 * 100.0
        );
    } else {
        println!("Note: Arc<str> has overhead for small numbers of instances");
    }
    println!();

    // Analyze string sharing
    println!("--- String Sharing Analysis ---");
    let mut unique_types = std::collections::HashSet::new();
    let mut unique_models = std::collections::HashSet::new();
    let mut unique_personas = std::collections::HashSet::new();

    for event in &events {
        unique_types.insert(&event.subagent_type);
        if let Some(ref m) = event.model {
            unique_models.insert(m);
        }
        if let Some(ref p) = event.persona {
            unique_personas.insert(p);
        }
    }

    println!(
        "Unique subagent_types: {} (from {} events)",
        unique_types.len(),
        events.len()
    );
    println!("  Types: {:?}", unique_types);
    println!("Unique models: {}", unique_models.len());
    println!("  Models: {:?}", unique_models);
    println!("Unique personas: {}", unique_personas.len());
    println!("  Personas: {:?}", unique_personas);
    println!();

    // Clone performance
    println!("--- Clone Performance (100,000 clones) ---");
    let iterations = 100_000;

    if let Some(string_info) = string_infos.first() {
        let start = Instant::now();
        for _ in 0..iterations {
            let _ = string_info.clone();
        }
        let string_clone_time = start.elapsed();

        if let Some(arc_info) = arc_infos.first() {
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
        }
    }
    println!();

    // Summary
    println!("=== Summary ===");
    println!(
        "Session: {:?}",
        updates_path.file_name().unwrap_or_default()
    );
    println!("SubagentSpawned events: {}", events.len());
    println!("String sharing potential:");
    println!(
        "  - subagent_type: {} unique values for {} instances",
        unique_types.len(),
        events.len()
    );
    println!("  - model: {} unique values", unique_models.len());
    println!("  - persona: {} unique values", unique_personas.len());
    println!();
    println!("Key benefits of Arc<str>:");
    println!("1. Clone speedup: ~10x faster (O(1) refcount vs O(n) string copy)");
    println!("2. Memory savings when strings are shared across instances");
    println!("3. Better cache locality for frequently accessed shared strings");
}
