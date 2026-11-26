//! Layout cache for the list pane.
//!
//! Tracks per-item heights and prefix sums so that scroll-position ↔ item-index
//! conversions are fast.  Two variants:
//!
//! - [`FixedHeight`] — all items have height 1 (NoWrap mode).  Everything is O(1).
//! - [`Variable`]    — items have different heights (Wrap mode).  Uses a prefix-sum
//!   vec for O(log n) position lookups.

/// Wrap mode for the list pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WrapMode {
    /// Soft-wrap lines at viewport width.  Variable height per item.
    /// Requires full layout cache.
    Wrap,
    /// No wrapping — each item is exactly 1 visual line, truncated with `…`.
    /// Layout is trivial O(1).
    NoWrap,
}

/// Layout cache — an enum to support the fixed-height fast path.
#[derive(Debug, Clone)]
pub enum ListLayoutCache {
    /// All items are height 1 (NoWrap mode).  No allocation needed.
    FixedHeight {
        /// Number of items (= total height in visual lines).
        count: usize,
    },
    /// Variable-height items (Wrap mode).
    Variable {
        /// Width at which heights were computed.
        width: u16,
        /// Per-item heights (indexed by *visible* index when filtered).
        heights: Vec<u16>,
        /// Prefix sums: `prefix_sums[i]` = sum of `heights[0..i]`.
        ///
        /// Length = `heights.len() + 1`.  `prefix_sums[0] = 0`.
        /// `prefix_sums[n] = total_height`.
        prefix_sums: Vec<usize>,
    },
}

impl ListLayoutCache {
    // -----------------------------------------------------------------------
    // Constructors
    // -----------------------------------------------------------------------

    /// Create a fixed-height cache for `count` items (all height 1).
    pub fn fixed(count: usize) -> Self {
        Self::FixedHeight { count }
    }

    /// Build a variable-height cache from an iterator of per-item heights.
    pub fn from_heights(width: u16, heights: impl IntoIterator<Item = u16>) -> Self {
        let heights: Vec<u16> = heights.into_iter().collect();
        let mut prefix_sums = Vec::with_capacity(heights.len() + 1);
        prefix_sums.push(0);
        for &h in &heights {
            let prev = *prefix_sums.last().unwrap();
            prefix_sums.push(prev + h as usize);
        }
        Self::Variable {
            width,
            heights,
            prefix_sums,
        }
    }

    /// Extend an existing `Variable` cache with additional item heights.
    ///
    /// Used for **incremental append**: when new items arrive, we compute
    /// heights only for the new items and extend the prefix-sum array.
    ///
    /// Panics if `self` is `FixedHeight` — caller must ensure the mode matches.
    pub fn extend_heights(&mut self, new_heights: impl IntoIterator<Item = u16>) {
        match self {
            Self::Variable {
                heights,
                prefix_sums,
                ..
            } => {
                for h in new_heights {
                    let prev = *prefix_sums.last().unwrap();
                    prefix_sums.push(prev + h as usize);
                    heights.push(h);
                }
            }
            Self::FixedHeight { .. } => {
                panic!("extend_heights called on FixedHeight cache");
            }
        }
    }

    // -----------------------------------------------------------------------
    // Queries
    // -----------------------------------------------------------------------

    /// Total height in visual lines.
    pub fn total_height(&self) -> usize {
        match self {
            Self::FixedHeight { count } => *count,
            Self::Variable { prefix_sums, .. } => *prefix_sums.last().unwrap_or(&0),
        }
    }

    /// Number of items in the cache.
    pub fn item_count(&self) -> usize {
        match self {
            Self::FixedHeight { count } => *count,
            Self::Variable { heights, .. } => heights.len(),
        }
    }

    /// Virtual-y position (in visual lines from top) of item at `idx`.
    ///
    /// For `FixedHeight`, this is just `idx`.
    pub fn virtual_y(&self, idx: usize) -> usize {
        match self {
            Self::FixedHeight { .. } => idx,
            Self::Variable { prefix_sums, .. } => prefix_sums.get(idx).copied().unwrap_or(0),
        }
    }

    /// Height of item at `idx` in visual lines.
    pub fn item_height(&self, idx: usize) -> u16 {
        match self {
            Self::FixedHeight { .. } => 1,
            Self::Variable { heights, .. } => heights.get(idx).copied().unwrap_or(1),
        }
    }

    /// Find the item index whose virtual-y range contains `y`.
    ///
    /// For `FixedHeight`, this is just `y` (clamped to `count - 1`).
    /// For `Variable`, binary search on prefix sums — O(log n).
    ///
    /// Returns `None` if the cache is empty.
    pub fn item_at_y(&self, y: usize) -> Option<usize> {
        match self {
            Self::FixedHeight { count } => {
                if *count == 0 {
                    None
                } else {
                    Some(y.min(*count - 1))
                }
            }
            Self::Variable { prefix_sums, .. } => {
                if prefix_sums.len() <= 1 {
                    return None; // empty
                }
                // Binary search: find the largest i such that prefix_sums[i] <= y.
                // partition_point returns the first index where prefix_sums[i] > y,
                // so we subtract 1.
                let pos = prefix_sums.partition_point(|&s| s <= y);
                let idx = pos.saturating_sub(1);
                // Clamp to valid item range
                let max_idx = prefix_sums.len() - 2; // last valid item index
                Some(idx.min(max_idx))
            }
        }
    }

