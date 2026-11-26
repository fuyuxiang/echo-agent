/// Known binary file extensions — skip content reading for these.
/// PDF is intentionally excluded since it has dedicated handling.
///
/// NOTE: opencode has its own local copy of this list.
pub const BINARY_EXTENSIONS: &[&str] = &[
    "7z", "a", "avi", "avif", "bin", "bmp", "class", "dat", "dll", "doc", "docx", "dylib", "exe",
    "gif", "gz", "ico", "jar", "jpeg", "jpg", "lib", "mov", "mp3", "mp4", "o", "obj", "odp", "ods",
    "odt", "png", "ppt", "pyc", "pyd", "pyo", "qoi", "rar", "so", "tar", "tif", "tiff", "war",
    "wasm", "webp", "xls", "xlsx", "zip",
];

const SAMPLE_SIZE: usize = 8192;
const NON_PRINTABLE_THRESHOLD: f64 = 0.3;

/// Returns `true` if the file should be treated as binary.
///
/// A file is binary if its extension is in [`BINARY_EXTENSIONS`], or if
/// a significant portion of the first [`SAMPLE_SIZE`] bytes are non-printable.
pub fn is_binary(extension: &str, bytes: &[u8]) -> bool {
    if BINARY_EXTENSIONS.binary_search(&extension).is_ok() {
        return true;
    }
    if bytes.is_empty() {
        return false;
    }

    let sample = &bytes[..bytes.len().min(SAMPLE_SIZE)];

    // Any null byte → binary.
    if sample.contains(&0x00) {
        return true;
    }

    // High ratio of non-printable bytes → binary.
    // Bytes 0-8 and 14-31 are control characters (excluding tab, newline, CR, etc.)
    let non_printable = sample
        .iter()
        .filter(|&&b| b < 9 || (14..=31).contains(&b))
        .count();
    let ratio = non_printable as f64 / sample.len() as f64;

    ratio > NON_PRINTABLE_THRESHOLD
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_known_binary_extensions() {
        for ext in &[
            "zip", "exe", "wasm", "dll", "so", "dylib", "png", "mp4", "jpeg", "jpg", "webp", "tiff",
        ] {
            assert!(is_binary(ext, &[]), "extension '{ext}' should be binary");
        }
    }

    #[test]
    fn pdf_not_in_binary_extensions() {
        assert!(
            !BINARY_EXTENSIONS.contains(&"pdf"),
            "pdf must not be in BINARY_EXTENSIONS"
        );
        assert!(
            !is_binary("pdf", &[]),
            "pdf extension alone should not be binary"
        );
    }

    #[test]
    fn pptx_not_in_binary_extensions() {
        assert!(
            !BINARY_EXTENSIONS.contains(&"pptx"),
            "pptx must not be in BINARY_EXTENSIONS — it has dedicated handling"
        );
        assert!(
            !is_binary("pptx", &[]),
            "pptx extension alone should not be binary"
        );
    }

    #[test]
    fn allows_text_files() {
        assert!(!is_binary("txt", b"Hello, world!\n"));
        assert!(!is_binary("rs", b"fn main() {}\n"));
        assert!(!is_binary("py", b"print('hello')\n"));
    }

    #[test]
    fn empty_content_is_not_binary() {
        assert!(!is_binary("", &[]));
        assert!(!is_binary("txt", &[]));
    }

    #[test]
    fn detects_null_bytes() {
        assert!(is_binary("", &[0x48, 0x65, 0x00, 0x6C]));
    }

    #[test]
    fn null_byte_at_sample_boundary() {
        let mut data = vec![b'A'; SAMPLE_SIZE - 1];
        data.push(0x00);
        assert!(is_binary("", &data));
    }

    #[test]
    fn null_byte_beyond_sample_not_detected() {
        let mut data = vec![b'A'; SAMPLE_SIZE];
        data.push(0x00);
        assert!(!is_binary("", &data));
    }

    #[test]
    fn threshold_boundary_not_binary() {
        // Exactly 30% non-printable → ratio = 0.30, NOT > 0.3 → not binary.
        let mut data: Vec<u8> = vec![0x01; 30];
        data.extend(vec![b'A'; 70]);
        assert_eq!(data.len(), 100);
        assert!(
            !is_binary("", &data),
            "30/100 = 0.30 should NOT be binary (threshold is >0.3)"
        );
    }

    #[test]
    fn threshold_boundary_is_binary() {
        // 31% non-printable → ratio = 0.31, > 0.3 → binary.
        let mut data: Vec<u8> = vec![0x01; 31];
        data.extend(vec![b'A'; 69]);
        assert_eq!(data.len(), 100);
        assert!(
            is_binary("", &data),
            "31/100 = 0.31 should be binary (threshold is >0.3)"
        );
    }

    #[test]
    fn tabs_and_newlines_are_printable() {
        // Bytes 9 (tab), 10 (LF), 13 (CR) should NOT count as non-printable.
        let data = b"\t\n\r\t\n\rHello World";
        assert!(!is_binary("", data));
    }

    #[test]
    fn large_text_file_is_not_binary() {
        let data = "fn main() {\n    println!(\"hello\");\n}\n"
            .repeat(500)
            .into_bytes();
        assert!(!is_binary("", &data));
    }

    #[test]
    fn extensions_are_sorted() {
        let mut sorted = BINARY_EXTENSIONS.to_vec();
        sorted.sort();
        assert_eq!(
            BINARY_EXTENSIONS,
            &sorted[..],
            "BINARY_EXTENSIONS should be sorted alphabetically"
        );
    }
}
