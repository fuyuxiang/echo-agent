//! CLI tool for code graph navigation.
//!
//! Provides go-to-definition and go-to-references functionality.
//!
//! # Usage
//!
//! ```bash
//! # Build the index for a repository
//! code-graph index /path/to/repo
//!
//! # Build the index with custom cache location
//! code-graph index /path/to/repo --cache /path/to/cache.bin
//!
//! # Go to definition (by position)
//! code-graph definition /path/to/repo --file src/main.rs --row 10 --col 15
//!
//! # Go to definition (by symbol name)
//! code-graph definition /path/to/repo --symbol MyStruct
//!
//! # Go to references (by position)
//! code-graph references /path/to/repo --file src/main.rs --row 10 --col 15
//!
//! # Go to references (by symbol name)
//! code-graph references /path/to/repo --symbol MyStruct
//!
//! # Show index statistics
//! code-graph stats /path/to/repo
//! ```

use std::path::{Path, PathBuf};
use std::time::Instant;

use clap::{Parser, Subcommand};

use echo_agent_codebase_graph::{
    IndexBuilder, Navigator, ScopeGraphIndex, get_cache_path, load_index, save_index,
};

// Use mimalloc for faster allocation
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

#[derive(Parser)]
#[command(name = "code-graph")]
#[command(author, version, about = "High-performance code navigation tool", long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Build or rebuild the index for a repository
    Index {
        /// Path to the repository
        path: PathBuf,
        /// Custom cache file path (default: <repo>/.goto_index.bin)
        #[arg(short, long)]
        cache: Option<PathBuf>,
        /// Force rebuild even if cache exists
        #[arg(short, long)]
        force: bool,
        /// Number of threads to use
        #[arg(short, long)]
        threads: Option<usize>,
    },

    /// Go to definition for a symbol
    Definition {
        /// Path to the repository
        path: PathBuf,
        /// Custom cache file path (default: <repo>/.goto_index.bin)
        #[arg(long)]
        cache: Option<PathBuf>,
        /// File path (for position-based lookup)
        #[arg(short, long)]
        file: Option<PathBuf>,
        /// Row number (1-indexed, for position-based lookup)
        #[arg(short, long)]
        row: Option<usize>,
        /// Column number (1-indexed, for position-based lookup)
        #[arg(short, long)]
        col: Option<usize>,
        /// Symbol name (for direct lookup)
        #[arg(short, long)]
        symbol: Option<String>,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },

    /// Go to references for a symbol
    References {
        /// Path to the repository
        path: PathBuf,
        /// Custom cache file path (default: <repo>/.goto_index.bin)
        #[arg(long)]
        cache: Option<PathBuf>,
        /// File path (for position-based lookup)
        #[arg(short, long)]
        file: Option<PathBuf>,
        /// Row number (1-indexed, for position-based lookup)
        #[arg(short, long)]
        row: Option<usize>,
        /// Column number (1-indexed, for position-based lookup)
        #[arg(short, long)]
        col: Option<usize>,
        /// Symbol name (for direct lookup)
        #[arg(short, long)]
        symbol: Option<String>,
        /// Include definition in results
        #[arg(long)]
        include_definition: bool,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },

    /// Show index statistics
    Stats {
        /// Path to the repository
        path: PathBuf,
        /// Custom cache file path (default: <repo>/.goto_index.bin)
        #[arg(long)]
        cache: Option<PathBuf>,
    },
}

fn main() {
    let cli = Cli::parse();

    match cli.command {
        Commands::Index {
            path,
            cache,
            force,
            threads,
        } => {
            cmd_index(&path, cache.as_deref(), force, threads);
        }
        Commands::Definition {
            path,
            cache,
            file,
            row,
            col,
            symbol,
            json,
        } => {
            cmd_definition(&path, cache.as_deref(), file, row, col, symbol, json);
        }
        Commands::References {
            path,
            cache,
            file,
            row,
            col,
            symbol,
            include_definition,
            json,
        } => {
            cmd_references(
                &path,
                cache.as_deref(),
                file,
                row,
                col,
                symbol,
                include_definition,
                json,
            );
        }
        Commands::Stats { path, cache } => {
            cmd_stats(&path, cache.as_deref());
        }
    }
}

/// Get the effective cache path - use custom if provided, otherwise default.
fn effective_cache_path(repo_path: &Path, custom_cache: Option<&Path>) -> PathBuf {
    custom_cache
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| get_cache_path(repo_path))
}

/// Load index from cache or build if necessary.
fn load_or_build_index(repo_path: &Path, cache_path: &Path) -> ScopeGraphIndex {
    if let Ok(index) = load_index(cache_path) {
        println!("Loaded index from cache: {}", cache_path.display());
        return index;
    }

    println!("Building index for: {}", repo_path.display());
    let start = Instant::now();

    let index = IndexBuilder::new()
        .build(repo_path)
        .expect("Failed to build index");

    let elapsed = start.elapsed();
    let (files, defs, refs) = index.stats();
    println!(
        "Built index: {} files, {} defs, {} refs in {:?}",
        files, defs, refs, elapsed
    );

    // Save to cache
    if let Err(e) = save_index(cache_path, &index) {
        println!("Warning: Failed to save cache: {}", e);
    } else {
        println!("Saved cache to: {}", cache_path.display());
    }

    index
}

