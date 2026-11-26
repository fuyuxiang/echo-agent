#![allow(clippy::new_without_default)]

pub mod editor;
pub mod render;
pub mod textarea;
pub mod wrapping;

pub use editor::{
    ApplyEditPlanError, EditBuffer, EditCommand, EditDelta, EditOutcome, EditPlan,
    PostEditCursorAffinity, SingleLineViewport, WordStyle, classify_key_event,
};
pub use textarea::{
    ClipboardProvider, ElementId, ElementKind, InternalClipboard, MouseAction, TextArea,
    TextAreaState, TextElement, TextElementEvent, TextElementEventKind, is_undo_input,
};

use crossterm::event::KeyModifiers;

// On Windows, AltGr arrives as Ctrl+Alt; on other platforms it's composed before reaching us.
#[cfg(target_os = "windows")]
#[inline]
pub fn is_altgr(modifiers: KeyModifiers) -> bool {
    let without_shift = modifiers & !KeyModifiers::SHIFT;
    without_shift == (KeyModifiers::CONTROL | KeyModifiers::ALT)
}

#[cfg(not(target_os = "windows"))]
#[inline]
pub fn is_altgr(_modifiers: KeyModifiers) -> bool {
    false
}
