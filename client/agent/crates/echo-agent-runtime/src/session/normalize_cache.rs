//! Process-wide cache for normalized images: `moka::future::Cache`
//! collapses concurrent identical-content compute via `try_get_with`,
//! byte-weighted eviction caps memory at [`CACHE_MAX_BYTES`], and
//! TTL / TTI keep long-running daemons hygienic.

use std::borrow::Cow;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock};
use std::time::Duration;

use bytes::Bytes;
use moka::future::Cache;

use crate::session::image_describe::content_fingerprint_bytes;
use crate::session::image_normalize::ImageCompressionInfo;

const CACHE_MAX_BYTES: u64 = 64 * 1024 * 1024;
const CACHE_TTL: Duration = Duration::from_secs(60 * 60);
const CACHE_TTI: Duration = Duration::from_secs(15 * 60);

/// Cacheable normalize outcome. [`bytes::Bytes`] keeps cache-hit
/// clones O(1) (refcount bump, no memcpy).
#[derive(Debug, Clone)]
pub(crate) enum NormalizedEntry {
    Unchanged {
        bytes: Bytes,
        mime: Cow<'static, str>,
    },
    Compressed {
        bytes: Bytes,
        mime: Cow<'static, str>,
        info: ImageCompressionInfo,
    },
    /// Re-encode could not meet the byte cap; original bytes kept.
    /// Caller emits an `image_re_encode_fallback` notice.
    ReEncodingOversized {
        bytes: Bytes,
        mime: Cow<'static, str>,
    },
}

/// Error from the normalize compute step. `try_get_with` never
/// inserts on `Err`, so transient failures (slow disks, join errors)
/// do not pin a sticky failure slot in front of a future success.
#[derive(Debug, Clone, thiserror::Error)]
#[error("{0}")]
pub(crate) struct NormalizeError(pub String);

/// Cache-key namespace per harness profile. Enum (not `bool`) so
/// future variants land in a fresh slot rather than colliding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum HarnessVariant {
    Default,
    Cursor,
}

impl HarnessVariant {
    pub(crate) fn from_is_cursor(is_cursor: bool) -> Self {
        if is_cursor {
            Self::Cursor
        } else {
            Self::Default
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct CacheKey {
    content: [u8; 32],
    harness: HarnessVariant,
}

fn cache_key(raw_bytes: &[u8], harness: HarnessVariant) -> CacheKey {
    CacheKey {
        content: content_fingerprint_bytes(raw_bytes),
        harness,
    }
}

pub(crate) struct NormalizeCache {
    inner: Cache<CacheKey, NormalizedEntry>,
    enabled: AtomicBool,
}

impl NormalizeCache {
    pub(crate) fn global() -> &'static Self {
        static INSTANCE: LazyLock<NormalizeCache> =
            LazyLock::new(|| NormalizeCache::with_capacity(CACHE_MAX_BYTES));
        &INSTANCE
    }

    pub(crate) fn with_capacity(max_bytes: u64) -> Self {
        let inner = Cache::builder()
            .max_capacity(max_bytes)
            .weigher(weigh_entry)
            .time_to_live(CACHE_TTL)
            .time_to_idle(CACHE_TTI)
            .build();
        Self {
            inner,
            enabled: AtomicBool::new(false),
        }
    }

    /// Default false; toggled from remote settings at startup.
    pub(crate) fn set_enabled(&self, enabled: bool) {
        self.enabled.store(enabled, Ordering::Relaxed);
    }

    fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed)
    }

    /// Compute-once via moka's `try_get_with`: concurrent callers
    /// share one computation; `Err` leaves the slot empty for retry.
    /// When the cache is disabled (the default), bypasses moka entirely
    /// and calls `compute` directly.
    pub(crate) async fn get_or_try_insert_with<F, Fut>(
        &self,
        raw_bytes: Vec<u8>,
        harness: HarnessVariant,
        compute: F,
    ) -> Result<NormalizedEntry, Arc<NormalizeError>>
    where
        F: FnOnce(Vec<u8>) -> Fut,
        Fut: std::future::Future<Output = Result<NormalizedEntry, NormalizeError>>,
    {
        if !self.is_enabled() {
            return compute(raw_bytes).await.map_err(Arc::new);
        }
        let key = cache_key(&raw_bytes, harness);
        self.inner.try_get_with(key, compute(raw_bytes)).await
    }

