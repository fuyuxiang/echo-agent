//! Shared event queue for the push path: producers enqueue out-of-band events;
//! readers drain the ones relevant to them at hook points. Internally
//! synchronized and `Arc`-shared (clones share one queue), mirroring
//! [`crate::buffer::InterjectionBuffer`].

use std::sync::{Arc, Mutex, MutexGuard};

#[derive(Debug)]
pub struct EventQueue<E> {
    events: Arc<Mutex<Vec<E>>>,
}

impl<E> Clone for EventQueue<E> {
    fn clone(&self) -> Self {
        Self {
            events: Arc::clone(&self.events),
        }
    }
}

impl<E> Default for EventQueue<E> {
    fn default() -> Self {
        Self::new()
    }
}

impl<E> EventQueue<E> {
    pub fn new() -> Self {
        Self {
            events: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Producer hook: record an event for later draining.
    pub fn push(&self, event: E) {
        self.lock().push(event);
    }

    /// Push, then drop the oldest events so at most `max` remain.
    pub fn push_capped(&self, event: E, max: usize) {
        let mut q = self.lock();
        q.push(event);
        if q.len() > max {
            let excess = q.len() - max;
            q.drain(..excess);
        }
    }

    pub fn len(&self) -> usize {
        self.lock().len()
    }

    pub fn is_empty(&self) -> bool {
        self.lock().is_empty()
    }

    /// Remove and return events matching `take`, retaining the rest. FIFO order
    /// is preserved in both the returned and retained sets.
    pub fn drain_matching(&self, take: impl Fn(&E) -> bool) -> Vec<E> {
        let mut q = self.lock();
        let (matched, kept): (Vec<E>, Vec<E>) =
            std::mem::take(&mut *q).into_iter().partition(|e| take(e));
        *q = kept;
        matched
    }

    /// Remove and return all events, leaving the queue empty (FIFO order).
    pub fn drain_all(&self) -> Vec<E> {
        std::mem::take(&mut *self.lock())
    }

    /// Discard all events.
    pub fn clear(&self) {
        self.lock().clear();
    }

    fn lock(&self) -> MutexGuard<'_, Vec<E>> {
        self.events.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl<E: Clone> EventQueue<E> {
    /// Clone of the current events, for inspection without draining.
    pub fn snapshot(&self) -> Vec<E> {
        self.lock().clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_and_len() {
        let q: EventQueue<u32> = EventQueue::new();
        assert!(q.is_empty());
        q.push(1);
        q.push(2);
        assert_eq!(q.len(), 2);
    }

    #[test]
    fn clones_share_one_queue() {
        let q: EventQueue<u32> = EventQueue::new();
        let q2 = q.clone();
        q.push(7);
        assert_eq!(q2.len(), 1);
    }

    #[test]
    fn push_capped_drops_oldest() {
        let q: EventQueue<u32> = EventQueue::new();
        for i in 0..5 {
            q.push_capped(i, 3);
        }
        assert_eq!(q.drain_matching(|_| true), vec![2, 3, 4]);
        assert!(q.is_empty());
    }

    #[test]
    fn drain_matching_returns_matched_retains_rest_fifo() {
        let q: EventQueue<u32> = EventQueue::new();
        for i in 0..6 {
            q.push(i);
        }
        let evens = q.drain_matching(|n| n % 2 == 0);
        assert_eq!(evens, vec![0, 2, 4]);
        assert_eq!(q.drain_matching(|_| true), vec![1, 3, 5]);
    }

    #[test]
    fn push_capped_under_limit_keeps_all() {
        let q: EventQueue<u32> = EventQueue::new();
        q.push_capped(1, 5);
        q.push_capped(2, 5);
        assert_eq!(q.drain_matching(|_| true), vec![1, 2]);
    }

    #[test]
    fn drain_matching_none_match_retains_all() {
        let q: EventQueue<u32> = EventQueue::new();
        q.push(1);
        q.push(2);
        assert!(q.drain_matching(|n| *n > 10).is_empty());
        assert_eq!(q.len(), 2);
    }

    #[test]
    fn drain_matching_on_empty_is_empty() {
        let q: EventQueue<u32> = EventQueue::new();
        assert!(q.drain_matching(|_| true).is_empty());
    }

    #[test]
    fn drain_all_empties_in_fifo_order() {
        let q: EventQueue<u32> = EventQueue::new();
        q.push(1);
        q.push(2);
        assert_eq!(q.drain_all(), vec![1, 2]);
        assert!(q.is_empty());
    }

    #[test]
    fn clear_discards_all() {
        let q: EventQueue<u32> = EventQueue::new();
        q.push(1);
        q.clear();
        assert!(q.is_empty());
    }

    #[test]
    fn snapshot_reads_without_draining() {
        let q: EventQueue<u32> = EventQueue::new();
        q.push(9);
        assert_eq!(q.snapshot(), vec![9]);
        assert_eq!(q.len(), 1);
    }
}
