//! Tracing-based utility macros.
//!
//! This crate provides macros for:
//! - Timestamped logging (`tprintln!`, `teprintln!`)
//! - Execution timing with automatic logging (`timed!`)
//!
//! # Examples
//!
//! ```ignore
//! use echo_agent_tracing_macros::{tprintln, teprintln, timed};
//!
//! // Timestamped logging
//! tprintln!("Hello, world!");
//! teprintln!("Warning: something happened");
//!
//! // Execution timing
//! let result = timed!(log: "expensive_operation", {
//!     // ... expensive work ...
//!     42
//! });
//!
//! // Timing with Result handling
//! let result: Result<i32, String> = timed!(try: "fallible_operation", {
//!     Ok(42)
//! });
//! ```

mod timed;
mod timestamp;

// Macros are automatically exported at crate root via #[macro_export]
