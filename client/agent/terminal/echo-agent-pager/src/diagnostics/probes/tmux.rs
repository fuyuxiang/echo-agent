use crate::terminal::tmux_probe;

pub type TmuxProbeResult<T> = tmux_probe::TmuxQueryResult<T>;

pub trait TmuxOptionQuery {
    fn show_option(&self, option: &str) -> TmuxProbeResult<String>;

    fn option_support(&self, option: &str) -> TmuxProbeResult<()>;

    fn control_mode(&self) -> TmuxProbeResult<bool>;

    /// The attached client's resolved terminal features, which decide whether
    /// tmux forwards 24-bit color or reduces it to the client terminfo palette.
    fn client_features(&self) -> TmuxProbeResult<String>;
}

pub struct LiveTmuxProbe;

impl TmuxOptionQuery for LiveTmuxProbe {
    fn show_option(&self, option: &str) -> TmuxProbeResult<String> {
        tmux_probe::query_option(option)
    }

    fn option_support(&self, option: &str) -> TmuxProbeResult<()> {
        tmux_probe::query_option_support(option)
    }

    fn control_mode(&self) -> TmuxProbeResult<bool> {
        tmux_probe::query_control_mode()
    }

    fn client_features(&self) -> TmuxProbeResult<String> {
        tmux_probe::query_client_features()
    }
}
