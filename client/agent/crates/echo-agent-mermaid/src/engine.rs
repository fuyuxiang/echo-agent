//! The [`MermaidEngine`] trait, error type, resource limits, and the
//! panic-isolating [`render_checked`] entry point.

use std::panic::AssertUnwindSafe;

use crate::{RenderParams, RenderedDiagram};

/// Why a diagram failed to render.
///
/// Every variant maps to the same user-facing outcome (fall back to the source
/// code block); they differ only for observability. [`Panic`](Self::Panic)
/// carries the message of an engine panic isolated by [`render_checked`] (see
/// its docs for the `panic = "unwind"` requirement).
#[derive(thiserror::Error, Debug)]
pub enum MermaidError {
    /// The source could not be parsed into a diagram.
    #[error("mermaid parse error: {0}")]
    Parse(String),
    /// The diagram parsed but layout failed.
    #[error("mermaid layout error: {0}")]
    Layout(String),
    /// The SVG could not be rasterized to PNG.
    #[error("mermaid rasterize error: {0}")]
    Rasterize(String),
    /// An external engine exceeded its wall-clock budget.
    #[error("mermaid render timed out")]
    Timeout,
    /// The engine cannot render this input (unknown/exotic diagram, disabled
    /// engine, or a breached resource limit such as oversized source).
    #[error("mermaid render unsupported: {0}")]
    Unsupported(String),
    /// The engine panicked and [`render_checked`] caught it and converted it to
    /// this error. **Only intercepted when the binary is built with
    /// `panic = "unwind"`**; under `panic = "abort"` the process aborts instead
    /// (see [`render_checked`]). Carries the panic message.
    #[error("mermaid engine panicked: {0}")]
    Panic(String),
}

/// Caps applied by [`render_checked`] before the engine so untrusted source
/// can't trivially exhaust memory via an oversized payload.
///
/// This enforces `max_source_bytes`. A wall-clock timeout for a runaway render
/// is enforced *out of process* by the caller (the pager renders each diagram in
/// a short-lived child via [`crate::run_with_timeout`], which a synchronous
/// in-process call could not self-impose). The output pixmap area/height are
/// separately capped inside [`crate::rasterize`] ([`crate::MAX_OUTPUT_MEGAPIXELS`]
/// + [`RenderParams::max_height_px`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RenderLimits {
    /// Maximum accepted source length in bytes. Larger input is rejected with
    /// [`MermaidError::Unsupported`] *before* the engine runs.
    pub max_source_bytes: usize,
}

impl Default for RenderLimits {
    fn default() -> Self {
        // 64 KiB — comfortably larger than any hand-authored diagram.
        Self {
            max_source_bytes: 64 * 1024,
        }
    }
}

/// A pluggable Mermaid rendering backend.
///
/// Implementations turn Mermaid source into a rasterized [`RenderedDiagram`].
/// Prefer calling [`render_checked`] over this method directly: it applies
/// [`RenderLimits`] and isolates panics. Implementations must be cheap to share
/// (`Send + Sync`) so a worker pool can hold one behind an `Arc`.
pub trait MermaidEngine: Send + Sync {
    /// Render `source` to a PNG using `params`.
    ///
    /// Implementations may panic on pathological input; callers are expected to
    /// wrap this via [`render_checked`].
    fn render(&self, source: &str, params: &RenderParams) -> Result<RenderedDiagram, MermaidError>;
}

/// Render `source` with `engine`, enforcing `limits` and isolating panics.
///
/// This is the entry point a caller (e.g. a render worker) should use over
/// [`MermaidEngine::render`]:
///
/// - Source larger than [`RenderLimits::max_source_bytes`] is rejected with
///   [`MermaidError::Unsupported`] **without invoking the engine**.
/// - An engine panic is caught and returned as [`MermaidError::Panic`].
///
/// # Panic isolation is conditional on the unwind strategy
///
/// `catch_unwind` only intercepts panics under `panic = "unwind"`. The shipped
/// Release CLI profiles build with `panic = "abort"`,
/// under which a panicking engine aborts the **whole process** and this guard is
/// a no-op. True crash-isolation over untrusted source therefore comes from
/// running the engine *out of process*: the pager spawns a short-lived child per
/// diagram (see [`crate::run_with_timeout`] and the pager's `mermaid_worker`), so
/// a child abort is contained and the timeout is a real process kill. Within a
/// single process this guard still upgrades a (test/unwind-profile) panic to a
/// clean error. `catch_unwind` cannot catch aborts from stack overflow or
/// allocation failure even under unwind.
pub fn render_checked(
    engine: &dyn MermaidEngine,
    source: &str,
    params: &RenderParams,
    limits: &RenderLimits,
) -> Result<RenderedDiagram, MermaidError> {
    if source.len() > limits.max_source_bytes {
        return Err(MermaidError::Unsupported(format!(
            "source is {} bytes, over the {}-byte limit",
            source.len(),
            limits.max_source_bytes
        )));
    }

    // AssertUnwindSafe: a panic aborts this render and is converted to an error;
    // no observable state from the engine is reused afterward.
    match std::panic::catch_unwind(AssertUnwindSafe(|| engine.render(source, params))) {
        Ok(result) => result,
        Err(payload) => {
            let msg = panic_message(payload);
            // The panic message can embed untrusted source fragments, so keep its
            // content out of the default `warn` stream and behind `debug`.
            tracing::warn!(target: "mermaid", panic_len = msg.len(), "engine panicked; converted to error");
            tracing::debug!(target: "mermaid", panic = %msg, "engine panic message");
            Err(MermaidError::Panic(msg))
        }
    }
}

