//! Position and Range types for representing source code locations.
//!
//! These types follow LSP conventions:
//! - Internally stored as 0-indexed (tree-sitter compatible)
//! - Public API provides both 0-indexed and 1-indexed accessors

use serde::{Deserialize, Serialize};

/// A position in a source file.
///
/// Positions are stored as 0-indexed internally (tree-sitter compatible).
/// Use `line_1indexed()` and `column_1indexed()` for display/LSP output.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash, Default)]
#[serde(rename_all = "camelCase")]
pub struct Position {
    /// Line number (0-indexed)
    line: usize,
    /// Character/column number (0-indexed)
    character: usize,
    /// Byte offset from start of file
    byte_offset: usize,
}

impl Position {
    /// Create a new position (0-indexed).
    pub fn new(line: usize, character: usize, byte_offset: usize) -> Self {
        Self {
            line,
            character,
            byte_offset,
        }
    }

    /// Create a position from a tree-sitter point.
    pub fn from_tree_sitter_point(point: &tree_sitter::Point, byte_offset: usize) -> Self {
        Self {
            line: point.row,
            character: point.column,
            byte_offset,
        }
    }

    /// Convert to tree-sitter Point.
    pub fn to_tree_sitter(&self) -> tree_sitter::Point {
        tree_sitter::Point::new(self.line, self.character)
    }

    /// Get the line number (0-indexed).
    pub fn line(&self) -> usize {
        self.line
    }

    /// Get the line number (1-indexed, for LSP/display).
    pub fn line_1indexed(&self) -> usize {
        self.line + 1
    }

    /// Get the column/character number (0-indexed).
    pub fn column(&self) -> usize {
        self.character
    }

    /// Get the column/character number (1-indexed, for LSP/display).
    pub fn column_1indexed(&self) -> usize {
        self.character + 1
    }

    /// Alias for column() - matches LSP terminology.
    pub fn character(&self) -> usize {
        self.character
    }

    /// Get the byte offset.
    pub fn byte_offset(&self) -> usize {
        self.byte_offset
    }

    /// Get the byte offset (alias).
    pub fn to_byte_offset(&self) -> usize {
        self.byte_offset
    }

    /// Set the byte offset.
    pub fn set_byte_offset(&mut self, byte_offset: usize) {
        self.byte_offset = byte_offset;
    }

    /// Check if this position is before or at another position.
    pub fn before_other(&self, other: &Position) -> bool {
        self.line < other.line || (self.line == other.line && self.character <= other.character)
    }

    /// Check if this position is after or at another position.
    pub fn after_other(&self, other: &Position) -> bool {
        self.line > other.line || (self.line == other.line && self.character >= other.character)
    }

    /// Create a position from a byte offset and line end indices.
    pub fn from_byte(byte: usize, line_end_indices: &[u32]) -> Self {
        let line = line_end_indices
            .iter()
            .position(|&line_end_byte| (line_end_byte as usize) > byte)
            .unwrap_or(0);

        let column = line
            .checked_sub(1)
            .and_then(|idx| line_end_indices.get(idx))
            .map(|&prev_line_end| byte.saturating_sub(prev_line_end as usize))
            .unwrap_or(byte);

        Self::new(line, column, byte)
    }

    /// Shift the column by a given amount.
    pub fn shift_column(self, column_move: usize) -> Self {
        Self {
            line: self.line,
            character: self.character + column_move.saturating_sub(1),
            byte_offset: 0,
        }
    }

    /// Move to the next line.
    pub fn move_to_next_line(mut self) -> Self {
        self.line += 1;
        self.character = 0;
        self.byte_offset = 0;
        self
    }
}

impl From<tree_sitter::Point> for Position {
    fn from(point: tree_sitter::Point) -> Self {
        Self {
            line: point.row,
            character: point.column,
            byte_offset: 0,
        }
    }
}

impl From<Position> for tree_sitter::Point {
    fn from(val: Position) -> Self {
        val.to_tree_sitter()
    }
}

/// A range in a source file.
///
/// Ranges are stored as 0-indexed internally (tree-sitter compatible).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash, Default)]
#[serde(rename_all = "camelCase")]
pub struct Range {
    /// Start position of the range
    start_position: Position,
    /// End position of the range
    end_position: Position,
}

impl Range {
    /// Create a new range from start and end positions.
    pub fn new(start_position: Position, end_position: Position) -> Self {
        Self {
            start_position,
            end_position,
        }
    }

    /// Create a range from a tree-sitter node.
    pub fn for_tree_node(node: &tree_sitter::Node) -> Self {
        let range = node.range();
        Self {
            start_position: Position {
                line: range.start_point.row,
                character: range.start_point.column,
                byte_offset: range.start_byte,
            },
            end_position: Position {
                line: range.end_point.row,
                character: range.end_point.column,
                byte_offset: range.end_byte,
            },
        }
    }

    /// Alias for for_tree_node (compatibility).
    pub fn from_tree_sitter_node(node: &tree_sitter::Node) -> Self {
        Self::for_tree_node(node)
    }

    /// Get the start position.
    pub fn start_position(&self) -> Position {
        self.start_position
    }

    /// Get the end position.
    pub fn end_position(&self) -> Position {
        self.end_position
    }

