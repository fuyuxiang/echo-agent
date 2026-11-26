//! Reusable text-search primitives.
//!
//! [`TextMatcher`] compiles a substring or regex query (smart-case) once and
//! answers match queries; it owns no corpus and no UI state. The free
//! [`next_index_after`] / [`prev_index_before`] helpers wrap-navigate a sorted
//! slice of match positions for `n`/`N` traversal.

pub mod matcher;

pub use matcher::{QueryKind, TextMatcher};

/// Position in ascending `sorted` of the first index after `current`, wrapping
/// to the front. `None` when `sorted` is empty.
pub fn next_index_after(sorted: &[usize], current: usize) -> Option<usize> {
    if sorted.is_empty() {
        return None;
    }
    let pos = sorted.partition_point(|&i| i <= current);
    Some(if pos < sorted.len() { pos } else { 0 })
}

/// Position in ascending `sorted` of the last index before `current`, wrapping
/// to the back. `None` when `sorted` is empty.
pub fn prev_index_before(sorted: &[usize], current: usize) -> Option<usize> {
    if sorted.is_empty() {
        return None;
    }
    let pos = sorted.partition_point(|&i| i < current);
    Some(if pos > 0 { pos - 1 } else { sorted.len() - 1 })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn next_index_after_wraps() {
        let sorted = [0usize, 2, 4];
        assert_eq!(next_index_after(&sorted, 0), Some(1));
        assert_eq!(next_index_after(&sorted, 2), Some(2));
        assert_eq!(next_index_after(&sorted, 4), Some(0));
        assert_eq!(next_index_after(&[], 3), None);
        let one = [5usize];
        assert_eq!(next_index_after(&one, 5), Some(0));
        assert_eq!(next_index_after(&one, 1), Some(0));
    }

    #[test]
    fn prev_index_before_wraps() {
        let sorted = [0usize, 2, 4];
        assert_eq!(prev_index_before(&sorted, 4), Some(1));
        assert_eq!(prev_index_before(&sorted, 2), Some(0));
        assert_eq!(prev_index_before(&sorted, 0), Some(2));
        assert_eq!(prev_index_before(&[], 3), None);
        let one = [5usize];
        assert_eq!(prev_index_before(&one, 5), Some(0));
        assert_eq!(prev_index_before(&one, 9), Some(0));
    }
}
