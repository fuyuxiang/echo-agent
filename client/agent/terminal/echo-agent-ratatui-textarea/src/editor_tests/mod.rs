use std::ops::Range;

use unicode_segmentation::UnicodeSegmentation as _;

use super::EditDelta;

mod editing;
mod keys;
mod planning;
mod viewport;

fn delta(replaced_byte_range: Range<usize>, inserted_byte_range: Range<usize>) -> EditDelta {
    EditDelta {
        replaced_byte_range,
        inserted_byte_range,
    }
}

fn is_extended_grapheme_boundary(text: &str, byte: usize) -> bool {
    byte == text.len() || text.grapheme_indices(true).any(|(index, _)| index == byte)
}
