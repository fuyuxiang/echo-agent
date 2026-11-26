//! Low-level rendering utilities.
//!
//! Generic rendering primitives used by the scrollback and viewport.
pub mod bidi;
pub mod color;
pub mod draw;
pub mod gboom_overlay;
pub mod highlight;
pub mod image_overlay;
pub mod line_utils;
pub mod osc8;
pub mod preview_overlay;
pub mod renderable;
pub mod scrollbar;
pub mod terminal_output;
pub mod tool_paths;
pub mod video_overlay;
pub mod wrapping;
pub use image_overlay::render_image_overlay;
pub use preview_overlay::{PreviewConfig, PreviewStyle, render_preview_overlay};
pub mod safe_buf;
pub use renderable::Renderable;
pub use safe_buf::SafeBuf;