/// Best-effort extraction of a human-readable message from a panic payload.
fn panic_message(payload: Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "engine panicked".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Records whether `render` was invoked, to prove the early limit check
    /// short-circuits before the engine runs.
    struct SpyEngine {
        called: std::sync::atomic::AtomicBool,
        outcome: fn() -> Result<RenderedDiagram, MermaidError>,
    }

    impl MermaidEngine for SpyEngine {
        fn render(
            &self,
            _source: &str,
            _params: &RenderParams,
        ) -> Result<RenderedDiagram, MermaidError> {
            self.called.store(true, std::sync::atomic::Ordering::SeqCst);
            (self.outcome)()
        }
    }

    fn ok_diagram() -> Result<RenderedDiagram, MermaidError> {
        Ok(RenderedDiagram {
            png: vec![1, 2, 3],
            width_px: 10,
            height_px: 20,
        })
    }

    #[test]
    fn passes_through_engine_success() {
        let engine = SpyEngine {
            called: Default::default(),
            outcome: ok_diagram,
        };
        let out = render_checked(
            &engine,
            "flowchart LR; A-->B",
            &RenderParams::default(),
            &RenderLimits::default(),
        )
        .expect("should succeed");
        assert_eq!(out.width_px, 10);
        assert!(engine.called.load(std::sync::atomic::Ordering::SeqCst));
    }

    #[test]
    fn oversized_source_rejected_before_engine_runs() {
        let engine = SpyEngine {
            called: Default::default(),
            // If the engine were ever called for oversized input, this panic
            // would surface as `Panic`, not `Unsupported`, failing the assert.
            outcome: || panic!("engine must not be called for oversized source"),
        };
        let limits = RenderLimits {
            max_source_bytes: 8,
        };
        let err = render_checked(
            &engine,
            "this source is definitely longer than eight bytes",
            &RenderParams::default(),
            &limits,
        )
        .expect_err("oversized source must be rejected");
        assert!(matches!(err, MermaidError::Unsupported(_)));
        assert!(
            !engine.called.load(std::sync::atomic::Ordering::SeqCst),
            "engine must not run when the source is over the limit"
        );
    }

    #[test]
    fn source_at_limit_is_accepted() {
        let engine = SpyEngine {
            called: Default::default(),
            outcome: ok_diagram,
        };
        let src = "12345678"; // exactly 8 bytes
        let limits = RenderLimits {
            max_source_bytes: 8,
        };
        assert!(render_checked(&engine, src, &RenderParams::default(), &limits).is_ok());
    }

    #[test]
    fn panicking_engine_becomes_panic_error() {
        struct Panicky;
        impl MermaidEngine for Panicky {
            fn render(&self, _: &str, _: &RenderParams) -> Result<RenderedDiagram, MermaidError> {
                panic!("boom in layout");
            }
        }
        let err = render_checked(
            &Panicky,
            "flowchart LR; A-->B",
            &RenderParams::default(),
            &RenderLimits::default(),
        )
        .expect_err("panic must be converted to an error");
        match err {
            MermaidError::Panic(msg) => assert!(msg.contains("boom in layout")),
            other => panic!("expected Panic, got {other:?}"),
        }
    }

    #[test]
    fn engine_errors_pass_through_unchanged() {
        struct Failing(fn() -> MermaidError);
        impl MermaidEngine for Failing {
            fn render(&self, _: &str, _: &RenderParams) -> Result<RenderedDiagram, MermaidError> {
                Err((self.0)())
            }
        }
        // The wrapper must return the exact same variant *and* payload the engine
        // produced — not merely "not Panic".
        for make in [
            (|| MermaidError::Parse("p-payload".into())) as fn() -> MermaidError,
            || MermaidError::Layout("l-payload".into()),
            || MermaidError::Rasterize("r-payload".into()),
            || MermaidError::Timeout,
            || MermaidError::Unsupported("u-payload".into()),
        ] {
            let injected = make();
            let err = render_checked(
                &Failing(make),
                "x",
                &RenderParams::default(),
                &RenderLimits::default(),
            )
            .expect_err("engine error should pass through");
            assert_eq!(
                std::mem::discriminant(&err),
                std::mem::discriminant(&injected),
                "variant changed: got {err:?}, expected {injected:?}"
            );
            // Payload round-trips verbatim (Display embeds the carried string).
            assert_eq!(err.to_string(), injected.to_string());
        }
    }

    #[test]
    fn error_display_is_descriptive() {
        // Each variant's Display carries a distinguishing word, and payload
        // variants interpolate their carried message.
        assert!(MermaidError::Timeout.to_string().contains("timed out"));
        for (err, word) in [
            (MermaidError::Parse("PL".into()), "parse"),
            (MermaidError::Layout("PL".into()), "layout"),
            (MermaidError::Rasterize("PL".into()), "rasterize"),
            (MermaidError::Unsupported("PL".into()), "unsupported"),
            (MermaidError::Panic("PL".into()), "panicked"),
        ] {
            let s = err.to_string();
            assert!(s.contains(word), "{s:?} missing {word:?}");
            assert!(s.contains("PL"), "{s:?} missing interpolated payload");
        }
    }
}
