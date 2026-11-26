use log::error;
use log::info;
use tokio::time::Instant;

/// A simple timer that logs the runtime of an operation.
pub struct Timer {
    /// Time the operation started.
    start: Instant,
    /// An ID shown in the logs to associate the log messages from the timer with each other-
    id: uuid::Uuid,
    /// A string that is being logged.
    message: String,
    /// True if the timer has been stopped already.
    stopped: bool,
}

impl Timer {
    /// Creates a new Timer instance and starts the timer.
    pub fn new<S: AsRef<str>>(message: S) -> Self {
        let id = uuid::Uuid::new_v4();
        info!("[{}] START: {}", id, message.as_ref());
        Self {
            start: Instant::now(),
            id,
            message: message.as_ref().to_string(),
            stopped: false,
        }
    }

    /// Stops the timer and logs the result.
    pub fn stop<T>(&mut self, result: T) -> T {
        if !self.stopped {
            let runtime = self.start.elapsed().as_secs_f32();
            info!(
                "[{}] FINISHED in {:.3}s: {}",
                self.id, runtime, self.message
            );
            self.stopped = true;
        }

        result
    }

    /// Stop the timer prematurely and logs an error.
    pub fn force_stop(&mut self) {
        if !self.stopped {
            let runtime = self.start.elapsed().as_secs_f32();
            error!(
                "[{}] FAILED after {:.3}s: {}",
                self.id, runtime, self.message
            );
            self.stopped = true;
        }
    }
}

/// Automatically report the runtime when the object is dropped.
impl Drop for Timer {
    fn drop(&mut self) {
        self.force_stop();
    }
}
