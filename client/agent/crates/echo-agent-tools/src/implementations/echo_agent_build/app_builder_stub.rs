//! Stub surface when the app-builder feature is off.

/// Placeholder config — app-builder tools are unavailable in this build.
#[derive(Debug, Clone, Default)]
pub enum AppBuilderDeployerConfig {
    #[default]
    Disabled,
}

impl AppBuilderDeployerConfig {
    pub fn is_enabled(&self) -> bool {
        false
    }
}