fn cmd_index(path: &Path, custom_cache: Option<&Path>, _force: bool, threads: Option<usize>) {
    let cache_path = effective_cache_path(path, custom_cache);

    println!("Building index for: {}", path.display());
    let start = Instant::now();

    let mut builder = IndexBuilder::new();
    if let Some(t) = threads {
        builder = builder.with_threads(t);
    }

    let index = builder.build(path).expect("Failed to build index");

    let elapsed = start.elapsed();
    let (files, defs, refs) = index.stats();

    println!("Index built successfully!");
    println!("  Files indexed: {}", files);
    println!("  Definitions:   {}", defs);
    println!("  References:    {}", refs);
    println!("  Aliases:       {}", index.alias_count());
    println!("  Time:          {:?}", elapsed);

    // Always save when explicitly indexing
    if let Err(e) = save_index(&cache_path, &index) {
        println!("Error saving cache: {}", e);
        std::process::exit(1);
    } else {
        println!("  Cache saved:   {}", cache_path.display());
    }
}

fn cmd_definition(
    repo_path: &Path,
    custom_cache: Option<&Path>,
    file: Option<PathBuf>,
    row: Option<usize>,
    col: Option<usize>,
    symbol: Option<String>,
    json: bool,
) {
    let cache_path = effective_cache_path(repo_path, custom_cache);
    let index = load_or_build_index(repo_path, &cache_path);
    let navigator = Navigator::new(index);

    let result = match (file, row, col, symbol) {
        // Position-based lookup
        (Some(file_path), Some(r), Some(c), _) => {
            let abs_path = if file_path.is_absolute() {
                file_path
            } else {
                repo_path.join(&file_path)
            };

            match navigator.goto_definition(&abs_path, r, c) {
                Ok(r) => r,
                Err(e) => {
                    println!("Error: {}", e);
                    std::process::exit(1);
                }
            }
        }
        // Symbol-based lookup
        (_, _, _, Some(sym)) => navigator.goto_definition_by_name(&sym, None),
        _ => {
            println!("Error: Must provide either --file, --row, --col OR --symbol");
            std::process::exit(1);
        }
    };

    if json {
        print_json(&result);
    } else {
        println!("Symbol: {}", result.symbol);
        println!("Definitions ({}):", result.locations.len());
        for loc in &result.locations {
            println!("  {}:{}", loc.path, loc.line);
        }
    }
}

fn cmd_references(
    repo_path: &Path,
    custom_cache: Option<&Path>,
    file: Option<PathBuf>,
    row: Option<usize>,
    col: Option<usize>,
    symbol: Option<String>,
    include_definition: bool,
    json: bool,
) {
    let cache_path = effective_cache_path(repo_path, custom_cache);
    let index = load_or_build_index(repo_path, &cache_path);
    let navigator = Navigator::new(index);

    let result = match (file, row, col, symbol) {
        // Position-based lookup
        (Some(file_path), Some(r), Some(c), _) => {
            let abs_path = if file_path.is_absolute() {
                file_path
            } else {
                repo_path.join(&file_path)
            };

            match navigator.goto_references(&abs_path, r, c, include_definition) {
                Ok(r) => r,
                Err(e) => {
                    println!("Error: {}", e);
                    std::process::exit(1);
                }
            }
        }
        // Symbol-based lookup
        (_, _, _, Some(sym)) => navigator.goto_references_by_name(&sym, None, include_definition),
        _ => {
            println!("Error: Must provide either --file, --row, --col OR --symbol");
            std::process::exit(1);
        }
    };

    if json {
        print_json(&result);
    } else {
        println!("Symbol: {}", result.symbol);
        println!("References ({}):", result.locations.len());
        for loc in &result.locations {
            if let Some(sym) = &loc.symbol {
                println!("  {}:{} (as {})", loc.path, loc.line, sym);
            } else {
                println!("  {}:{}", loc.path, loc.line);
            }
        }
    }
}

fn cmd_stats(path: &Path, custom_cache: Option<&Path>) {
    let cache_path = effective_cache_path(path, custom_cache);
    let index = load_or_build_index(path, &cache_path);
    let (files, defs, refs) = index.stats();

    println!("Index Statistics for: {}", path.display());
    println!("  Cache location: {}", cache_path.display());
    println!("  Files indexed:  {}", files);
    println!("  Definitions:    {}", defs);
    println!("  References:     {}", refs);
    println!("  Aliases:        {}", index.alias_count());

    // Top symbols by reference count
    let ref_counts = index.top_referenced_symbols(10);

    println!("\nTop 10 most referenced symbols:");
    for (name, count) in &ref_counts {
        println!("  {:6} {}", count, name);
    }
}

fn print_json(result: &echo_agent_codebase_graph::NavigationResult) {
    use serde_json::json;

    let locations: Vec<_> = result
        .locations
        .iter()
        .map(|loc| {
            json!({
                "path": &loc.path,
                "line": loc.line,
                "symbol": loc.symbol,
            })
        })
        .collect();

    let output = json!({
        "symbol": result.symbol,
        "locations": locations,
    });

    println!("{}", serde_json::to_string_pretty(&output).unwrap());
}
