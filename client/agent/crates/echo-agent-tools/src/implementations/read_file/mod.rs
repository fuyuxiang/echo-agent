//! Shared image/PDF/metadata helpers for read tools (echo_agent_build, etc.).

pub mod document;
pub mod image;
pub mod metadata;
pub(crate) mod office;
pub mod pdf;
pub mod pptx;

pub use metadata::{FileMetadata, bytes_to_metadata};
pub use pdf::{PDF_MAX_PAGES_PER_READ, parse_page_range};

pub use image::{CompressImageError, compress_image_for_conversation, image_read_output};
pub use pdf::{
    MAX_PDF_BYTES, PDF_PROCESS_TIMEOUT, extract_pdf_plain_text_cursor, is_pdf_file, make_test_pdf,
    run_document_extraction,
};

// Internal helpers still used only inside this crate.
pub(crate) use pdf::handle_pdf;
