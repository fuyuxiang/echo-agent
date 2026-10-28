//! Own memory work so shutdown can cancel and join it before the final disk ACK.
use std::cell::{Cell, RefCell};

#[derive(Default)]
pub struct MemoryJobs {
    stopping: Cell<bool>,
    tasks: RefCell<Vec<tokio::task::JoinHandle<()>>>,
    completed: RefCell<std::collections::VecDeque<String>>,
}

impl MemoryJobs {
    pub fn completed(&self, identity: &str) -> bool {
        self.completed
            .borrow()
            .iter()
            .any(|saved| saved == identity)
    }

    pub fn mark_completed(&self, identity: String) {
        let mut completed = self.completed.borrow_mut();
        completed.push_back(identity);
        if completed.len() > 32 {
            completed.pop_front();
        }
    }
    pub fn is_stopping(&self) -> bool {
        self.stopping.get()
    }

    pub fn spawn(&self, work: impl std::future::Future<Output = ()> + 'static) {
        if self.stopping.get() {
            return;
        }
        let mut tasks = self.tasks.borrow_mut();
        tasks.retain(|task| !task.is_finished());
        tasks.push(tokio::task::spawn_local(work));
    }

    pub async fn stop(&self) {
        self.stopping.set(true);
        let tasks = std::mem::take(&mut *self.tasks.borrow_mut());
        for task in &tasks {
            task.abort();
        }
        for task in tasks {
            let _ = task.await;
        }
    }
}

impl Drop for MemoryJobs {
    fn drop(&mut self) {
        for task in self.tasks.get_mut() {
            task.abort();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn shutdown_joins_cancellation_and_prevents_new_memory_work() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let jobs = MemoryJobs::default();
                let dropped = std::rc::Rc::new(Cell::new(false));
                struct Guard(std::rc::Rc<Cell<bool>>);
                impl Drop for Guard {
                    fn drop(&mut self) {
                        self.0.set(true);
                    }
                }
                let marker = dropped.clone();
                jobs.spawn(async move {
                    let _guard = Guard(marker);
                    std::future::pending::<()>().await;
                });
                tokio::task::yield_now().await;
                jobs.stop().await;
                assert!(
                    dropped.get(),
                    "the final save must follow cancellation cleanup"
                );
                jobs.spawn(async { panic!("memory work must not start during shutdown") });
                tokio::task::yield_now().await;
                jobs.stop().await;
            })
            .await;
    }
}
