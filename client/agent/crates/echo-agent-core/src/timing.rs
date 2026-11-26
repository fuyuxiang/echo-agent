const TARGET: &str = "echo_agent_instrumentation";

pub struct TimingGuard {
    name: &'static str,
    start: std::time::Instant,
}

impl TimingGuard {
    pub fn new(name: &'static str) -> Self {
        Self {
            name,
            start: std::time::Instant::now(),
        }
    }
}

impl Drop for TimingGuard {
    fn drop(&mut self) {
        let elapsed_us = self.start.elapsed().as_micros() as u64;
        tracing::info!(
            target: TARGET,
            event = "timing",
            name = self.name,
            elapsed_us,
        );
    }
}

pub fn timer(name: &'static str) -> TimingGuard {
    TimingGuard::new(name)
}
