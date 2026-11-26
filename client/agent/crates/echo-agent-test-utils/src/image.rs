//! Synthetic image fixtures shared across crates' test suites.

/// Wrap a PNG into a minimal single-frame ICO. `width`/`height` are the
/// ICONDIRENTRY bytes (`0` means 256); the PNG carries the real dimensions.
pub fn ico_with_png_frame(png: &[u8], width: u8, height: u8) -> Vec<u8> {
    let mut buf = Vec::with_capacity(22 + png.len());
    buf.extend_from_slice(&[0, 0, 1, 0, 1, 0]); // ICONDIR
    buf.extend_from_slice(&[width, height, 0, 0, 1, 0, 32, 0]); // ICONDIRENTRY
    buf.extend_from_slice(&(png.len() as u32).to_le_bytes()); // bytes in resource
    buf.extend_from_slice(&22u32.to_le_bytes()); // offset to the PNG payload
    buf.extend_from_slice(png);
    buf
}
