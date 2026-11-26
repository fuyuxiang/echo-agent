//! Magic-byte metadata inspection shared by read tools.

/// Metadata extracted from file bytes via magic-byte inspection.
#[derive(Debug, Clone)]
pub struct FileMetadata {
    pub size: usize,
    pub mime_type: String,
}

impl FileMetadata {
    pub fn is_image(&self) -> bool {
        self.mime_type.starts_with("image/")
    }

    pub fn is_pdf(&self) -> bool {
        self.mime_type == "application/pdf"
    }
}

const PDF_MAGIC: &[u8; 5] = b"%PDF-";

pub(crate) fn is_pdf_magic(bytes: &[u8]) -> bool {
    bytes.len() >= 5 && bytes[..5] == *PDF_MAGIC
}

/// Infer file metadata (MIME type, extension) from raw bytes using magic-byte inspection.
pub fn bytes_to_metadata(
    file_bytes: &[u8],
) -> Result<FileMetadata, echo_agent_tool_runtime::ToolError> {
    let size = file_bytes.len();
    let data = infer::get(file_bytes).ok_or_else(|| {
        echo_agent_tool_runtime::ToolError::invalid_arguments(
            "failed to infer file type from magic bytes",
        )
    })?;

    Ok(FileMetadata {
        size,
        mime_type: data.mime_type().to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_pdf_magic_valid() {
        assert!(is_pdf_magic(b"%PDF-1.7 rest of file"));
        assert!(is_pdf_magic(b"%PDF-2.0"));
    }

    #[test]
    fn is_pdf_magic_invalid() {
        assert!(!is_pdf_magic(b"not a pdf"));
        assert!(!is_pdf_magic(b"%PD"));
        assert!(!is_pdf_magic(b""));
    }

    #[test]
    fn file_metadata_is_pdf() {
        let meta = FileMetadata {
            size: 100,
            mime_type: "application/pdf".to_string(),
        };
        assert!(meta.is_pdf());
        assert!(!meta.is_image());
    }

    #[test]
    fn file_metadata_image_is_not_pdf() {
        let meta = FileMetadata {
            size: 100,
            mime_type: "image/png".to_string(),
        };
        assert!(!meta.is_pdf());
        assert!(meta.is_image());
    }

    #[test]
    fn metadata_is_image_and_is_pdf_are_exclusive() {
        let pdf_meta = FileMetadata {
            size: 0,
            mime_type: "application/pdf".to_string(),
        };
        assert!(pdf_meta.is_pdf());
        assert!(!pdf_meta.is_image());

        let img_meta = FileMetadata {
            size: 0,
            mime_type: "image/jpeg".to_string(),
        };
        assert!(img_meta.is_image());
        assert!(!img_meta.is_pdf());
    }
}
