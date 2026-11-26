//! Arena-based string interner for memory-efficient string deduplication.
//!
//! This module provides a string interner that stores all strings in a single
//! contiguous buffer, minimizing allocations and improving cache locality.
//! It uses hash-based lookup for O(1) interning operations.
//!
//! # Design
//!
//! The interner uses a two-level lookup approach:
//! 1. **Primary lookup**: HashMap from 64-bit hash -> list of StringIds with that hash
//! 2. **Collision resolution**: When hashes collide, actual string content is compared
//!
//! This gives O(1) average case for both `intern()` and `get_id()` operations.
//!
//! # Example
//!
//! ```
//! use echo_agent_codebase_graph::interner::StringInterner;
//!
//! let mut interner = StringInterner::new();
//!
//! let id1 = interner.intern("hello");
//! let id2 = interner.intern("world");
//! let id3 = interner.intern("hello"); // Returns same id as id1
//!
//! assert_eq!(id1, id3);
//! assert_ne!(id1, id2);
//! assert_eq!(interner.get(id1), Some("hello"));
//! ```

use std::hash::{Hash, Hasher};

use hashbrown::HashMap;
use nohash_hasher::BuildNoHashHasher;
use rustc_hash::FxHasher;
use serde::{Deserialize, Serialize};
use smallvec::SmallVec;

/// Type alias for HashMap with u64 keys that are already hashed.
/// Uses NoHashHasher since keys don't need re-hashing.
type U64NoHashMap<V> = HashMap<u64, V, BuildNoHashHasher<u64>>;

/// A compact identifier for an interned string.
/// Using u32 allows up to 4 billion unique strings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct StringId(u32);

impl StringId {
    /// Create a new StringId from a raw u32 value.
    #[inline]
    pub const fn new(id: u32) -> Self {
        Self(id)
    }

    /// Get the raw u32 value.
    #[inline]
    pub const fn as_u32(self) -> u32 {
        self.0
    }
}

/// Arena-based string interner for efficient string deduplication.
///
/// Stores all strings in a single contiguous buffer to minimize allocations
/// and improve cache locality. Uses a hash-based lookup for O(1) interning.
///
/// The interner stores arbitrary byte sequences, supporting paths and strings
/// that may not be valid UTF-8.
#[derive(Debug, Clone)]
pub struct StringInterner {
    /// Contiguous storage for all interned byte strings
    arena: Vec<u8>,
    /// Maps hash -> StringId(s). Most buckets have exactly one entry.
    /// Using SmallVec<[StringId; 1]> optimizes for the common case of no collisions.
    /// Uses NoHashHasher since keys are already hashed.
    lookup: U64NoHashMap<SmallVec<[StringId; 1]>>,
    /// Maps StringId to (start, len) in arena
    offsets: Vec<(u32, u16)>,
}

impl Default for StringInterner {
    fn default() -> Self {
        Self::new()
    }
}

impl StringInterner {
    /// Create a new empty interner.
    pub fn new() -> Self {
        Self {
            arena: Vec::new(),
            lookup: U64NoHashMap::default(),
            offsets: Vec::new(),
        }
    }

    /// Create an interner with pre-allocated capacity.
    ///
    /// # Arguments
    /// * `string_bytes` - Estimated total bytes for all strings
    /// * `num_strings` - Estimated number of unique strings
    pub fn with_capacity(string_bytes: usize, num_strings: usize) -> Self {
        Self {
            arena: Vec::with_capacity(string_bytes),
            lookup: U64NoHashMap::with_capacity_and_hasher(
                num_strings,
                BuildNoHashHasher::default(),
            ),
            offsets: Vec::with_capacity(num_strings),
        }
    }

    /// Intern a byte string, returning its StringId.
    /// If the string is already interned, returns the existing id.
    ///
    /// # Complexity
    /// O(1) average case, O(k) worst case where k is the number of
    /// hash collisions (typically 0 or 1).
    pub fn intern_bytes(&mut self, s: &[u8]) -> StringId {
        let hash = Self::hash_bytes(s);

        // Check if already interned
        if let Some(ids) = self.lookup.get(&hash) {
            for &id in ids {
                if self.get_bytes(id) == Some(s) {
                    return id;
                }
            }
        }

        // Not found, add new
        let start = self.arena.len() as u32;
        let len = s.len() as u16;

        self.arena.extend_from_slice(s);

        let id = StringId::new(self.offsets.len() as u32);
        self.offsets.push((start, len));

        // Add to lookup
        self.lookup.entry(hash).or_default().push(id);

        id
    }

