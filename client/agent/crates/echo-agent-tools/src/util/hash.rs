//! Shared hashing utilities for hashline anchor generation.
//!
//! Provides FNV-1a 32-bit hashing and whitespace-normalized line fingerprinting.
//! Used by the `echo_agent_build_hashline` anchor schemes.
//!
//! ## Normalization policy
//!
//! Before hashing, lines are normalized: leading/trailing whitespace is trimmed
//! and internal whitespace runs are collapsed to a single ASCII space. This keeps
//! anchors stable across formatter-only edits (indentation, trailing whitespace,
//! tab/space normalization) while still distinguishing meaningful content changes
//! (e.g. `return x` vs `returnx`).

/// FNV-1a 32-bit offset basis.
const FNV_OFFSET: u32 = 2_166_136_261;

/// FNV-1a 32-bit prime.
const FNV_PRIME: u32 = 16_777_619;

/// Compute FNV-1a 32-bit hash of raw bytes.
///
/// This is the low-level primitive — callers that want whitespace-normalized
/// fingerprints should use [`line_hash`] instead.
pub fn fnv1a_32(data: &[u8]) -> u32 {
    let mut h: u32 = FNV_OFFSET;
    for &byte in data {
        h ^= byte as u32;
        h = h.wrapping_mul(FNV_PRIME);
    }
    h
}

/// Compute a whitespace-normalized FNV-1a 32-bit fingerprint of a single line.
///
/// Normalization: `trim()` + collapse internal whitespace runs to a single
/// ASCII space. The hash is computed over the normalized byte sequence.
///
/// Returns the raw `u32` hash. Use [`encode_hash`] to convert to a compact
/// letter-based anchor string.
pub fn line_hash(line: &str) -> u32 {
    let mut h: u32 = FNV_OFFSET;
    let mut prev_ws = false;

    for byte in line.trim().bytes() {
        if byte.is_ascii_whitespace() {
            if !prev_ws {
                h ^= b' ' as u32;
                h = h.wrapping_mul(FNV_PRIME);
                prev_ws = true;
            }
        } else {
            h ^= byte as u32;
            h = h.wrapping_mul(FNV_PRIME);
            prev_ws = false;
        }
    }

    h
}

/// Encode a 32-bit hash as `n` lowercase ASCII letters (a–z).
///
/// Each letter is derived from a different byte region of the hash to spread
/// entropy. The default anchor length for benchmarking is 3; 2 is retained
/// as a control configuration.
///
/// # Panics
///
/// Panics if `len` is 0 or greater than 4.
pub fn encode_hash(hash: u32, len: usize) -> String {
    assert!(len > 0 && len <= 4, "encode_hash: len must be 1..=4");

    let mut result = String::with_capacity(len);
    for i in 0..len {
        let byte = ((hash >> (i * 8)) % 26) as u8 + b'a';
        result.push(byte as char);
    }
    result
}

/// Default anchor hash length (3 lowercase letters).
pub const DEFAULT_HASH_LEN: usize = 3;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fnv1a_32_empty() {
        // FNV-1a of empty input is the offset basis.
        assert_eq!(fnv1a_32(b""), FNV_OFFSET);
    }

    #[test]
    fn fnv1a_32_deterministic() {
        let a = fnv1a_32(b"hello world");
        let b = fnv1a_32(b"hello world");
        assert_eq!(a, b);
    }

    #[test]
    fn fnv1a_32_different_inputs_differ() {
        assert_ne!(fnv1a_32(b"hello"), fnv1a_32(b"world"));
    }

    #[test]
    fn line_hash_deterministic() {
        let a = line_hash("  let x = 1;  ");
        let b = line_hash("  let x = 1;  ");
        assert_eq!(a, b);
    }

    #[test]
    fn line_hash_whitespace_normalization_indentation() {
        // Different indentation → same hash.
        let a = line_hash("    let x = 1;");
        let b = line_hash("  let x = 1;");
        let c = line_hash("\tlet x = 1;");
        assert_eq!(a, b);
        assert_eq!(b, c);
    }

    #[test]
    fn line_hash_whitespace_normalization_trailing() {
        let a = line_hash("let x = 1;");
        let b = line_hash("let x = 1;   ");
        let c = line_hash("let x = 1;\t");
        assert_eq!(a, b);
        assert_eq!(b, c);
    }

    #[test]
    fn line_hash_whitespace_normalization_internal_collapse() {
        // Multiple internal spaces collapse to one.
        let a = line_hash("let x = 1;");
        let b = line_hash("let  x  =  1;");
        assert_eq!(a, b);
    }

    #[test]
    fn line_hash_preserves_token_boundaries() {
        // "return x" vs "returnx" must differ.
        let a = line_hash("return x");
        let b = line_hash("returnx");
        assert_ne!(a, b);
    }

    #[test]
    fn line_hash_empty_line() {
        // Empty and whitespace-only lines should hash the same.
        let a = line_hash("");
        let b = line_hash("   ");
        let c = line_hash("\t\t");
        assert_eq!(a, b);
        assert_eq!(b, c);
    }

    #[test]
    fn line_hash_content_changes_differ() {
        assert_ne!(line_hash("let x = 1;"), line_hash("let y = 1;"));
        assert_ne!(line_hash("let x = 1;"), line_hash("let x = 2;"));
    }

    #[test]
    fn encode_hash_length() {
        let h = fnv1a_32(b"test");
        assert_eq!(encode_hash(h, 2).len(), 2);
        assert_eq!(encode_hash(h, 3).len(), 3);
        assert_eq!(encode_hash(h, 4).len(), 4);
    }

    #[test]
    fn encode_hash_lowercase_letters() {
        let h = fnv1a_32(b"test");
        let encoded = encode_hash(h, 3);
        assert!(encoded.chars().all(|c| c.is_ascii_lowercase()));
    }

    #[test]
    fn encode_hash_deterministic() {
        let h = fnv1a_32(b"test");
        assert_eq!(encode_hash(h, 3), encode_hash(h, 3));
    }

    #[test]
    #[should_panic(expected = "len must be 1..=4")]
    fn encode_hash_zero_len_panics() {
        encode_hash(0, 0);
    }

    #[test]
    #[should_panic(expected = "len must be 1..=4")]
    fn encode_hash_five_len_panics() {
        encode_hash(0, 5);
    }

    #[test]
    fn encode_hash_different_hashes_differ() {
        let a = encode_hash(fnv1a_32(b"hello"), 3);
        let b = encode_hash(fnv1a_32(b"world"), 3);
        assert_ne!(a, b);
    }
}
