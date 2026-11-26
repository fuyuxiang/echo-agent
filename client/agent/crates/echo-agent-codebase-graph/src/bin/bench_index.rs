//! Benchmark binary for index building.

use std::path::Path;
use std::time::Instant;

use echo_agent_codebase_graph::{IndexBuilder, LanguageRegistry};

// Use mimalloc for faster allocation in multi-threaded workloads
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let path = if let Some(p) = args.get(1) {
        p.clone()
    } else if let Ok(p) =
        std::env::var("BENCH_REPO_ROOT").or_else(|_| std::env::var("ECHO_AGENT_ROOT"))
    {
        p
    } else {
        eprintln!("Usage: bench_index <path>");
        eprintln!("Or set BENCH_REPO_ROOT to a large checkout to bench against");
        std::process::exit(1);
    };

    // First, verify all queries compile
    println!("Verifying query compilation...");
    let registry = LanguageRegistry::new();
    for ext in &["ts", "tsx", "js", "jsx", "rs", "go", "py"] {
        match registry.for_extension(ext) {
            Some(config) => match config.compile_query() {
                Ok(query) => {
                    println!("  .{}: OK ({} patterns)", ext, query.pattern_count());
                }
                Err(e) => {
                    println!("  .{}: FAILED - {:?}", ext, e);
                }
            },
            None => println!("  .{}: NOT SUPPORTED", ext),
        }
    }
    println!();

    let root_path = Path::new(&path);

    println!("Building index for: {}", root_path.display());
    let start = Instant::now();

    let index = IndexBuilder::new()
        .build(root_path)
        .expect("Failed to build index");

    let elapsed = start.elapsed();
    let (file_count, defs, refs) = index.stats();

    println!("Files indexed: {}", file_count);
    println!(
        "Indexed {} definitions, {} references in {:?}",
        defs, refs, elapsed
    );
    println!("Aliases: {}", index.alias_count());
    println!(
        "Files/sec: {:.0}",
        file_count as f64 / elapsed.as_secs_f64()
    );
}
