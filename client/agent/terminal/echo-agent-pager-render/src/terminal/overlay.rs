use std::io::{self, Write};
use std::sync::atomic::{AtomicU64, Ordering};

use ratatui::layout::Rect;

use super::image::{
    GraphicsProtocol, KITTY_PLACEMENT_ID, build_overlay_image_escapes_for_protocol,
    clear_kitty_image, detect_graphics_protocol, fit_image_to_cells,
};

static NEXT_OWNER_ID: AtomicU64 = AtomicU64::new(1);

thread_local! {
    static OWNER: std::cell::Cell<Option<u64>> = const { std::cell::Cell::new(None) };
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Ownership {
    Static(u64),
    Clear,
}

#[derive(Debug)]
pub struct Escapes {
    bytes: String,
    ownership: Ownership,
}

impl Escapes {
    pub fn as_str(&self) -> &str {
        &self.bytes
    }

    pub fn into_string(self) -> String {
        self.bytes
    }

    pub fn commit(self) -> String {
        commit(self.ownership);
        self.bytes
    }
}

#[derive(Debug, Default)]
pub struct PostFlush {
    bytes: String,
    ownership: Option<Ownership>,
}

impl PostFlush {
    pub fn plain(bytes: String) -> Self {
        Self {
            bytes,
            ownership: None,
        }
    }

    pub fn append(&mut self, other: Self) {
        self.bytes.push_str(&other.bytes);
        if let Some(ownership) = other.ownership {
            self.ownership = Some(ownership);
        }
    }

    pub fn append_plain(&mut self, bytes: &str) {
        self.bytes.push_str(bytes);
    }

    pub fn as_str(&self) -> &str {
        &self.bytes
    }

    pub fn write_to(self, writer: &mut impl Write) -> io::Result<()> {
        writer.write_all(self.bytes.as_bytes())?;
        if let Some(ownership) = self.ownership {
            commit(ownership);
        }
        Ok(())
    }
}

impl From<Escapes> for PostFlush {
    fn from(escapes: Escapes) -> Self {
        Self {
            bytes: escapes.bytes,
            ownership: Some(escapes.ownership),
        }
    }
}

pub(crate) fn next_owner_id() -> u64 {
    NEXT_OWNER_ID.fetch_add(1, Ordering::Relaxed)
}

pub fn reset_owner() {
    OWNER.with(|owner| owner.set(None));
}

fn commit(ownership: Ownership) {
    OWNER.with(|owner| {
        owner.set(match ownership {
            Ownership::Static(id) => Some(id),
            Ownership::Clear => None,
        });
    });
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn static_image_for_protocol(
    protocol: GraphicsProtocol,
    image_data: &[u8],
    cols: u16,
    rows: u16,
    cell_x: u16,
    cell_y: u16,
    owner_id: u64,
) -> Option<Escapes> {
    let retransmit = OWNER.with(|owner| owner.get() != Some(owner_id));
    let bytes = build_overlay_image_escapes_for_protocol(
        protocol, image_data, cols, rows, cell_x, cell_y, retransmit,
    )?;
    Some(Escapes {
        bytes,
        ownership: Ownership::Static(owner_id),
    })
}

#[allow(clippy::too_many_arguments)]
pub fn static_image(
    image_data: &[u8],
    cols: u16,
    rows: u16,
    cell_x: u16,
    cell_y: u16,
    owner_id: u64,
) -> Option<Escapes> {
    static_image_for_protocol(
        detect_graphics_protocol(),
        image_data,
        cols,
        rows,
        cell_x,
        cell_y,
        owner_id,
    )
}

pub fn volatile_image(
    image_data: &[u8],
    cols: u16,
    rows: u16,
    cell_x: u16,
    cell_y: u16,
) -> Option<Escapes> {
    let bytes = build_overlay_image_escapes_for_protocol(
        detect_graphics_protocol(),
        image_data,
        cols,
        rows,
        cell_x,
        cell_y,
        true,
    )?;
    Some(Escapes {
        bytes,
        ownership: Ownership::Clear,
    })
}

pub fn static_centered(
    image_data: &[u8],
    img_w: u32,
    img_h: u32,
    overlay_rect: Rect,
    owner_id: u64,
) -> Option<Escapes> {
    let (cols, rows, x, y) = centered_placement(img_w, img_h, overlay_rect)?;
    static_image(image_data, cols, rows, x, y, owner_id)
}

pub fn volatile_centered(
    image_data: &[u8],
    img_w: u32,
    img_h: u32,
    overlay_rect: Rect,
) -> Option<Escapes> {
    let (cols, rows, x, y) = centered_placement(img_w, img_h, overlay_rect)?;
    volatile_image(image_data, cols, rows, x, y)
}

pub fn clear() -> Option<Escapes> {
    match detect_graphics_protocol() {
        GraphicsProtocol::Kitty => Some(clear_kitty()),
        GraphicsProtocol::ITerm2 | GraphicsProtocol::None => None,
    }
}

pub fn clear_kitty() -> Escapes {
    Escapes {
        bytes: clear_kitty_image(KITTY_PLACEMENT_ID),
        ownership: Ownership::Clear,
    }
}

fn centered_placement(img_w: u32, img_h: u32, overlay_rect: Rect) -> Option<(u16, u16, u16, u16)> {
    let max_cols = overlay_rect.width.saturating_sub(2);
    let max_rows = overlay_rect.height.saturating_sub(2);
    if max_cols < 4 || max_rows < 2 {
        return None;
    }
    let (cols, rows) = fit_image_to_cells(img_w, img_h, max_cols, max_rows);
    let x = overlay_rect.x + 1 + max_cols.saturating_sub(cols) / 2;
    let y = overlay_rect.y + 1 + max_rows.saturating_sub(rows) / 2;
    Some((cols, rows, x, y))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::image::set_protocol_for_test;

    fn png() -> [u8; 8] {
        [0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n']
    }

    #[test]
    fn static_owner_reuses_consecutive_frames_after_commit() {
        let _guard = set_protocol_for_test(GraphicsProtocol::Kitty);
        reset_owner();
        let first = static_image(&png(), 20, 10, 0, 0, 11).unwrap();
        assert!(first.as_str().contains("a=t"));
        let _ = first.commit();
        let second = static_image(&png(), 20, 10, 0, 0, 11).unwrap();
        assert!(!second.as_str().contains("a=t"));
    }

    #[test]
    fn discarded_clear_does_not_invalidate_static_owner() {
        let _guard = set_protocol_for_test(GraphicsProtocol::Kitty);
        reset_owner();
        let _ = static_image(&png(), 20, 10, 0, 0, 11).unwrap().commit();
        let _discarded = clear().unwrap();
        let next = static_image(&png(), 20, 10, 0, 0, 11).unwrap();
        assert!(!next.as_str().contains("a=t"));
    }

    #[test]
    fn discarded_static_escape_does_not_replace_owner() {
        let _guard = set_protocol_for_test(GraphicsProtocol::Kitty);
        reset_owner();
        let _ = static_image(&png(), 20, 10, 0, 0, 11).unwrap().commit();
        let _discarded = static_image(&png(), 20, 10, 0, 0, 12).unwrap();
        let next = static_image(&png(), 20, 10, 0, 0, 11).unwrap();
        assert!(!next.as_str().contains("a=t"));
    }

    #[test]
    fn failed_post_flush_write_does_not_commit_transition() {
        struct FailingWriter;

        impl std::io::Write for FailingWriter {
            fn write(&mut self, _buf: &[u8]) -> std::io::Result<usize> {
                Err(std::io::Error::other("injected write failure"))
            }

            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }

        let _guard = set_protocol_for_test(GraphicsProtocol::Kitty);
        reset_owner();
        let _ = static_image(&png(), 20, 10, 0, 0, 11).unwrap().commit();
        let clear = PostFlush::from(clear().unwrap());
        assert!(clear.write_to(&mut FailingWriter).is_err());
        let next = static_image(&png(), 20, 10, 0, 0, 11).unwrap();
        assert!(!next.as_str().contains("a=t"));
    }

    #[test]
    fn committed_clear_and_volatile_frame_invalidate_owner() {
        let _guard = set_protocol_for_test(GraphicsProtocol::Kitty);
        reset_owner();
        let _ = static_image(&png(), 20, 10, 0, 0, 11).unwrap().commit();
        let _ = clear().unwrap().commit();
        assert!(
            static_image(&png(), 20, 10, 0, 0, 11)
                .unwrap()
                .as_str()
                .contains("a=t")
        );
        let _ = static_image(&png(), 20, 10, 0, 0, 11).unwrap().commit();
        let _ = volatile_image(&png(), 20, 10, 0, 0).unwrap().commit();
        assert!(
            static_image(&png(), 20, 10, 0, 0, 11)
                .unwrap()
                .as_str()
                .contains("a=t")
        );
    }
}