    #[cfg(test)]
    pub(crate) async fn get_for_tests(
        &self,
        raw_bytes: &[u8],
        harness: HarnessVariant,
    ) -> Option<NormalizedEntry> {
        self.inner.get(&cache_key(raw_bytes, harness)).await
    }
}

fn weigh_entry(_k: &CacheKey, v: &NormalizedEntry) -> u32 {
    let (bytes_len, mime_len) = match v {
        NormalizedEntry::Unchanged { bytes, mime }
        | NormalizedEntry::Compressed { bytes, mime, .. }
        | NormalizedEntry::ReEncodingOversized { bytes, mime } => (bytes.len(), mime.len()),
    };
    bytes_len
        .saturating_add(mime_len)
        .try_into()
        .unwrap_or(u32::MAX)
}

/// `spawn_blocking` adapter mapping `JoinError` → [`NormalizeError`].
pub(crate) async fn run_blocking<F, T>(work: F) -> Result<T, NormalizeError>
where
    F: FnOnce() -> Result<T, NormalizeError> + Send + 'static,
    T: Send + 'static,
{
    match tokio::task::spawn_blocking(work).await {
        Ok(r) => r,
        Err(e) => Err(NormalizeError(format!("join error: {e}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn enabled_cache(max_bytes: u64) -> NormalizeCache {
        let cache = NormalizeCache::with_capacity(max_bytes);
        cache.set_enabled(true);
        cache
    }

    fn fake_unchanged(bytes: &'static [u8]) -> NormalizedEntry {
        NormalizedEntry::Unchanged {
            bytes: Bytes::from_static(bytes),
            mime: Cow::Borrowed("image/png"),
        }
    }

    fn payload_bytes(entry: &NormalizedEntry) -> &Bytes {
        match entry {
            NormalizedEntry::Unchanged { bytes, .. }
            | NormalizedEntry::Compressed { bytes, .. }
            | NormalizedEntry::ReEncodingOversized { bytes, .. } => bytes,
        }
    }

    #[tokio::test]
    async fn hit_returns_same_entry() {
        let cache = enabled_cache(CACHE_MAX_BYTES);
        let counter = Arc::new(AtomicUsize::new(0));

        let raw = b"identical-content".to_vec();

        for _ in 0..2 {
            let c = counter.clone();
            let entry = cache
                .get_or_try_insert_with(raw.clone(), HarnessVariant::Default, move |_| async move {
                    c.fetch_add(1, Ordering::SeqCst);
                    Ok(fake_unchanged(b"out"))
                })
                .await
                .expect("compute ok");
            assert_eq!(payload_bytes(&entry), &Bytes::from_static(b"out"));
        }

        assert_eq!(counter.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn miss_on_different_content() {
        let cache = enabled_cache(CACHE_MAX_BYTES);
        let counter = Arc::new(AtomicUsize::new(0));

        for raw in [b"alpha".to_vec(), b"beta".to_vec()] {
            let c = counter.clone();
            cache
                .get_or_try_insert_with(raw, HarnessVariant::Default, move |_| async move {
                    c.fetch_add(1, Ordering::SeqCst);
                    Ok(fake_unchanged(b"out"))
                })
                .await
                .expect("compute ok");
        }

        assert_eq!(counter.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn miss_on_different_harness_flag() {
        let cache = enabled_cache(CACHE_MAX_BYTES);
        let counter = Arc::new(AtomicUsize::new(0));
        let raw = b"same".to_vec();

        for harness in [HarnessVariant::Default, HarnessVariant::Cursor] {
            let c = counter.clone();
            cache
                .get_or_try_insert_with(raw.clone(), harness, move |_| async move {
                    c.fetch_add(1, Ordering::SeqCst);
                    Ok(fake_unchanged(b"out"))
                })
                .await
                .expect("compute ok");
        }

        assert_eq!(counter.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn concurrent_callers_dedup() {
        let cache = Arc::new(enabled_cache(CACHE_MAX_BYTES));
        let counter = Arc::new(AtomicUsize::new(0));
        let raw = b"shared-content".to_vec();

        let mut handles = Vec::new();
        for _ in 0..5 {
            let cache = cache.clone();
            let counter = counter.clone();
            let raw = raw.clone();
            handles.push(tokio::spawn(async move {
                cache
                    .get_or_try_insert_with(raw, HarnessVariant::Default, move |_| async move {
                        counter.fetch_add(1, Ordering::SeqCst);
                        tokio::time::sleep(Duration::from_millis(50)).await;
                        Ok(fake_unchanged(b"out"))
                    })
                    .await
                    .expect("compute ok")
            }));
        }
        for h in handles {
            h.await.expect("join ok");
        }

        assert_eq!(counter.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn error_does_not_poison() {
        let cache = enabled_cache(CACHE_MAX_BYTES);
        let raw = b"poisonable".to_vec();
        let err_counter = Arc::new(AtomicUsize::new(0));
        let ok_counter = Arc::new(AtomicUsize::new(0));

        let ec = err_counter.clone();
        let err = cache
            .get_or_try_insert_with(raw.clone(), HarnessVariant::Default, move |_| async move {
                ec.fetch_add(1, Ordering::SeqCst);
                Err(NormalizeError("boom".into()))
            })
            .await
            .expect_err("first call errors");
        assert_eq!(err_counter.load(Ordering::SeqCst), 1);
        assert_eq!(err.0, "boom");

        let oc = ok_counter.clone();
        let ok = cache
            .get_or_try_insert_with(raw, HarnessVariant::Default, move |_| async move {
                oc.fetch_add(1, Ordering::SeqCst);
                Ok(fake_unchanged(b"out"))
            })
            .await
            .expect("second call succeeds");
        assert_eq!(
            ok_counter.load(Ordering::SeqCst),
            1,
            "Err entry must not poison the slot"
        );
        assert_eq!(payload_bytes(&ok), &Bytes::from_static(b"out"));
    }

    #[tokio::test]
    async fn byte_budget_evicts_lru() {
        // Capacity sized so exactly two 609-byte entries fit; a third
        // insert forces LRU eviction of `a`.
        let cache = enabled_cache(1500);
        let big_payload = Bytes::from(vec![0u8; 600]);

        for tag in [b"a".as_slice(), b"b".as_slice(), b"c".as_slice()] {
            let payload = big_payload.clone();
            cache
                .get_or_try_insert_with(
                    tag.to_vec(),
                    HarnessVariant::Default,
                    move |_| async move {
                        Ok(NormalizedEntry::Unchanged {
                            bytes: payload,
                            mime: Cow::Borrowed("image/png"),
                        })
                    },
                )
                .await
                .expect("compute ok");
            cache.inner.run_pending_tasks().await;
        }

        let a_counter = AtomicUsize::new(0);
        cache
            .get_or_try_insert_with(b"a".to_vec(), HarnessVariant::Default, |_| async {
                a_counter.fetch_add(1, Ordering::SeqCst);
                Ok(fake_unchanged(b"refilled"))
            })
            .await
            .expect("recompute ok");
        assert_eq!(
            a_counter.load(Ordering::SeqCst),
            1,
            "earliest entry must have been evicted under the byte cap"
        );

        let c_counter = AtomicUsize::new(0);
        cache
            .get_or_try_insert_with(b"c".to_vec(), HarnessVariant::Default, |_| async {
                c_counter.fetch_add(1, Ordering::SeqCst);
                Ok(fake_unchanged(b"recomputed-c"))
            })
            .await
            .expect("c lookup ok");
        assert_eq!(
            c_counter.load(Ordering::SeqCst),
            0,
            "most-recent entry `c` must still be present"
        );
    }

    #[tokio::test]
    async fn cache_hit_shares_underlying_buffer() {
        let cache = enabled_cache(CACHE_MAX_BYTES);
        let raw = b"ptr-identity".to_vec();
        let payload = Bytes::from(vec![1u8, 2, 3, 4, 5]);
        let payload_for_closure = payload.clone();
        let expected_ptr = payload.as_ptr();

        let first = cache
            .get_or_try_insert_with(raw.clone(), HarnessVariant::Default, move |_| async move {
                Ok(NormalizedEntry::Unchanged {
                    bytes: payload_for_closure,
                    mime: Cow::Borrowed("image/png"),
                })
            })
            .await
            .expect("compute ok");
        assert_eq!(payload_bytes(&first).as_ptr(), expected_ptr);

        let second = cache
            .get_or_try_insert_with(raw, HarnessVariant::Default, |_| async {
                panic!("must not recompute on hit")
            })
            .await
            .expect("hit ok");
        assert_eq!(
            payload_bytes(&second).as_ptr(),
            expected_ptr,
            "cache hit must hand back the same backing buffer"
        );
    }

    #[tokio::test]
    async fn run_blocking_maps_join_error() {
        let err = run_blocking::<_, ()>(|| panic!("boom in worker"))
            .await
            .expect_err("panicking worker must surface as join error");
        assert!(
            err.0.starts_with("join error:"),
            "expected join-error prefix, got: {}",
            err.0
        );
    }

    #[test]
    fn blake3_key_derivation_stable() {
        let raw = b"deterministic";
        let k1 = cache_key(raw, HarnessVariant::Default);
        let k2 = cache_key(raw, HarnessVariant::Default);
        assert_eq!(k1.content, k2.content);
        assert_eq!(k1, k2);
    }

    #[test]
    fn blake3_key_per_harness() {
        let raw = b"deterministic";
        assert_ne!(
            cache_key(raw, HarnessVariant::Default),
            cache_key(raw, HarnessVariant::Cursor),
        );
        assert_eq!(
            cache_key(raw, HarnessVariant::Cursor),
            cache_key(raw, HarnessVariant::Cursor),
        );
    }

    #[tokio::test]
    async fn disabled_cache_does_not_dedup() {
        let cache = NormalizeCache::with_capacity(CACHE_MAX_BYTES);
        let counter = Arc::new(AtomicUsize::new(0));
        let raw = b"identical-content".to_vec();

        for _ in 0..2 {
            let c = counter.clone();
            cache
                .get_or_try_insert_with(raw.clone(), HarnessVariant::Default, move |_| async move {
                    c.fetch_add(1, Ordering::SeqCst);
                    Ok(fake_unchanged(b"out"))
                })
                .await
                .expect("compute ok");
        }

        assert_eq!(counter.load(Ordering::SeqCst), 2);
        cache.inner.run_pending_tasks().await;
        assert_eq!(cache.inner.entry_count(), 0);
    }

    #[tokio::test]
    async fn disabled_cache_returns_compute_value() {
        let cache = NormalizeCache::with_capacity(CACHE_MAX_BYTES);
        let raw = b"compute-passthrough".to_vec();
        let payload = Bytes::from_static(b"passthrough-bytes");
        let payload_for_closure = payload.clone();

        let entry = cache
            .get_or_try_insert_with(raw, HarnessVariant::Default, move |_| async move {
                Ok(NormalizedEntry::Unchanged {
                    bytes: payload_for_closure,
                    mime: Cow::Borrowed("image/png"),
                })
            })
            .await
            .expect("compute ok");

        match entry {
            NormalizedEntry::Unchanged { bytes, mime } => {
                assert_eq!(bytes, payload);
                assert_eq!(mime, Cow::Borrowed("image/png"));
            }
            other => panic!("expected Unchanged, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn enabled_toggle_takes_effect() {
        let cache = NormalizeCache::with_capacity(CACHE_MAX_BYTES);
        let counter = Arc::new(AtomicUsize::new(0));
        let raw = b"toggle-content".to_vec();

        cache.set_enabled(true);
        for _ in 0..2 {
            let c = counter.clone();
            cache
                .get_or_try_insert_with(raw.clone(), HarnessVariant::Default, move |_| async move {
                    c.fetch_add(1, Ordering::SeqCst);
                    Ok(fake_unchanged(b"out"))
                })
                .await
                .expect("compute ok");
        }
        assert_eq!(counter.load(Ordering::SeqCst), 1);

        cache.set_enabled(false);
        for _ in 0..2 {
            let c = counter.clone();
            cache
                .get_or_try_insert_with(raw.clone(), HarnessVariant::Default, move |_| async move {
                    c.fetch_add(1, Ordering::SeqCst);
                    Ok(fake_unchanged(b"out"))
                })
                .await
                .expect("compute ok");
        }
        assert_eq!(counter.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn disabled_cache_propagates_err() {
        let cache = NormalizeCache::with_capacity(CACHE_MAX_BYTES);
        let raw = b"err-passthrough".to_vec();

        let err = cache
            .get_or_try_insert_with(raw, HarnessVariant::Default, move |_| async move {
                Err(NormalizeError("boom".into()))
            })
            .await
            .expect_err("compute err");
        assert_eq!(err.0, "boom");

        cache.inner.run_pending_tasks().await;
        assert_eq!(cache.inner.entry_count(), 0);
    }
}