    /// Intern a UTF-8 string. Convenience wrapper around `intern_bytes`.
    #[inline]
    pub fn intern(&mut self, s: &str) -> StringId {
        self.intern_bytes(s.as_bytes())
    }

    /// Get the StringId for a byte string without interning it.
    /// Returns None if the string is not in the interner.
    ///
    /// # Complexity
    /// O(1) average case.
    pub fn get_bytes_id(&self, s: &[u8]) -> Option<StringId> {
        let hash = Self::hash_bytes(s);

        if let Some(ids) = self.lookup.get(&hash) {
            for &id in ids {
                if self.get_bytes(id) == Some(s) {
                    return Some(id);
                }
            }
        }
        None
    }

    /// Get the StringId for a UTF-8 string without interning it.
    #[inline]
    pub fn get_id(&self, s: &str) -> Option<StringId> {
        self.get_bytes_id(s.as_bytes())
    }

    /// Get the raw bytes for a StringId.
    ///
    /// # Complexity
    /// O(1)
    pub fn get_bytes(&self, id: StringId) -> Option<&[u8]> {
        let (start, len) = *self.offsets.get(id.0 as usize)?;
        self.arena
            .get(start as usize..(start as usize + len as usize))
    }

    /// Get the string for a StringId, if it's valid UTF-8.
    ///
    /// # Complexity
    /// O(1)
    pub fn get(&self, id: StringId) -> Option<&str> {
        self.get_bytes(id).and_then(|b| std::str::from_utf8(b).ok())
    }