    /// Get the start position by reference.
    pub fn get_start_position(&self) -> &Position {
        &self.start_position
    }

    /// Get the end position by reference.
    pub fn get_end_position(&self) -> &Position {
        &self.end_position
    }

    /// Set the start position.
    pub fn set_start_position(&mut self, position: Position) {
        self.start_position = position;
    }

    /// Set the end position.
    pub fn set_end_position(&mut self, position: Position) {
        self.end_position = position;
    }

    /// Set the start byte offset.
    pub fn set_start_byte(&mut self, byte: usize) {
        self.start_position.set_byte_offset(byte);
    }

    /// Set the end byte offset.
    pub fn set_end_byte(&mut self, byte: usize) {
        self.end_position.set_byte_offset(byte);
    }

    /// Get the start byte offset.
    pub fn start_byte(&self) -> usize {
        self.start_position.byte_offset
    }

    /// Get the end byte offset.
    pub fn end_byte(&self) -> usize {
        self.end_position.byte_offset
    }

    /// Get the start line (0-indexed).
    pub fn start_line(&self) -> usize {
        self.start_position.line
    }

    /// Get the start line (1-indexed, for LSP/display).
    pub fn start_line_1indexed(&self) -> usize {
        self.start_position.line + 1
    }

    /// Get the end line (0-indexed).
    pub fn end_line(&self) -> usize {
        self.end_position.line
    }

    /// Get the end line (1-indexed, for LSP/display).
    pub fn end_line_1indexed(&self) -> usize {
        self.end_position.line + 1
    }

    /// Get the start column (0-indexed).
    pub fn start_column(&self) -> usize {
        self.start_position.character
    }

    /// Get the start column (1-indexed, for LSP/display).
    pub fn start_column_1indexed(&self) -> usize {
        self.start_position.character + 1
    }

    /// Get the end column (0-indexed).
    pub fn end_column(&self) -> usize {
        self.end_position.character
    }

    /// Get the end column (1-indexed, for LSP/display).
    pub fn end_column_1indexed(&self) -> usize {
        self.end_position.character + 1
    }

    /// Get the byte size of the range.
    pub fn byte_size(&self) -> usize {
        self.end_byte().saturating_sub(self.start_byte()) + 1
    }

    /// Get the number of bytes (alias).
    pub fn len(&self) -> usize {
        self.end_byte().saturating_sub(self.start_byte())
    }

    /// Check if the range is empty (zero bytes).
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Get the line size (number of lines spanned, can be negative for inverted ranges).
    pub fn line_size(&self) -> i64 {
        self.end_line() as i64 - self.start_line() as i64
    }

    /// Check if this range contains another range (using line/column).
    pub fn contains(&self, other: &Range) -> bool {
        self.contains_check_line_column(other)
    }

    /// Check if this range contains another range (line-only check).
    pub fn contains_check_line(&self, other: &Range) -> bool {
        let start_position_check = self.start_line() <= other.start_line();
        let end_position_check = self.end_line() >= other.end_line();
        start_position_check && end_position_check
    }

    /// Check if this range contains another range (line and column check).
    pub fn contains_check_line_column(&self, other: &Range) -> bool {
        let start_position_check = self.start_line() < other.start_line()
            || (self.start_line() == other.start_line()
                && self.start_column() <= other.start_column());
        let end_position_check = self.end_line() > other.end_line()
            || (self.end_line() == other.end_line() && self.end_column() >= other.end_column());
        start_position_check && end_position_check
    }

    /// Check if this range contains a position.
    pub fn contains_position(&self, position: &Position) -> bool {
        self.start_position().before_other(position) && self.end_position().after_other(position)
    }

    /// Check if this range contains a line.
    pub fn contains_line(&self, line: usize) -> bool {
        self.start_position().line() <= line && line <= self.end_position().line()
    }

    /// Check if this range intersects with another (line-based).
    pub fn intersects_without_byte(&self, other: &Range) -> bool {
        // Two ranges intersect if one starts before the other ends AND ends after the other starts
        self.start_line() <= other.end_line() && self.end_line() >= other.start_line()
    }

    /// Convert to tree-sitter Range.
    pub fn to_tree_sitter_range(&self) -> tree_sitter::Range {
        tree_sitter::Range {
            start_byte: self.start_position.byte_offset,
            end_byte: self.end_position.byte_offset,
            start_point: self.start_position.to_tree_sitter(),
            end_point: self.end_position.to_tree_sitter(),
        }
    }

    /// Create a range from byte offsets using line end indices.
    pub fn from_byte_range(range: std::ops::Range<usize>, line_end_indices: &[u32]) -> Range {
        let start = Position::from_byte(range.start, line_end_indices);
        let end = Position::from_byte(range.end, line_end_indices);
        Self::new(start, end)
    }

    /// Check equality based on line numbers only.
    pub fn check_equality_without_byte(&self, other: &Range) -> bool {
        self.start_line() == other.start_line() && self.end_line() == other.end_line()
    }

    /// Check equality based on line ranges.
    pub fn equals_line_range(&self, other: &Range) -> bool {
        self.start_line() == other.start_line() && self.end_line() == other.end_line()
    }
}
