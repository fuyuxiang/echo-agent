//! Per-key registry of [`CircuitBreaker`] instances (one per upstream
//! endpoint, one per tenant, etc.).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::breaker::CircuitBreaker;
use crate::config::BreakerConfig;

pub struct CircuitBreakerRegistry {
    config: BreakerConfig,
    breakers: Mutex<HashMap<String, Arc<CircuitBreaker>>>,
}

impl CircuitBreakerRegistry {
    pub fn new(config: BreakerConfig) -> Self {
        Self {
            config,
            breakers: Mutex::new(HashMap::new()),
        }
    }

    /// Returns `None` if the registry's config has `enabled = false`;
    /// otherwise returns (and lazily creates) the breaker for `key`.
    pub fn get(&self, key: &str) -> Option<Arc<CircuitBreaker>> {
        if !self.config.enabled {
            return None;
        }
        let mut breakers = self.breakers.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(cb) = breakers.get(key) {
            return Some(Arc::clone(cb));
        }
        let cb = Arc::new(CircuitBreaker::new(self.config.clone()));
        let ret = Arc::clone(&cb);
        breakers.insert(key.to_owned(), cb);
        Some(ret)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_returns_none_when_disabled() {
        let cfg = BreakerConfig {
            enabled: false,
            ..Default::default()
        };
        let reg = CircuitBreakerRegistry::new(cfg);
        assert!(reg.get("endpoint-a").is_none());
    }

    #[test]
    fn registry_returns_same_breaker_for_same_key() {
        let reg = CircuitBreakerRegistry::new(BreakerConfig::default());
        let a = reg.get("endpoint-a").unwrap();
        let b = reg.get("endpoint-a").unwrap();
        assert!(Arc::ptr_eq(&a, &b));
    }

    #[test]
    fn registry_returns_distinct_breakers_for_distinct_keys() {
        let reg = CircuitBreakerRegistry::new(BreakerConfig::default());
        let a = reg.get("endpoint-a").unwrap();
        let b = reg.get("endpoint-b").unwrap();
        assert!(!Arc::ptr_eq(&a, &b));
    }
}
