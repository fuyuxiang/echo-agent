//! File watcher for appearance configuration.
//!
//! In dev mode, watches ~/.echo-agent/pager.toml for changes and hot-reloads.
//! In prod mode, returns static defaults (no file operations).
use super::config::AppearanceConfig;
use std::io;
use std::path::PathBuf;
use tokio::sync::watch;
/// Watches for appearance config changes.
///
/// In dev mode: reads from ~/.echo-agent/pager.toml, watches for changes.
/// In prod mode: returns static defaults, `.changed()` never fires.
pub struct ConfigWatcher {
    rx: watch::Receiver<AppearanceConfig>,
    #[allow(dead_code)]
    state: WatcherState,
}
enum WatcherState {
    /// No background task (prod mode or dev without notify)
    Static {
        /// Keep sender alive so channel doesn't close
        _tx: watch::Sender<AppearanceConfig>,
    },
}
impl ConfigWatcher {
    /// Start the config watcher.
    ///
    /// - In dev mode: reads/creates ~/.echo-agent/pager.toml, watches for changes
    /// - In prod mode: returns default config, no file operations
    pub async fn start() -> io::Result<Self> {
        Self::start_static()
    }
    /// Get current config.
    pub fn current(&self) -> watch::Ref<'_, AppearanceConfig> {
        self.rx.borrow()
    }
    /// Wait for config to change. Never completes in prod mode.
    pub async fn changed(&mut self) -> Result<(), watch::error::RecvError> {
        self.rx.changed().await
    }
    /// Path to `$ECHO_AGENT_HOME/pager.toml`.
    fn pager_config_path() -> PathBuf {
        crate::util::pager_toml_path()
    }
    /// Start with config loaded from disk (prod mode — no hot-reload).
    fn start_static() -> io::Result<Self> {
        let config = echo_agent_config::user_echo_agent_home()
            .and_then(|_| std::fs::read_to_string(Self::pager_config_path()).ok())
            .and_then(|content| {
                toml::from_str::<super::config::RawAppearanceConfig>(&content)
                    .ok()
                    .map(AppearanceConfig::from)
            })
            .unwrap_or_default();
        let (tx, rx) = watch::channel(config);
        Ok(Self {
            rx,
            state: WatcherState::Static { _tx: tx },
        })
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn test_watcher_start() {
        let watcher = ConfigWatcher::start().await.unwrap();
        let config = watcher.current();
        let _ = config.scrollback.blocks.edit.indent;
        let _ = config.scrollback.blocks.edit.vpad;
    }
}
