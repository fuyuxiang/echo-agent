//! Process-cached reqwest clients for tool backends. The key must cover
//! every input that shapes the client: headers via [`headers_fingerprint`],
//! constant timeouts via the kind prefix. Cached transports outlive
//! per-session runtimes; pooled connections are ready-checked on reuse.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex};

/// LRU cap; evicted clients keep working for their holders.
const MAX_ENTRIES: usize = 32;

#[derive(Default)]
struct Entry {
    slot: Arc<Mutex<Option<reqwest::Client>>>,
    last_used: u64,
}

/// Opaque cache key. [`cache_key`] is the only constructor, so the header
/// fingerprint can never be skipped and a raw string can never stand in.
#[derive(Clone, PartialEq, Eq, Hash)]
pub(crate) struct CacheKey(String);

/// Cached client for `key`; misses single-flight on the slot lock, which is
/// held across the synchronous `build` (keep builds fast). Errors are not cached.
pub(crate) fn cached_client<E>(
    key: CacheKey,
    build: impl FnOnce() -> Result<reqwest::Client, E>,
) -> Result<reqwest::Client, E> {
    static CACHE: LazyLock<Mutex<(u64, HashMap<CacheKey, Entry>)>> =
        LazyLock::new(Default::default);
    let slot = {
        let mut guard = CACHE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let (tick, map) = &mut *guard;
        *tick += 1;
        if !map.contains_key(&key)
            && map.len() >= MAX_ENTRIES
            && let Some(lru) = map
                .iter()
                .min_by_key(|(_, e)| e.last_used)
                .map(|(k, _)| k.clone())
        {
            map.remove(&lru);
        }
        let entry = map.entry(key).or_default();
        entry.last_used = *tick;
        entry.slot.clone()
    };
    // A builder panic must not brick the key; the slot is simply still empty.
    let mut slot = slot
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(client) = &*slot {
        return Ok(client.clone());
    }
    let built = build()?;
    *slot = Some(built.clone());
    Ok(built)
}

/// Sole [`CacheKey`] constructor. Because [`cached_client`] takes `CacheKey`
/// rather than `&str`, the header fingerprint is impossible to skip: the type
/// is the guarantee.
pub(crate) fn cache_key(kind: &str, headers: &reqwest::header::HeaderMap) -> CacheKey {
    CacheKey(format!("{kind}|{}", headers_fingerprint(headers)))
}

/// Hash of sorted, length-prefixed (name, value-bytes) pairs: collision-resistant
/// and keeps raw credentials out of the process-lifetime key map.
fn headers_fingerprint(headers: &reqwest::header::HeaderMap) -> String {
    use std::hash::{Hash, Hasher};
    let mut pairs: Vec<(&str, &[u8])> = headers
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_bytes()))
        .collect();
    pairs.sort();
    let mut hasher = std::hash::DefaultHasher::new();
    pairs.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::header::{HeaderMap, HeaderValue};

    // Cache is process-global; serialize so fills/evictions cannot cross tests.
    static TEST_LOCK: Mutex<()> = Mutex::new(());
    fn lock() -> std::sync::MutexGuard<'static, ()> {
        TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    #[test]
    fn any_changed_header_misses_the_cache() {
        let _g = lock();
        let mut h1 = HeaderMap::new();
        h1.insert("authorization", HeaderValue::from_static("Bearer old"));
        h1.insert("x-extra", HeaderValue::from_static("v"));
        let mut rotated = h1.clone();
        rotated.insert("authorization", HeaderValue::from_static("Bearer new"));
        let mut extra = h1.clone();
        extra.insert("x-extra", HeaderValue::from_static("v2"));

        let _ = cached_client::<()>(cache_key("rot", &h1), || Ok(reqwest::Client::new()));
        for headers in [&rotated, &extra] {
            let mut built = false;
            let _ = cached_client::<()>(cache_key("rot", headers), || {
                built = true;
                Ok(reqwest::Client::new())
            });
            assert!(built, "changed header must miss the cache");
        }
    }

    #[test]
    fn build_error_is_propagated_and_not_cached() {
        let _g = lock();
        let key = cache_key("k-err", &HeaderMap::new());
        let err = cached_client::<&str>(key.clone(), || Err("boom"));
        assert_eq!(err.unwrap_err(), "boom");
        let mut built = false;
        let ok = cached_client::<&str>(key, || {
            built = true;
            Ok(reqwest::Client::new())
        });
        assert!(ok.is_ok() && built, "error must not poison the key");
    }

    #[test]
    fn concurrent_misses_coalesce_on_one_build() {
        let _g = lock();
        use std::sync::Barrier;
        use std::sync::atomic::{AtomicUsize, Ordering};
        let builds = Arc::new(AtomicUsize::new(0));
        // Barrier forces overlap; single-flight must admit exactly one builder.
        let in_build = Arc::new(Barrier::new(2));
        let spawn = |builds: Arc<AtomicUsize>, gate: Arc<Barrier>| {
            std::thread::spawn(move || {
                cached_client::<()>(cache_key("k-flight", &HeaderMap::new()), || {
                    builds.fetch_add(1, Ordering::SeqCst);
                    gate.wait();
                    Ok(reqwest::Client::new())
                })
                .unwrap();
            })
        };
        let t1 = spawn(builds.clone(), in_build.clone());
        let t2 = spawn(builds.clone(), in_build.clone());
        in_build.wait();
        t1.join().unwrap();
        t2.join().unwrap();
        assert_eq!(
            builds.load(Ordering::SeqCst),
            1,
            "siblings must share one build"
        );
    }

    #[test]
    fn cap_evicts_least_recently_used() {
        let _g = lock();
        for i in 0..MAX_ENTRIES {
            let _ = cached_client::<()>(cache_key(&format!("lru-{i}"), &HeaderMap::new()), || {
                Ok(reqwest::Client::new())
            });
        }
        let _ = cached_client::<()>(cache_key("lru-0", &HeaderMap::new()), || panic!("must hit"));
        let _ = cached_client::<()>(cache_key("lru-overflow", &HeaderMap::new()), || {
            Ok(reqwest::Client::new())
        });
        let mut rebuilt_0 = false;
        let _ = cached_client::<()>(cache_key("lru-0", &HeaderMap::new()), || {
            rebuilt_0 = true;
            Ok(reqwest::Client::new())
        });
        assert!(!rebuilt_0, "recently-used entry must survive the cap");
    }
}
