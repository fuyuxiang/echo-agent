//! Worktree-pool configuration value type, extracted from echo-agent-runtime
//! (config dependency inversion).

use serde::{Deserialize, Serialize};

/// Configuration for the pre-created worktree pool.
///
/// The pool pre-creates linked worktrees in the background so that fork
/// flows can acquire a ready-made worktree instead of creating one from
/// scratch. Set in `config.toml` under `[worktree_pool]`.
///
/// Example:
/// ```toml
/// [worktree_pool]
/// enabled = true
/// pool_size = 2
/// file_count_threshold = 50000
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PoolConfig {
    /// Whether the pool is enabled at all.
    /// Can be set to false to disable pooling regardless of repo size.
    /// Default: true (auto-detect based on file_count_threshold)
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// Number of worktrees to keep ready in the pool.
    /// 2 is the minimum useful value when forks need parallel worktrees.
    /// Default: 2
    #[serde(default = "default_pool_size")]
    pub pool_size: usize,

    /// Minimum number of tracked files for the pool to activate.
    /// Below this threshold, on-demand creation is fast enough.
    /// Default: 50_000
    #[serde(default = "default_file_count_threshold")]
    pub file_count_threshold: usize,

    /// Number of threads to use for worktree creation when populating the pool.
    /// This can speed up pool population on large repos, but also increases resource usage.
    /// Default: 3.
    #[serde(default = "default_pool_parallelism")]
    pub parallelism: usize,
}

fn default_true() -> bool {
    true
}

fn default_pool_parallelism() -> usize {
    3
}

fn default_pool_size() -> usize {
    2
}

fn default_file_count_threshold() -> usize {
    50_000
}

impl Default for PoolConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            pool_size: default_pool_size(),
            file_count_threshold: default_file_count_threshold(),
            parallelism: default_pool_parallelism(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_matches_serde_helpers() {
        let c = PoolConfig::default();
        assert!(c.enabled);
        assert_eq!(c.pool_size, 2);
        assert_eq!(c.file_count_threshold, 50_000);
        assert_eq!(c.parallelism, 3);
    }

    #[test]
    fn empty_table_applies_all_field_defaults() {
        let c: PoolConfig = serde_json::from_str("{}").unwrap();
        assert!(c.enabled);
        assert_eq!(c.pool_size, 2);
        assert_eq!(c.file_count_threshold, 50_000);
        assert_eq!(c.parallelism, 3);
    }

    #[test]
    fn partial_override_keeps_other_field_defaults() {
        let c: PoolConfig = serde_json::from_str(r#"{"enabled": false, "pool_size": 5}"#).unwrap();
        assert!(!c.enabled);
        assert_eq!(c.pool_size, 5);
        assert_eq!(c.file_count_threshold, 50_000);
        assert_eq!(c.parallelism, 3);
    }
}
