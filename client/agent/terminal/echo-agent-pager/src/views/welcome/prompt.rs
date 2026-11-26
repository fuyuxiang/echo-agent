//! Prompt component — renders the welcome screen prompt using PromptWidget.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use crate::views::prompt_widget::{PromptInfo, PromptStyle, PromptWidget};

use super::WelcomePromptFocus;

pub fn prompt_inset(compact: bool) -> u16 {
    if compact { 0 } else { 2 }
}

/// Render the welcome prompt using the shared PromptWidget.
/// Returns the cursor position and ownership-bearing post-flush output.
#[allow(clippy::too_many_arguments)]
pub fn render_prompt(
    area: Rect,
    buf: &mut Buffer,
    focus: WelcomePromptFocus,
    prompt: &mut PromptWidget,
    info: &PromptInfo<'_>,
    pad_left: u16,
    pad_right: u16,
    compact: bool,
) -> (
    Option<(u16, u16)>,
    Option<crate::terminal::overlay::PostFlush>,
) {
    let focused = focus == WelcomePromptFocus::Focused;
    let style = PromptStyle {
        focused,
        show_prefix: true,
        vpad_top: 1,
        compact,
        chrome: true,
        chrome_pad_left: pad_left,
        chrome_pad_right: pad_right,
        placeholder_override: Some("Type a message..."),
        ..PromptStyle::default()
    };

    // Inset the prompt area so the selection box border sits over dark background.
    // In compact mode, no inset (prompt_inset returns 0) to match session layout.
    let inset = prompt_inset(compact);
    let inset_area = Rect {
        x: area.x + inset,
        y: area.y,
        width: area.width.saturating_sub(inset * 2),
        height: area.height,
    };

    let result = prompt.draw(buf, inset_area, None, &style, Some(info), None);

    (result.cursor_pos, result.post_flush_escapes.map(Into::into))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::image::{GraphicsProtocol, set_protocol_for_test};
    use crossterm::Command;

    fn png() -> [u8; 8] {
        [0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n']
    }

    #[test]
    fn prompt_post_flush_keeps_ownership_when_plain_bytes_are_appended() {
        let _guard = set_protocol_for_test(GraphicsProtocol::Kitty);
        crate::terminal::overlay::reset_owner();
        let _ = crate::terminal::overlay::static_image(&png(), 20, 10, 0, 0, 71)
            .unwrap()
            .commit();
        let area = Rect::new(0, 0, 80, 3);
        let mut buf = Buffer::empty(area);
        let mut prompt = PromptWidget::new();
        let info = PromptInfo {
            model_name: "test",
            flags: &[],
            multiline: false,
            usage_warning: None,
            usage_warning_critical: false,
        };

        let (_, post_flush) = render_prompt(
            area,
            &mut buf,
            WelcomePromptFocus::Focused,
            &mut prompt,
            &info,
            2,
            2,
            false,
        );
        let mut post_flush = post_flush.expect("welcome clear");
        let mut cursor_bytes = String::new();
        let _ = crate::terminal::SetPointerCursor.write_ansi(&mut cursor_bytes);
        assert!(!cursor_bytes.is_empty());
        post_flush.append_plain(&cursor_bytes);
        assert!(post_flush.as_str().contains("a=d"));
        assert!(post_flush.as_str().ends_with(cursor_bytes.as_str()));
        assert!(
            !crate::terminal::overlay::static_image(&png(), 20, 10, 0, 0, 71)
                .unwrap()
                .as_str()
                .contains("a=t"),
            "constructing welcome output must not commit its clear"
        );

        let mut emitted = Vec::new();
        post_flush.write_to(&mut emitted).unwrap();
        assert!(
            crate::terminal::overlay::static_image(&png(), 20, 10, 0, 0, 71)
                .unwrap()
                .as_str()
                .contains("a=t"),
            "writing welcome output must commit its clear"
        );
    }
}
