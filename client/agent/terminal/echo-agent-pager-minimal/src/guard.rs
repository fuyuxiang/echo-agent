//! Compile-time guard for minimal mode's resize strategy (design K6 / risk #2).
//!
//! The terminal owns committed history, so minimal must use only the built-in
//! `autoresize` / `set_viewport_height` and must NEVER call the inline crate's
//! RIS-rerender helpers or `emit_to_scrollback` — those re-emit history the
//! terminal already has, double-printing (or, with ED3, wiping) committed
//! scrollback. This test fails loudly if such a call ever sneaks into the
//! minimal module.

/// The forbidden inline-crate helpers. Scanned against the minimal sources via
/// `include_str!` (this guard file is intentionally not scanned, since it names
/// the identifiers here).
#[test]
fn minimal_never_uses_ris_rerender_or_emit_to_scrollback() {
    const FORBIDDEN: &[&str] = &[
        "resize_purge_rerender",
        "emit_to_scrollback",
        "resize_viewport_height",
    ];
    // EVERY module of this crate except this guard file (which names the
    // forbidden identifiers). Keep in sync with `lib.rs`'s module list — a
    // module missing here is a hole in the K6 guard.
    let sources = [
        ("lib.rs", include_str!("lib.rs")),
        ("auth.rs", include_str!("auth.rs")),
        ("commit.rs", include_str!("commit.rs")),
        ("full_view.rs", include_str!("full_view.rs")),
        ("live.rs", include_str!("live.rs")),
        ("overlay.rs", include_str!("overlay.rs")),
        ("panel.rs", include_str!("panel.rs")),
        ("plan.rs", include_str!("plan.rs")),
        ("todo.rs", include_str!("todo.rs")),
        ("welcome.rs", include_str!("welcome.rs")),
    ];
    for (name, src) in sources {
        for needle in FORBIDDEN {
            assert!(
                !src.contains(needle),
                "minimal/{name} references forbidden resize helper `{needle}` — it would \
                 double-print committed scrollback (design K6 / risk #2); use the built-in \
                 autoresize / set_viewport_height instead"
            );
        }
    }
}
