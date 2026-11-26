mod common;
mod resize;
mod scrollback;
mod segment;
mod terminal;

#[cfg(test)]
mod tests;

pub use self::{
    common::{TerminalLike, with_synchronized_output},
    resize::{resize_purge_rerender, resize_viewport_height},
    scrollback::emit_to_scrollback,
    segment::split_into_line_segments,
    terminal::{LinkSpan, Terminal},
};