    /// Get the string for a StringId, with lossy UTF-8 conversion.
    /// Invalid UTF-8 sequences are replaced with the replacement character.
    pub fn get_lossy(&self, id: StringId) -> Option<std::borrow::Cow<'_, str>> {
        self.get_bytes(id).map(String::from_utf8_lossy)
    }

    /// Number of interned strings.
    #[inline]
    pub fn len(&self) -> usize {
        self.offsets.len()
    }

    /// Check if the interner is empty.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.offsets.is_empty()
    }

    /// Total bytes used by the arena.
    #[inline]
    pub fn arena_bytes(&self) -> usize {
        self.arena.len()
    }

    /// Compute FxHash of a byte slice.
    #[inline]
    fn hash_bytes(s: &[u8]) -> u64 {
        let mut hasher = FxHasher::default();
        s.hash(&mut hasher);
        hasher.finish()
    }

    /// Iterate over all strings with their IDs (only valid UTF-8).
    pub fn iter(&self) -> impl Iterator<Item = (StringId, &str)> {
        self.offsets
            .iter()
            .enumerate()
            .filter_map(|(idx, &(start, len))| {
                let bytes = self
                    .arena
                    .get(start as usize..(start as usize + len as usize))?;
                let s = std::str::from_utf8(bytes).ok()?;
                Some((StringId::new(idx as u32), s))
            })
    }

    /// Iterate over all byte strings with their IDs.
    pub fn iter_bytes(&self) -> impl Iterator<Item = (StringId, &[u8])> {
        self.offsets
            .iter()
            .enumerate()
            .filter_map(|(idx, &(start, len))| {
                let bytes = self
                    .arena
                    .get(start as usize..(start as usize + len as usize))?;
                Some((StringId::new(idx as u32), bytes))
            })
    }

    /// Clear the interner, removing all strings but keeping allocated capacity.
    pub fn clear(&mut self) {
        self.arena.clear();
        self.lookup.clear();
        self.offsets.clear();
    }

    /// Get the internal arena for serialization purposes.
    pub fn arena(&self) -> &[u8] {
        &self.arena
    }

    /// Get the internal offsets for serialization purposes.
    pub fn offsets(&self) -> &[(u32, u16)] {
        &self.offsets
    }

    /// Release over-allocated capacity in the arena and offsets buffers.
    ///
    /// After a bulk build the arena and offsets Vecs may hold up to 2× their
    /// actual content due to doubling growth.  Calling this reclaims that
    /// wasted heap.  The lookup table is intentionally left unshrunk because
    /// it benefits from load-factor headroom.
    ///
    /// This is an internal maintenance hook called by `ScopeGraphIndex::compact()`.
    pub(crate) fn shrink_to_fit(&mut self) {
        self.arena.shrink_to_fit();
        self.offsets.shrink_to_fit();
    }

    /// Reconstruct an interner from serialized data.
    ///
    /// This rebuilds the lookup table from the arena and offsets.
    pub fn from_parts(arena: Vec<u8>, offsets: Vec<(u32, u16)>) -> Self {
        let mut lookup: U64NoHashMap<SmallVec<[StringId; 1]>> =
            U64NoHashMap::with_capacity_and_hasher(offsets.len(), BuildNoHashHasher::default());

        for (idx, &(start, len)) in offsets.iter().enumerate() {
            if let Some(bytes) = arena.get(start as usize..(start as usize + len as usize)) {
                let hash = Self::hash_bytes(bytes);
                let id = StringId::new(idx as u32);
                lookup.entry(hash).or_default().push(id);
            }
        }

        Self {
            arena,
            lookup,
            offsets,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_basic_interning() {
        let mut interner = StringInterner::new();

        let id1 = interner.intern("src");
        let id2 = interner.intern("lib");
        let id3 = interner.intern("src"); // duplicate

        assert_eq!(id1, id3);
        assert_ne!(id1, id2);
        assert_eq!(interner.get(id1), Some("src"));
        assert_eq!(interner.get(id2), Some("lib"));
        assert_eq!(interner.len(), 2);
    }

    #[test]
    fn test_get_id() {
        let mut interner = StringInterner::new();

        let id_src = interner.intern("src");
        let id_lib = interner.intern("lib");

        assert_eq!(interner.get_id("src"), Some(id_src));
        assert_eq!(interner.get_id("lib"), Some(id_lib));
        assert_eq!(interner.get_id("nonexistent"), None);

        // get_id should not modify the interner
        assert_eq!(interner.len(), 2);
    }

    #[test]
    fn test_bytes_interning() {
        let mut interner = StringInterner::new();

        // Valid UTF-8
        let id1 = interner.intern_bytes(b"hello");
        assert_eq!(interner.get(id1), Some("hello"));

        // Invalid UTF-8
        let invalid_utf8: &[u8] = &[0x80, 0x81, 0x82];
        let id2 = interner.intern_bytes(invalid_utf8);
        assert_eq!(interner.get(id2), None); // Not valid UTF-8
        assert_eq!(interner.get_bytes(id2), Some(invalid_utf8));

        // Duplicate bytes return same ID
        let id3 = interner.intern_bytes(invalid_utf8);
        assert_eq!(id2, id3);
    }

    #[test]
    fn test_many_strings() {
        let mut interner = StringInterner::new();

        let count = 10_000;
        let mut ids = Vec::with_capacity(count);

        for i in 0..count {
            let s = format!("string_{}", i);
            ids.push(interner.intern(&s));
        }

        assert_eq!(interner.len(), count);

        // Verify all strings can be looked up
        for (i, &id) in ids.iter().enumerate() {
            let s = format!("string_{}", i);
            assert_eq!(interner.get_id(&s), Some(id));
            assert_eq!(interner.get(id), Some(s.as_str()));
        }
    }

    #[test]
    fn test_from_parts() {
        let mut interner = StringInterner::new();
        interner.intern("hello");
        interner.intern("world");
        interner.intern("foo");

        let arena = interner.arena().to_vec();
        let offsets = interner.offsets().to_vec();

        let restored = StringInterner::from_parts(arena, offsets);

        assert_eq!(restored.len(), 3);
        assert_eq!(restored.get_id("hello"), Some(StringId::new(0)));
        assert_eq!(restored.get_id("world"), Some(StringId::new(1)));
        assert_eq!(restored.get_id("foo"), Some(StringId::new(2)));
    }

    #[test]
    fn test_clear() {
        let mut interner = StringInterner::new();
        interner.intern("hello");
        interner.intern("world");

        assert_eq!(interner.len(), 2);

        interner.clear();

        assert_eq!(interner.len(), 0);
        assert!(interner.is_empty());
        assert_eq!(interner.get_id("hello"), None);
    }
}
