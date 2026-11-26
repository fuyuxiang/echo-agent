// Exercise the actual model-facing formatter with actual production types.
mod types {
    pub mod memory_backend {
        pub use echo_agent_tools::types::memory_backend::{
            format_staleness_note, MemorySearchResult,
        };
    }
}

#[path = "../../agent/crates/echo-agent-tools/src/implementations/memory/search_output.rs"]
mod production_memory_search_output;
