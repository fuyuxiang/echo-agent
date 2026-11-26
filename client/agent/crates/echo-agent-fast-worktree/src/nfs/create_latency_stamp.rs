//! Product-create stamp lines (`WorktreeBuilder::create`, including crawlers).
//!
//! Distinct from the grove library key `GROVE_BASELINE_NFS_WT_CREATE_MS`
//! (prepare + read-only `finish_mount`). A non-NFS strategy must not emit an
//! NFS-named key.

/// Grove library gate env. This module must never print it.
pub const LIBRARY_CREATE_ENV: &str = "GROVE_BASELINE_NFS_WT_CREATE_MS";
pub const PRODUCT_CREATE_KEY: &str = "NFS_WT_CREATE_PRODUCT_MS";
pub const PRODUCT_CREATE_ENV: &str = "GROVE_BASELINE_NFS_WT_CREATE_PRODUCT_MS";

/// Format a p50 (median) create latency line for human logs (not a mean).
#[must_use]
pub fn format_create_p50(strategy: &str, p50_ms: f64, n: usize, iters: usize) -> String {
    if strategy != "nfs" {
        let key = if strategy == "copy" {
            "COPY_WT_CREATE_MS"
        } else {
            "MIXED_WT_CREATE_MS"
        };
        return format!(
            "{key} p50={p50_ms:.3} (n={n}, iters={iters}, strategy={strategy}; not an NFS key)"
        );
    }
    format!("{PRODUCT_CREATE_KEY} p50={p50_ms:.3} (n={n}, iters={iters})")
}

#[must_use]
#[deprecated(note = "renamed to format_create_p50; this formats median, not mean")]
pub fn format_create_mean(strategy: &str, p50_ms: f64, n: usize, iters: usize) -> String {
    format_create_p50(strategy, p50_ms, n, iters)
}

pub fn format_create_stamp(
    strategy: &str,
    p50_ms: f64,
    n: usize,
    release: bool,
    host: &str,
) -> Result<String, String> {
    if strategy != "nfs" {
        return Err(format!(
            "refusing to emit an NFS-named stamp for strategy={strategy}"
        ));
    }
    if !release {
        return Err("--stamp requires cargo run --release".into());
    }
    if host.is_empty() {
        return Err("stamp host must be non-empty".into());
    }
    Ok(format!(
        "{PRODUCT_CREATE_ENV}={p50_ms:.3} n={n} release=yes host={host} stat=p50"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copy_strategy_cannot_emit_nfs_named_key() {
        let line = format_create_p50("copy", 29.014, 8, 1);
        assert!(
            !line.contains("NFS_"),
            "copy mean must not wear an NFS name: {line}"
        );
        assert!(line.contains("COPY_WT_CREATE_MS"), "{line}");
        let err = format_create_stamp("copy", 29.014, 8, true, "26.5.2-aarch64")
            .expect_err("copy --stamp must fail");
        assert!(err.contains("strategy=copy"), "{err}");
        assert!(!err.contains(PRODUCT_CREATE_ENV), "{err}");
        assert!(!err.contains(LIBRARY_CREATE_ENV), "{err}");
    }

    #[test]
    fn nfs_stamp_is_product_key_with_n_and_release() {
        let line = format_create_stamp("nfs", 11790.0, 32, true, "26.5.2-aarch64").expect("nfs");
        assert!(line.starts_with(PRODUCT_CREATE_ENV), "{line}");
        assert!(line.contains("n=32"), "{line}");
        assert!(line.contains("release=yes"), "{line}");
        assert!(line.contains("host=26.5.2-aarch64"), "{line}");
        assert!(
            !line.contains(&format!("{LIBRARY_CREATE_ENV}=")),
            "product stamp must not emit the library env: {line}"
        );
        assert_ne!(LIBRARY_CREATE_ENV, PRODUCT_CREATE_ENV);
        format_create_stamp("nfs", 12.0, 32, false, "host").expect_err("debug stamp refused");
    }
}
