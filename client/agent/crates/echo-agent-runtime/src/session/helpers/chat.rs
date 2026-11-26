//! Shared text helper for the session summarizers and prompt builders.

/// Returns the largest valid UTF-8 character boundary index at or before `index`.
#[inline]
pub(super) fn floor_char_boundary(s: &str, index: usize) -> usize {
    if index >= s.len() {
        s.len()
    } else if s.is_char_boundary(index) {
        index
    } else {
        // UTF-8 characters are at most 4 bytes, back up at most 3 bytes
        let mut i = index;
        while i > 0 && !s.is_char_boundary(i) {
            i -= 1;
        }
        i
    }
}