    /// Width at which this cache was computed (only meaningful for `Variable`).
    pub fn cached_width(&self) -> Option<u16> {
        match self {
            Self::FixedHeight { .. } => None,
            Self::Variable { width, .. } => Some(*width),
        }
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_height_basics() {
        let cache = ListLayoutCache::fixed(5);
        assert_eq!(cache.total_height(), 5);
        assert_eq!(cache.item_count(), 5);
        assert_eq!(cache.virtual_y(0), 0);
        assert_eq!(cache.virtual_y(3), 3);
        assert_eq!(cache.item_height(0), 1);
        assert_eq!(cache.item_height(4), 1);
        assert_eq!(cache.item_at_y(0), Some(0));
        assert_eq!(cache.item_at_y(4), Some(4));
        // Clamped
        assert_eq!(cache.item_at_y(100), Some(4));
    }

    #[test]
    fn fixed_height_empty() {
        let cache = ListLayoutCache::fixed(0);
        assert_eq!(cache.total_height(), 0);
        assert_eq!(cache.item_count(), 0);
        assert_eq!(cache.item_at_y(0), None);
    }

    #[test]
    fn variable_height_basics() {
        // Items with heights: 3, 1, 2, 4
        let cache = ListLayoutCache::from_heights(80, vec![3, 1, 2, 4]);
        assert_eq!(cache.total_height(), 10);
        assert_eq!(cache.item_count(), 4);

        // virtual_y positions: 0, 3, 4, 6
        assert_eq!(cache.virtual_y(0), 0);
        assert_eq!(cache.virtual_y(1), 3);
        assert_eq!(cache.virtual_y(2), 4);
        assert_eq!(cache.virtual_y(3), 6);

        assert_eq!(cache.item_height(0), 3);
        assert_eq!(cache.item_height(1), 1);
        assert_eq!(cache.item_height(2), 2);
        assert_eq!(cache.item_height(3), 4);
    }

    #[test]
    fn variable_height_item_at_y() {
        // Items with heights: 3, 1, 2, 4  → prefix_sums: [0, 3, 4, 6, 10]
        let cache = ListLayoutCache::from_heights(80, vec![3, 1, 2, 4]);

        // y=0,1,2 → item 0
        assert_eq!(cache.item_at_y(0), Some(0));
        assert_eq!(cache.item_at_y(1), Some(0));
        assert_eq!(cache.item_at_y(2), Some(0));
        // y=3 → item 1
        assert_eq!(cache.item_at_y(3), Some(1));
        // y=4,5 → item 2
        assert_eq!(cache.item_at_y(4), Some(2));
        assert_eq!(cache.item_at_y(5), Some(2));
        // y=6,7,8,9 → item 3
        assert_eq!(cache.item_at_y(6), Some(3));
        assert_eq!(cache.item_at_y(9), Some(3));
        // y=10+ → clamped to item 3
        assert_eq!(cache.item_at_y(10), Some(3));
        assert_eq!(cache.item_at_y(100), Some(3));
    }

    #[test]
    fn variable_height_empty() {
        let cache = ListLayoutCache::from_heights(80, Vec::<u16>::new());
        assert_eq!(cache.total_height(), 0);
        assert_eq!(cache.item_count(), 0);
        assert_eq!(cache.item_at_y(0), None);
    }

    #[test]
    fn variable_height_single_item() {
        let cache = ListLayoutCache::from_heights(80, vec![5]);
        assert_eq!(cache.total_height(), 5);
        assert_eq!(cache.item_count(), 1);
        assert_eq!(cache.virtual_y(0), 0);
        assert_eq!(cache.item_at_y(0), Some(0));
        assert_eq!(cache.item_at_y(4), Some(0));
        assert_eq!(cache.item_at_y(5), Some(0)); // clamped
    }

    #[test]
    fn cached_width() {
        let fixed = ListLayoutCache::fixed(5);
        assert_eq!(fixed.cached_width(), None);

        let var = ListLayoutCache::from_heights(120, vec![1, 2]);
        assert_eq!(var.cached_width(), Some(120));
    }

    #[test]
    fn extend_heights_appends() {
        let mut cache = ListLayoutCache::from_heights(80, vec![3, 1]);
        assert_eq!(cache.item_count(), 2);
        assert_eq!(cache.total_height(), 4);

        cache.extend_heights(vec![2, 4]);
        assert_eq!(cache.item_count(), 4);
        assert_eq!(cache.total_height(), 10);

        // Prefix sums: [0, 3, 4, 6, 10]
        assert_eq!(cache.virtual_y(0), 0);
        assert_eq!(cache.virtual_y(1), 3);
        assert_eq!(cache.virtual_y(2), 4);
        assert_eq!(cache.virtual_y(3), 6);
        assert_eq!(cache.item_height(2), 2);
        assert_eq!(cache.item_height(3), 4);
    }

    #[test]
    fn extend_heights_empty_iter_is_noop() {
        let mut cache = ListLayoutCache::from_heights(80, vec![3, 1]);
        cache.extend_heights(Vec::<u16>::new());
        assert_eq!(cache.item_count(), 2);
        assert_eq!(cache.total_height(), 4);
    }
}
