//! End-to-end tests for the `browser_tab` and `browser_network_details` tools
//! against a real Chrome.
//!
//! The crate-level suite in `echo-agent-browser-tools` covers the CDP layer;
//! these drive the tools themselves, so the output the model actually sees is
//! what gets asserted.
//!
//! Ignored by default — CI has no Chrome. Run them with:
//!
//! ```bash
//! cargo test -p echo-agent-tools --features browser --test browser_tab_chrome_e2e \
//!     -- --ignored --test-threads=1
//! ```
