#![cfg_attr(rustfmt, rustfmt::skip)]
    use super::*;
    use crate::input::key::key;

    #[test]
    fn submit_via_try_send() {
        let mut pw = PromptWidget::new();
        pw.textarea.insert_str("hello world");
        assert_eq!(pw.try_send(), Some("hello world".into()));
    }

    #[test]
    fn try_send_on_empty_returns_none() {
        let mut pw = PromptWidget::new();
        assert_eq!(pw.try_send(), None);
    }

    #[test]
    fn try_send_on_whitespace_only_returns_none() {
        let mut pw = PromptWidget::new();
        pw.textarea.insert_str("   \n  ");
        assert_eq!(pw.try_send(), None);
    }

    #[test]
    fn shift_enter_inserts_newline() {
        let mut pw = PromptWidget::new();
        pw.textarea.insert_str("line1");
        assert_eq!(
            pw.handle_key(&key!(Enter, SHIFT).to_key_event()),
            PromptEvent::Edited
        );
        assert!(pw.textarea.text().contains('\n'));
    }

    #[test]
    fn alt_enter_inserts_newline() {
        let mut pw = PromptWidget::new();
        pw.textarea.insert_str("line1");
        assert_eq!(
            pw.handle_key(&key!(Enter, ALT).to_key_event()),
            PromptEvent::Edited
        );
        assert!(pw.textarea.text().contains('\n'));
    }

    #[test]
    fn stash_restore_preserves_text_and_cursor() {
        let mut pw = PromptWidget::new();
        pw.textarea.insert_str("hello world");
        pw.set_cursor(5);

        let stash = pw.stash();
        assert_eq!(stash.text, "hello world");
        assert_eq!(stash.cursor, 5);

        // Simulate overlay clearing the prompt.
        pw.set_text("");
        assert_eq!(pw.text(), "");

        // Restore should bring back both text and cursor position.
        pw.restore(stash);
        assert_eq!(pw.text(), "hello world");
        assert_eq!(pw.cursor(), 5);
    }

    #[test]
    fn clone_for_live_prefill_restores_without_stealing_session_images() {
        let mut pw = PromptWidget::new();
        pw.textarea.insert_str("inspect ");
        pw.insert_image(test_image()).unwrap();
        let session = pw.stash();
        assert!(pw.images.is_empty());

        pw.restore(session.clone_for_live_prefill());
        assert!(pw.text().contains("[Image #1]"));
        assert_eq!(pw.images.len(), 1);
        assert_eq!(session.images.len(), 1);
        drop(pw);
        assert_eq!(session.images.len(), 1);
    }

    #[test]
    fn stash_clear_restore_preserves_image_for_send() {
        let mut pw = PromptWidget::new();
        pw.textarea.insert_str("inspect ");
        pw.insert_image(test_image()).unwrap();
        let stashed_cursor = pw.cursor();

        let stash = pw.stash();
        assert!(
            pw.images.is_empty(),
            "stash must move image payload ownership"
        );
        pw.set_text("");
        pw.textarea.insert_str("temporary modal input");

        pw.restore(stash);
        assert_eq!(pw.text(), "inspect [Image #1] ");
        assert_eq!(pw.cursor(), stashed_cursor);
        assert_eq!(
            pw.textarea
                .elements()
                .iter()
                .filter(|e| e.kind == KIND_IMAGE)
                .count(),
            1,
            "restore must re-register the image chip"
        );

        let images = pw.drain_images();
        assert_eq!(images.len(), 1, "restored image must drain for submission");
        let (bytes, mime) =
            crate::prompt_images::load_for_send(&images[0]).expect("restored image loads");
        assert_eq!(bytes, vec![0u8; 16]);
        assert_eq!(mime, "image/png");
    }

    #[test]
    fn dropping_unrestored_stash_cleans_staged_image() {
        let dir = tempfile::tempdir().unwrap();
        let staged = dir.path().join("staged.png");
        std::fs::write(&staged, b"staged").unwrap();
        let mut image = test_image();
        image.staged_temp_path = Some(staged.clone());

        let mut pw = PromptWidget::new();
        pw.insert_image(image).unwrap();
        let stash = pw.stash();
        pw.set_text("");
        drop(stash);

        assert!(!staged.exists(), "dropped stash must release staged files");
    }

    #[test]
    fn ctrl_c_clears_text() {
        let mut pw = PromptWidget::new();
        pw.textarea.insert_str("some text");
        assert_eq!(
            pw.handle_key(&key!('c', CONTROL).to_key_event()),
            PromptEvent::Edited
        );
        assert!(pw.textarea.text().is_empty());
    }

    #[test]
    fn ctrl_c_on_empty_is_ignored() {
        let mut pw = PromptWidget::new();
        assert_eq!(
            pw.handle_key(&key!('c', CONTROL).to_key_event()),
            PromptEvent::Ignored
        );
    }

    #[test]
    fn char_input() {
        let mut pw = PromptWidget::new();
        assert_eq!(
            pw.handle_key(&key!('a').to_key_event()),
            PromptEvent::Edited
        );
        assert_eq!(pw.textarea.text(), "a");
    }

    #[test]
    fn backspace_edits() {
        let mut pw = PromptWidget::new();
        pw.textarea.insert_str("ab");
        assert_eq!(
            pw.handle_key(&key!(Backspace).to_key_event()),
            PromptEvent::Edited
        );
        assert_eq!(pw.textarea.text(), "a");
    }

    #[test]
    fn esc_is_ignored() {
        let mut pw = PromptWidget::new();
        assert_eq!(
            pw.handle_key(&key!(Esc).to_key_event()),
            PromptEvent::Ignored
        );
    }

    #[test]
    fn tab_is_ignored() {
        let mut pw = PromptWidget::new();
        assert_eq!(
            pw.handle_key(&key!(Tab).to_key_event()),
            PromptEvent::Ignored
        );
    }

    #[test]
    fn ctrl_w_deletes_word() {
        let mut pw = PromptWidget::new();
        pw.textarea.insert_str("hello world");
        assert_eq!(
            pw.handle_key(&key!('w', CONTROL).to_key_event()),
            PromptEvent::Edited
        );
        assert!(!pw.textarea.text().contains("world"));
    }

    #[test]
    fn ctrl_backspace_deletes_word() {
        let mut pw = PromptWidget::new();
        pw.textarea.insert_str("hello world");
        assert_eq!(
            pw.handle_key(&key!(Backspace, CONTROL).to_key_event()),
            PromptEvent::Edited
        );
        assert!(!pw.textarea.text().contains("world"));
    }

    #[test]
    fn cmd_backspace_and_ctrl_u_kill_to_beginning_keep_text_after_cursor() {
        // Kill cursor→BOL, not the whole line (terminals often send Cmd+Bsp as ^U).
        for key in [
            key!(Backspace, SUPER).to_key_event(),
            key!('u', CONTROL).to_key_event(),
        ] {
            let mut pw = PromptWidget::new();
            pw.textarea.insert_str("hello world");
            pw.textarea.set_cursor(5);
            assert_eq!(pw.handle_key(&key), PromptEvent::Edited);
            assert_eq!(pw.textarea.text(), " world");
            assert_eq!(pw.textarea.cursor(), 0);
        }
    }

    /// Cmd+A is gated to Ghostty in production, but every other test
    /// in this module runs in a generic test environment whose
    /// `terminal_context().brand` is `Unknown`. This helper builds a
    /// `PromptWidget` and force-enables the gate so the handler is
    /// exercised on every platform.
    fn ghostty_prompt() -> PromptWidget {
        let mut pw = PromptWidget::new();
        pw.cmd_a_select_all_enabled = true;
        pw
    }

    /// Gate predicate: only Ghostty enables `Cmd+A` select-all today.
    ///
    /// The `match` expression below is exhaustive over `TerminalName`,
    /// so adding a new variant to the enum will fail to compile here
    /// until someone decides whether the new terminal should opt in.
    /// That's the whole point — this gate decision is a per-brand
    /// policy choice and must not be silently inherited by future
    /// additions.
    #[test]
    fn cmd_a_supported_only_for_ghostty() {
        use crate::terminal::TerminalName;
        for brand in [
            TerminalName::AppleTerminal,
            TerminalName::Ghostty,
            TerminalName::Iterm2,
            TerminalName::WarpTerminal,
            TerminalName::VsCode,
            TerminalName::Cursor,
            TerminalName::Windsurf,
            TerminalName::Zed,
            TerminalName::WezTerm,
            TerminalName::Kitty,
            TerminalName::Alacritty,
            TerminalName::Rio,
            TerminalName::Foot,
            TerminalName::JetBrains,
            TerminalName::EchoAgentDesktop,
            TerminalName::Vte,
            TerminalName::Terminator,
            TerminalName::WindowsTerminal,
            TerminalName::Otty,
            TerminalName::Unknown,
        ] {
            // Compiler-enforced exhaustiveness: if a new TerminalName
            // variant is added without being listed above, this match
            // fails to compile and the gate decision is forced into
            // someone's hands.
            let expected = match brand {
                TerminalName::Ghostty => true,
                TerminalName::AppleTerminal
                | TerminalName::Iterm2
                | TerminalName::WarpTerminal
                | TerminalName::VsCode
                | TerminalName::Cursor
                | TerminalName::Windsurf
                | TerminalName::Zed
                | TerminalName::WezTerm
                | TerminalName::Kitty
                | TerminalName::Alacritty
                | TerminalName::Rio
                | TerminalName::Foot
                | TerminalName::JetBrains
                | TerminalName::EchoAgentDesktop
                | TerminalName::Vte
                | TerminalName::Terminator
                | TerminalName::WindowsTerminal
                | TerminalName::Otty
                | TerminalName::Unknown => false,
            };
            assert_eq!(
                cmd_a_select_all_supported(brand),
                expected,
                "gate verdict for {brand:?} does not match the policy"
            );
        }
    }

    #[test]
    fn cmd_a_selects_all_text() {
        let mut pw = ghostty_prompt();
        pw.textarea.insert_str("hello world");
        pw.textarea.set_cursor(3);

        assert_eq!(
            pw.handle_key(&key!('a', SUPER).to_key_event()),
            PromptEvent::Edited
        );

        let range = pw.textarea.selection_range().expect("selection set");
        assert_eq!(range.start, 0);
        assert_eq!(range.end, pw.textarea.text().len());
        assert_eq!(pw.textarea.cursor(), pw.textarea.text().len());
    }

    #[test]
    fn cmd_a_on_empty_is_ignored() {
        let mut pw = ghostty_prompt();
        assert_eq!(
            pw.handle_key(&key!('a', SUPER).to_key_event()),
            PromptEvent::Ignored
        );
        assert!(pw.textarea.selection_range().is_none());
    }

    #[test]
    fn cmd_a_then_typing_replaces_all_text() {
        let mut pw = ghostty_prompt();
        pw.textarea.insert_str("hello world");

        pw.handle_key(&key!('a', SUPER).to_key_event());
        assert!(pw.textarea.selection_range().is_some());

        // Typing while selection is active replaces it (textarea behaviour).
        pw.handle_key(&key!('x').to_key_event());
        assert_eq!(pw.textarea.text(), "x");
        assert!(pw.textarea.selection_range().is_none());
    }

    #[test]
    fn cmd_a_then_backspace_clears_text() {
        let mut pw = ghostty_prompt();
        pw.textarea.insert_str("hello world");

        pw.handle_key(&key!('a', SUPER).to_key_event());
        pw.handle_key(&key!(Backspace).to_key_event());

        assert!(pw.textarea.text().is_empty());
        assert!(pw.textarea.selection_range().is_none());
    }

    #[test]
    fn cmd_a_with_image_chip_selects_chip_text() {
        let mut pw = ghostty_prompt();
        pw.insert_image(test_image()).unwrap();
        // After insert_image: text = "[Image #1] ", one IMAGE element.
        pw.textarea.insert_str("describe this");
        let full = pw.textarea.text().to_owned();

        assert_eq!(
            pw.handle_key(&key!('a', SUPER).to_key_event()),
            PromptEvent::Edited
        );

        let range = pw.textarea.selection_range().expect("selection set");
        assert_eq!(range.start, 0);
        assert_eq!(range.end, full.len());
        // `selected_text` returns the buffer text, which includes the
        // `[Image #1]` chip placeholder. The path part is present when
        // `source_path` is set on the `PastedImage` — for `test_image()`
        // the source_path is `None`, so we just see `[Image #1]`.
        let selected = pw.textarea.selected_text().expect("selected text");
        assert!(selected.contains("[Image #1]"));
        assert!(selected.contains("describe this"));
    }

    #[test]
    fn cmd_a_then_backspace_clears_images_and_text() {
        let mut pw = ghostty_prompt();
        pw.insert_image(test_image()).unwrap();
        pw.insert_image(test_image()).unwrap();
        pw.textarea.insert_str("trailing words");
        assert_eq!(pw.images.len(), 2);

        pw.handle_key(&key!('a', SUPER).to_key_event());
        pw.handle_key(&key!(Backspace).to_key_event());

        // Buffer is emptied and the PastedImage records are reconciled
        // away by `sync_images_with_textarea` (called after the delete).
        assert!(pw.textarea.text().is_empty());
        assert!(
            pw.images.is_empty(),
            "deleting the full-buffer selection should drop image records too"
        );
    }

    /// Chip text is always path-free even when `source_path` is set —
    /// filepath lives on the PastedImage and in the preview overlay only.
    #[test]
    fn cmd_a_image_chip_selection_is_path_free() {
        use std::path::PathBuf;

        let mut img = test_image();
        img.source_path = Some(PathBuf::from("/tmp/echo-agent-test-image.png"));

        let mut pw = ghostty_prompt();
        pw.insert_image(img).unwrap();
        let full = pw.textarea.text().to_owned();
        assert!(
            full.contains("[Image #1]"),
            "chip text should be path-free: {full:?}"
        );
        assert!(
            !full.contains("/tmp/echo-agent-test-image.png"),
            "source path must not appear in the buffer chip: {full:?}"
        );
        assert_eq!(
            pw.images[0].source_path.as_deref(),
            Some(std::path::Path::new("/tmp/echo-agent-test-image.png")),
            "source_path retained on the PastedImage record"
        );

        pw.handle_key(&key!('a', SUPER).to_key_event());
        let selected = pw.textarea.selected_text().expect("selected text");
        assert!(selected.contains("[Image #1]"));
        assert!(
            !selected.contains("/tmp/echo-agent-test-image.png"),
            "select-all must not copy the filepath from the chip"
        );
    }

    /// When the gate is disabled (i.e. the user is not on Ghostty),
    /// `Cmd+A` must fall through untouched: no selection, no cursor
    /// movement, and the event is reported as `Ignored` (the textarea
    /// has no native binding for `SUPER + a`).
    #[test]
    fn cmd_a_is_noop_when_gate_is_disabled() {
        let mut pw = PromptWidget::new();
        pw.cmd_a_select_all_enabled = false; // simulate non-Ghostty
        pw.textarea.insert_str("hello world");
        let cursor_before = pw.textarea.cursor();

        let outcome = pw.handle_key(&key!('a', SUPER).to_key_event());

        assert_eq!(
            outcome,
            PromptEvent::Ignored,
            "with the gate off the handler must not claim the key"
        );
        assert!(
            pw.textarea.selection_range().is_none(),
            "no selection should be created"
        );
        assert_eq!(pw.textarea.cursor(), cursor_before, "cursor must not move");
        assert_eq!(pw.textarea.text(), "hello world");
    }

    #[test]
    fn paste_inserts_text() {
        let mut pw = PromptWidget::new();
        assert_eq!(pw.handle_paste("pasted content"), PromptEvent::Edited);
        assert_eq!(pw.textarea.text(), "pasted content");
    }

    #[test]
    fn paste_over_image_selection_reconciles_image_records() {
        let mut pw = PromptWidget::new();
        pw.insert_image(test_image()).unwrap();
        let len = pw.textarea.text().len();
        pw.textarea.set_selection(0, len);

        assert_eq!(pw.handle_paste("replacement"), PromptEvent::Edited);

        assert_eq!(pw.textarea.text(), "replacement");
        assert!(
            pw.textarea
                .elements()
                .iter()
                .all(|element| element.kind != KIND_IMAGE)
        );
        assert!(
            pw.images.is_empty(),
            "pasting over an image chip must remove its stored attachment"
        );
    }

    #[test]
    fn paste_empty_is_ignored() {
        let mut pw = PromptWidget::new();
        assert_eq!(pw.handle_paste(""), PromptEvent::Ignored);
    }

    #[test]
    fn desired_height_single_line() {
        let pw = PromptWidget::new();
        let style = PromptStyle {
            chrome: false,
            ..Default::default()
        };
        assert_eq!(pw.desired_height(80, &style, true, 20), 3); // top_divider(1)+text(1)+bot_divider(1)
    }

    #[test]
    fn desired_height_no_info() {
        let pw = PromptWidget::new();
        let style = PromptStyle {
            chrome: false,
            ..Default::default()
        };
        assert_eq!(pw.desired_height(80, &style, false, 20), 2); // vpad(1)+text(1)
    }

    #[test]
    fn desired_height_multiline() {
        let mut pw = PromptWidget::new();
        pw.textarea.insert_str("line1\nline2\nline3");
        let style = PromptStyle {
            chrome: false,
            ..Default::default()
        };
        assert_eq!(pw.desired_height(80, &style, true, 20), 5); // top_divider(1)+text(3)+bot_divider(1)
    }

    /// While history BROWSE mode is active the composer height is frozen at
    /// one text row: stepping onto a multi-line entry must not resize the
    /// box (the resize happens once the user edits and the browse detaches).
    #[test]
    fn desired_height_frozen_during_history_browse() {
        let mut pw = PromptWidget::new();
        let style = PromptStyle {
            chrome: false,
            ..Default::default()
        };
        assert!(pw.history_search.activate_browse(
            &[crate::views::history_search::HistoryEntry {
                text: "line1\nline2\nline3".into(),
            }],
            "",
        ));
        pw.set_text("line1\nline2\nline3"); // populated multi-line entry
        assert_eq!(
            pw.desired_height(80, &style, true, 20),
            3, // frozen: top_divider(1)+text(1)+bot_divider(1)
        );

        // Detach (deactivate) → the box resizes to fit the text.
        pw.history_search.deactivate();
        assert_eq!(pw.desired_height(80, &style, true, 20), 5);
    }

    #[test]
    fn desired_height_clamped_to_max() {
        let mut pw = PromptWidget::new();
        pw.textarea.insert_str(&"line\n".repeat(50));
        let style = PromptStyle {
            chrome: false,
            ..Default::default()
        };
        assert_eq!(pw.desired_height(80, &style, true, 10), 10);
    }

    #[test]
    fn desired_height_no_vpad() {
        let pw = PromptWidget::new();
        let style = PromptStyle {
            vpad_top: 0,
            chrome: false,
            ..Default::default()
        };
        assert_eq!(pw.desired_height(80, &style, true, 20), 2); // text(1)+bot_divider(1)
    }

    /// Regression test: inline prompt `desired_height` must use the narrower
    /// text width returned by each view's `inline_text_width` rather than
    /// the full `inner_width`. If the height is computed at the wider width,
    /// text that wraps in the actual draw area would be clipped because the
    /// panel doesn't grow enough.
    #[test]
    fn desired_height_inline_prompt_uses_render_width() {
        let mut pw = PromptWidget::new();
        // Insert text that fits on one line at width 80 but wraps at 69.
        let text: String = "a ".repeat(36); // 72 chars
        pw.textarea.insert_str(&text);

        let inline_style = PromptStyle::inline(ratatui::style::Color::Reset);
        let inner_width: u16 = 80;

        // Views with inline prompts should compute a narrower text width.
        let perm_w = crate::views::permission_view::inline_text_width(inner_width);
        let question_w = crate::views::question_view::inline_text_width(inner_width);

        for (label, render_width) in [("permission", perm_w), ("question", question_w)] {
            let h_wrong = pw.desired_height(inner_width, &inline_style, false, 15);
            let h_correct = pw.desired_height(render_width, &inline_style, false, 15);

            assert!(
                h_correct >= h_wrong,
                "{label}: height at render width ({render_width}) = {h_correct} \
                 must be >= height at inner_width ({inner_width}) = {h_wrong}"
            );
            assert_eq!(h_wrong, 1, "{label}: 72 chars should fit at width 80");
            assert_eq!(
                h_correct, 2,
                "{label}: 72 chars should wrap to 2 lines at width {render_width}"
            );
        }
    }

    #[test]
    fn ctrl_z_undoes() {
        let mut pw = PromptWidget::new();
        pw.handle_key(&key!('a').to_key_event());
        pw.handle_key(&key!('b').to_key_event());
        assert_eq!(pw.textarea.text(), "ab");
        assert_eq!(
            pw.handle_key(&key!('z', CONTROL).to_key_event()),
            PromptEvent::Edited
        );
        assert_ne!(pw.textarea.text(), "ab");
    }

    #[test]
    fn ctrl_r_redoes() {
        let mut pw = PromptWidget::new();
        pw.handle_key(&key!('a').to_key_event());
        pw.handle_key(&key!('b').to_key_event());
        pw.handle_key(&key!('z', CONTROL).to_key_event()); // undo
        let before = pw.textarea.text().to_string();
        assert_eq!(
            pw.handle_key(&key!('r', CONTROL).to_key_event()),
            PromptEvent::Edited
        );
        assert_ne!(pw.textarea.text(), before);
    }

    #[test]
    fn ctrl_shift_z_redoes() {
        let mut pw = PromptWidget::new();
        pw.handle_key(&key!('x').to_key_event());
        pw.handle_key(&key!('z', CONTROL).to_key_event()); // undo
        let before = pw.textarea.text().to_string();
        assert_eq!(
            pw.handle_key(&key!('z', CONTROL | SHIFT).to_key_event()),
            PromptEvent::Edited,
        );
        assert_ne!(pw.textarea.text(), before);
    }

    #[test]
    fn unknown_ctrl_key_is_ignored() {
        let mut pw = PromptWidget::new();
        assert_eq!(
            pw.handle_key(&key!('x', CONTROL).to_key_event()),
            PromptEvent::Ignored
        );
    }

    #[test]
    fn ctrl_j_inserts_newline() {
        let mut pw = PromptWidget::new();
        pw.textarea.insert_str("hello");
        assert_eq!(
            pw.handle_key(&key!('j', CONTROL).to_key_event()),
            PromptEvent::Edited
        );
        assert!(pw.textarea.text().contains('\n'));
    }

    #[test]
    fn ctrl_m_inserts_newline() {
        let mut pw = PromptWidget::new();
        pw.textarea.insert_str("hello");
        assert_eq!(
            pw.handle_key(&key!('m', CONTROL).to_key_event()),
            PromptEvent::Edited
        );
        assert!(pw.textarea.text().contains('\n'));
    }

    #[test]
    fn apply_backslash_continuation_trailing() {
        let mut pw = PromptWidget::new();
        pw.textarea.insert_str("hello\\");
        assert!(pw.apply_backslash_continuation());
        assert_eq!(pw.textarea.text(), "hello\n");
    }

    #[test]
    fn apply_backslash_continuation_no_backslash() {
        let mut pw = PromptWidget::new();
        pw.textarea.insert_str("hello");
        assert!(!pw.apply_backslash_continuation());
        assert_eq!(pw.textarea.text(), "hello");
    }

    #[test]
    fn apply_backslash_continuation_empty() {
        let mut pw = PromptWidget::new();
        assert!(!pw.apply_backslash_continuation());
    }

    #[test]
    fn backslash_continuation_via_try_send() {
        let mut pw = PromptWidget::new();
        pw.textarea.insert_str("hello\\");
        // try_send with trailing \ → continuation (insert newline), returns None
        assert_eq!(pw.try_send(), None);
        assert_eq!(pw.textarea.text(), "hello\n");
    }

    #[test]
    fn can_send_basic() {
        let mut pw = PromptWidget::new();
        assert!(!pw.can_send()); // empty
        pw.textarea.insert_str("hello");
        assert!(pw.can_send());
        pw.textarea.set_text("   ");
        assert!(!pw.can_send()); // whitespace only
    }

    #[test]
    fn can_send_backslash() {
        let mut pw = PromptWidget::new();
        pw.textarea.insert_str("hello\\");
        assert!(!pw.can_send()); // trailing backslash
    }

    // ── Paste element tests ──────────────────────────────────────────

    #[test]
    fn paste_single_line_inline() {
        let mut pw = PromptWidget::new();
        assert_eq!(pw.handle_paste("hello world"), PromptEvent::Edited);
        assert_eq!(pw.textarea.text(), "hello world");
        assert!(pw.textarea.elements().is_empty());
    }

    #[test]
    fn paste_single_line_trailing_newline_preserved() {
        // Trailing newlines are preserved so split paste batches keep
        // inter-line newlines (e.g. "hello\n" then "world").
        let mut pw = PromptWidget::new();
        assert_eq!(pw.handle_paste("hello world\n"), PromptEvent::Edited);
        assert_eq!(pw.textarea.text(), "hello world\n");
        assert!(pw.textarea.elements().is_empty());
    }

    #[test]
    fn paste_single_line_trailing_crlf_preserved() {
        let mut pw = PromptWidget::new();
        assert_eq!(pw.handle_paste("hello world\r\n"), PromptEvent::Edited);
        assert_eq!(pw.textarea.text(), "hello world\r\n");
        assert!(pw.textarea.elements().is_empty());
    }

    #[test]
    fn paste_bare_cr_multiline_creates_element() {
        // Some terminals send \r instead of \n in bracketed paste content.
        let mut pw = PromptWidget::new();
        assert_eq!(
            pw.handle_paste("line1\rline2\rline3\rline4"),
            PromptEvent::Edited
        );
        let normalized = "line1\nline2\nline3\nline4";
        assert_eq!(pw.textarea.text(), normalized);
        assert_eq!(pw.textarea.elements().len(), 1);
        assert_eq!(pw.textarea.elements()[0].kind, KIND_PASTE);
    }

    #[test]
    fn paste_multi_line_creates_element() {
        let mut pw = PromptWidget::new();
        let text = "line1\nline2\nline3\nline4";
        assert_eq!(pw.handle_paste(text), PromptEvent::Edited);
        assert_eq!(pw.textarea.text(), text);
        assert_eq!(pw.textarea.elements().len(), 1);
        assert_eq!(pw.textarea.elements()[0].kind, KIND_PASTE);
    }

    #[test]
    fn paste_large_single_line_creates_element() {
        // Single-line paste over the byte threshold still chips.
        let mut pw = PromptWidget::new();
        let text = "x".repeat(PASTE_CHIP_DISPLAY_BYTES + 1);
        assert_eq!(text.lines().count(), 1, "fixture must be a single line");
        assert_eq!(pw.handle_paste(&text), PromptEvent::Edited);
        assert_eq!(pw.textarea.elements().len(), 1);
        assert_eq!(pw.textarea.elements()[0].kind, KIND_PASTE);
    }

    #[test]
    fn paste_single_line_exactly_threshold_stays_inline() {
        // Exactly at the threshold stays inline (guards `>` vs `>=`).
        let mut pw = PromptWidget::new();
        let text = "x".repeat(PASTE_CHIP_DISPLAY_BYTES);
        assert_eq!(text.lines().count(), 1, "fixture must be a single line");
        assert_eq!(pw.handle_paste(&text), PromptEvent::Edited);
        assert!(
            pw.textarea.elements().is_empty(),
            "exactly-threshold single-line paste must stay inline"
        );
        assert_eq!(pw.textarea.text(), text);
    }

    #[test]
    fn paste_large_single_line_chip_shows_size_not_lines() {
        // A byte-triggered chip shows a size label, not a misleading "1 line".
        let mut pw = PromptWidget::new();
        let text = "x".repeat(12 * 1024); // 12 KB, single line
        assert_eq!(pw.handle_paste(&text), PromptEvent::Edited);
        let elems = pw.textarea.elements();
        assert_eq!(elems.len(), 1);
        assert_eq!(elems[0].kind, KIND_PASTE);
        let label: String = elems[0]
            .display
            .as_ref()
            .expect("chip has a display label")
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect();
        assert!(
            label.contains("KB"),
            "byte-triggered chip should show size (got {label:?})"
        );
        assert!(
            !label.contains("line"),
            "byte-triggered chip should not say lines (got {label:?})"
        );
    }

    #[test]
    fn paste_chip_size_label_matches_1000_based_threshold() {
        // Just over the 1000-based threshold must read "10 KB", not "9 KB".
        let line = paste_chip_display_bytes(PASTE_CHIP_DISPLAY_BYTES + 1);
        let label: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(label, "[Pasted: 10 KB]");
    }

    #[test]
    fn paste_large_multiline_chip_shows_size_not_lines() {
        // Regression: a large *multi-line* paste (e.g. 1 MB) was labeled
        // "[Pasted: N lines]" because the line-count path took precedence over
        // byte size. A large paste should read as its size regardless of how
        // many lines it has.
        let mut pw = PromptWidget::new();
        let text = "lorem ipsum dolor\n".repeat(2000); // ~36 KB across 2000 lines
        assert!(text.len() > PASTE_CHIP_DISPLAY_BYTES && text.lines().count() >= 4);
        assert_eq!(pw.handle_paste(&text), PromptEvent::Edited);
        let elems = pw.textarea.elements();
        assert_eq!(elems.len(), 1);
        let label: String = elems[0]
            .display
            .as_ref()
            .expect("chip has a display label")
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect();
        assert!(
            label.contains("KB") || label.contains("MB"),
            "large multi-line paste should show size (got {label:?})"
        );
        assert!(
            !label.contains("line"),
            "large multi-line paste should not show a line count (got {label:?})"
        );
    }

    #[test]
    fn paste_chip_size_label_formats_mb() {
        // >= 1 MB renders in MB (decimal), not "1000 KB".
        let line = paste_chip_display_bytes(1_000_000);
        let label: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(label, "[Pasted: 1.0 MB]");
    }

    #[test]
    fn paste_empty_string_is_ignored() {
        let mut pw = PromptWidget::new();
        assert_eq!(pw.handle_paste(""), PromptEvent::Ignored);
    }

    #[test]
    fn paste_element_at_cursor_returns_text() {
        let mut pw = PromptWidget::new();
        let text = "line1\nline2\nline3\nline4";
        pw.handle_paste(text);
        // Cursor should be at end of element after insert
        assert_eq!(pw.paste_element_at_cursor(), None);
        // Move cursor back into the element
        pw.textarea.set_cursor(0);
        assert_eq!(pw.paste_element_at_cursor(), Some(text));
    }

    #[test]
    fn paste_element_for_preview_shows_right_after_paste() {
        let mut pw = PromptWidget::new();
        let text = "line1\nline2\nline3\nline4";
        pw.handle_paste(text);
        // insert_element leaves the cursor one past the chip; the preview
        // must still show at the moment the chip is created.
        assert_eq!(pw.textarea.cursor(), pw.textarea.elements()[0].range.end);
        assert_eq!(pw.paste_element_for_preview(), Some(text));
    }

    #[test]
    fn paste_element_for_preview_dismissed_after_typing() {
        let mut pw = PromptWidget::new();
        pw.handle_paste("line1\nline2\nline3\nline4");
        pw.handle_key(&key!('x').to_key_event());
        // Cursor is no longer adjacent to the chip once text follows it.
        assert_eq!(pw.paste_element_for_preview(), None);
    }

    #[test]
    fn paste_element_for_preview_shows_on_chip() {
        let mut pw = PromptWidget::new();
        let text = "line1\nline2\nline3\nline4";
        pw.handle_paste(text);
        pw.textarea.set_cursor(0);
        assert_eq!(pw.paste_element_for_preview(), Some(text));
    }

    #[test]
    fn paste_element_for_preview_adjacent_chips_on_chip_wins() {
        let mut pw = PromptWidget::new();
        let first = "a1\na2\na3\na4";
        let second = "b1\nb2\nb3\nb4";
        pw.handle_paste(first);
        pw.handle_paste(second);
        let elems = pw.textarea.elements();
        assert_eq!(elems.len(), 2);
        assert_eq!(
            elems[0].range.end, elems[1].range.start,
            "chips must be adjacent"
        );
        let boundary = elems[0].range.end;
        // At the shared boundary the cursor sits ON the second chip, which
        // wins over the left-adjacent first chip.
        pw.textarea.set_cursor(boundary);
        assert_eq!(pw.paste_element_for_preview(), Some(second));
    }

    #[test]
    fn paste_element_for_preview_none_right_after_image_chip() {
        let mut pw = PromptWidget::new();
        pw.insert_image(test_image()).unwrap();
        let end = pw.textarea.elements()[0].range.end;
        // Right-adjacent fallback is gated on KIND_PASTE: an image chip
        // ending at the cursor must not trigger a paste preview.
        pw.textarea.set_cursor(end);
        assert_eq!(pw.paste_element_for_preview(), None);
    }

    // ── Image preview activation (paste-chip parity) ─────────────────

    #[test]
    fn image_for_preview_shows_right_after_insert() {
        let mut pw = PromptWidget::new();
        pw.insert_image(test_image()).unwrap();
        assert_eq!(
            pw.textarea.cursor(),
            pw.textarea.text().len(),
            "logical edit cursor must remain after the spacer"
        );
        assert!(
            pw.image_for_preview().is_some(),
            "just-inserted image must preview without moving the edit cursor"
        );
    }

    #[test]
    fn image_for_preview_dismissed_past_trailing_space() {
        let mut pw = PromptWidget::new();
        pw.insert_image(test_image()).unwrap();
        let end = pw.textarea.elements()[0].range.end;
        pw.set_cursor(end + 1);
        assert!(
            pw.image_for_preview().is_none(),
            "cursor after the trailing space must not open the preview"
        );
    }

    #[test]
    fn image_for_preview_shows_on_chip() {
        let mut pw = PromptWidget::new();
        pw.insert_image(test_image()).unwrap();
        pw.textarea.set_cursor(0);
        assert!(pw.image_for_preview().is_some());
        assert!(pw.image_at_cursor().is_some());
    }

    #[test]
    fn image_for_preview_dismissed_after_typing() {
        let mut pw = PromptWidget::new();
        pw.insert_image(test_image()).unwrap();
        pw.handle_key(&key!('x').to_key_event());
        assert_eq!(pw.text(), "[Image #1] x");
        assert!(
            pw.image_for_preview().is_none(),
            "typing after the chip must dismiss the preview"
        );
    }

    #[test]
    fn insert_image_then_backspace_removes_only_spacer() {
        let mut pw = PromptWidget::new();
        pw.insert_image(test_image()).unwrap();

        assert_eq!(
            pw.handle_key(&key!(Backspace).to_key_event()),
            PromptEvent::Edited
        );
        assert_eq!(pw.text(), "[Image #1]");
        assert_eq!(pw.images.len(), 1);
        assert_eq!(
            pw.textarea
                .elements()
                .iter()
                .filter(|e| e.kind == KIND_IMAGE)
                .count(),
            1,
            "Backspace at the real text end must not delete the atomic chip"
        );
    }

    #[test]
    fn repeated_image_inserts_keep_separator_and_preview_latest() {
        let mut pw = PromptWidget::new();
        pw.insert_image(test_image()).unwrap();
        pw.insert_image(test_image()).unwrap();

        assert_eq!(pw.text(), "[Image #1] [Image #2] ");
        assert_eq!(pw.cursor(), pw.text().len());
        assert_eq!(
            pw.image_for_preview().map(|image| image.display_number),
            Some(2)
        );
        let elements: Vec<_> = pw
            .textarea
            .elements()
            .iter()
            .filter(|e| e.kind == KIND_IMAGE)
            .collect();
        assert_eq!(elements.len(), 2);
        assert_eq!(
            elements[0].range.end + 1,
            elements[1].range.start,
            "one editable spacer must separate repeated image chips"
        );
    }

    #[test]
    fn image_preview_uses_cursor_or_hover_after_post_insert_dismissal() {
        let mut pw = PromptWidget::new();
        pw.insert_image(test_image()).unwrap();
        let image_id = pw.images[0].element_id;
        pw.handle_key(&key!('x').to_key_event());
        assert!(pw.image_for_preview().is_none());

        pw.set_cursor(0);
        assert_eq!(
            pw.image_for_preview().map(|image| image.element_id),
            Some(image_id)
        );

        pw.set_cursor(pw.text().len());
        pw.hovered_image_element_id = Some(image_id);
        assert_eq!(
            pw.image_for_preview().map(|image| image.element_id),
            Some(image_id)
        );
        pw.hovered_image_element_id = None;
        assert!(pw.image_for_preview().is_none());
    }

    #[test]
    fn alternating_prompt_widgets_retransmit_shared_kitty_placement() {
        let _guard = crate::terminal::image::set_protocol_for_test(
            crate::terminal::image::GraphicsProtocol::Kitty,
        );
        crate::terminal::overlay::reset_owner();
        let ready_image = || {
            let mut image = test_image();
            image.preview = crate::prompt_images::PromptImagePreview::ready_for_test(
                vec![0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n'],
                (100, 50),
            );
            image
        };
        let mut first = PromptWidget::new();
        let mut second = PromptWidget::new();
        first.insert_image(ready_image()).unwrap();
        second.insert_image(ready_image()).unwrap();
        assert_eq!(first.images[0].element_id, second.images[0].element_id);

        let area = Rect::new(0, 20, 60, 3);
        let overlay = Rect::new(0, 0, 60, 20);
        let mut buf = Buffer::empty(Rect::new(0, 0, 60, 23));
        let style = PromptStyle {
            focused: true,
            ..PromptStyle::default()
        };
        let first_escape = first
            .draw(&mut buf, area, Some(overlay), &style, None, None)
            .post_flush_escapes
            .expect("first preview");
        assert!(first_escape.as_str().contains("a=t"));
        let _ = first_escape.commit();
        let second_escape = second
            .draw(&mut buf, area, Some(overlay), &style, None, None)
            .post_flush_escapes
            .expect("second preview");
        assert!(second_escape.as_str().contains("a=t"));
        let _ = second_escape.commit();
        let first_again = first
            .draw(&mut buf, area, Some(overlay), &style, None, None)
            .post_flush_escapes
            .expect("first preview again");
        assert!(first_again.as_str().contains("a=t"));
    }

    #[test]
    fn image_for_preview_retains_source_path_on_record() {
        use std::path::PathBuf;
        let mut img = test_image();
        img.source_path = Some(PathBuf::from("/tmp/preview-path.png"));
        let mut pw = PromptWidget::new();
        pw.insert_image(img).unwrap();
        let preview = pw.image_for_preview().expect("preview after insert");
        assert_eq!(
            preview.source_path.as_deref(),
            Some(std::path::Path::new("/tmp/preview-path.png"))
        );
        assert!(
            !pw.textarea.text().contains("preview-path.png"),
            "path stays off the chip text"
        );
    }

    #[test]
    fn paste_element_for_preview_image_chip_at_boundary_suppresses() {
        let mut pw = PromptWidget::new();
        pw.handle_paste("line1\nline2\nline3\nline4");
        pw.insert_image(test_image()).unwrap();
        let elems = pw.textarea.elements();
        assert_eq!(elems.len(), 2);
        assert_eq!(elems[0].kind, KIND_PASTE);
        assert_eq!(elems[1].kind, KIND_IMAGE);
        assert_eq!(
            elems[0].range.end, elems[1].range.start,
            "image chip must start at the paste chip's end"
        );
        let boundary = elems[0].range.end;
        // On-chip match of any kind wins: at the boundary the cursor sits ON
        // the image chip, so no paste preview paints under the image preview.
        pw.textarea.set_cursor(boundary);
        assert_eq!(pw.paste_element_for_preview(), None);
        assert!(pw.image_at_cursor().is_some(), "image owns the overlay");
    }

    #[test]
    fn paste_preview_hint_on_chip_mentions_enter() {
        let mut pw = PromptWidget::new();
        pw.handle_paste("line1\nline2\nline3\nline4");
        pw.textarea.set_cursor(0);
        let hint: String = pw
            .paste_preview_hint(&Theme::current())
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect();
        assert!(hint.contains("enter"), "{hint}");
        assert!(hint.contains("expand"), "{hint}");
    }

    #[test]
    fn paste_preview_hint_right_adjacent_never_mentions_enter() {
        // At the post-paste position Enter submits, so the hint must not
        // advertise it — the honest affordance there is pasting again.
        let mut pw = PromptWidget::new();
        pw.handle_paste("line1\nline2\nline3\nline4");
        let hint: String = pw
            .paste_preview_hint(&Theme::current())
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect();
        assert!(hint.contains("paste again"), "{hint}");
        assert!(!hint.contains("enter"), "{hint}");
    }

    #[test]
    fn paste_element_enter_inlines() {
        let mut pw = PromptWidget::new();
        let text = "line1\nline2\nline3\nline4";
        pw.handle_paste(text);
        assert_eq!(pw.textarea.elements().len(), 1);

        // Move cursor onto the element
        pw.textarea.set_cursor(0);
        // Enter should inline it
        assert_eq!(
            pw.try_element_interaction(&key!(Enter).to_key_event()),
            Some(ElementInteraction::Inlined)
        );
        assert!(pw.textarea.elements().is_empty());
        // Text is still there
        assert_eq!(pw.textarea.text(), text);
    }

    #[test]
    fn paste_element_non_enter_does_not_inline() {
        let mut pw = PromptWidget::new();
        pw.handle_paste("line1\nline2\nline3\nline4");
        pw.textarea.set_cursor(0);
        // 'a' should not inline
        assert_eq!(pw.try_element_interaction(&key!('a').to_key_event()), None);
        assert_eq!(pw.textarea.elements().len(), 1);
    }

    #[test]
    fn expand_paste_element_at_cursor_inlines_chip() {
        let mut pw = PromptWidget::new();
        let text = "line1\nline2\nline3\nline4";
        pw.handle_paste(text);
        pw.textarea.set_cursor(0);
        assert!(pw.expand_paste_element_at_cursor());
        assert!(pw.textarea.elements().is_empty());
        assert_eq!(pw.textarea.text(), text);
    }

    #[test]
    fn expand_paste_element_at_cursor_requires_on_chip() {
        let mut pw = PromptWidget::new();
        pw.handle_paste("line1\nline2\nline3\nline4");
        // Cursor sits right after the chip. Unlike the display-only
        // preview, adjacency must NOT expand.
        assert!(!pw.expand_paste_element_at_cursor());
        assert_eq!(pw.textarea.elements().len(), 1);
    }

    #[test]
    fn expand_paste_element_at_cursor_ignores_image_chip() {
        let mut pw = PromptWidget::new();
        pw.insert_image(test_image()).unwrap();
        pw.textarea.set_cursor(0);
        assert!(!pw.expand_paste_element_at_cursor());
        assert_eq!(pw.textarea.elements().len(), 1);
    }

    #[test]
    fn repaste_identical_expands_chip() {
        let mut pw = PromptWidget::new();
        let text = "line1\nline2\nline3\nline4";
        pw.handle_paste(text);
        assert_eq!(pw.textarea.elements().len(), 1);
        assert_eq!(pw.handle_paste(text), PromptEvent::Edited);
        assert!(pw.textarea.elements().is_empty());
        // Exactly one copy of the content, now plain editable text.
        assert_eq!(pw.textarea.text(), text);
    }

    #[test]
    fn repaste_expand_is_one_undo_step() {
        let mut pw = PromptWidget::new();
        let text = "line1\nline2\nline3\nline4";
        pw.handle_paste(text);
        pw.handle_paste(text);
        assert!(pw.textarea.elements().is_empty());
        assert!(pw.textarea.undo());
        assert_eq!(pw.textarea.elements().len(), 1);
        assert_eq!(pw.textarea.elements()[0].kind, KIND_PASTE);
        assert_eq!(pw.textarea.text(), text);
    }

    #[test]
    fn repaste_different_content_adds_second_chip() {
        let mut pw = PromptWidget::new();
        pw.handle_paste("a1\na2\na3\na4");
        pw.handle_paste("b1\nb2\nb3\nb4");
        assert_eq!(pw.textarea.elements().len(), 2);
    }

    #[test]
    fn repaste_identical_away_from_chip_adds_second_chip() {
        let mut pw = PromptWidget::new();
        let text = "line1\nline2\nline3\nline4";
        pw.handle_paste(text);
        // Typing after the chip moves the cursor off / away from it.
        pw.textarea.insert_str(" tail");
        pw.handle_paste(text);
        assert_eq!(pw.textarea.elements().len(), 2);
    }

    #[test]
    fn repaste_with_tabs_expands_chip() {
        // insert_element expands tabs in the buffer; the repaste comparison
        // must canonicalize the incoming text the same way.
        let mut pw = PromptWidget::new();
        let text = "a\tb\nc\nd\ne";
        pw.handle_paste(text);
        assert_eq!(pw.textarea.elements().len(), 1);
        pw.handle_paste(text);
        assert!(pw.textarea.elements().is_empty());
        assert_eq!(pw.textarea.text(), "a    b\nc\nd\ne");
    }

    #[test]
    fn repaste_with_bare_cr_expands_chip() {
        // normalize_cr is an identity on \r\n; bare \r is its non-identity
        // case — the chip stores the \n form, so the repaste comparison
        // must normalize the incoming bytes before comparing.
        let mut pw = PromptWidget::new();
        let text = "line1\rline2\rline3\rline4";
        pw.handle_paste(text);
        assert_eq!(pw.textarea.elements().len(), 1);
        pw.handle_paste(text);
        assert!(pw.textarea.elements().is_empty());
        assert_eq!(pw.textarea.text(), "line1\nline2\nline3\nline4");
    }

    #[test]
    fn repaste_below_threshold_inserts_twice_inline() {
        let mut pw = PromptWidget::new();
        pw.handle_paste("ab\ncd");
        pw.handle_paste("ab\ncd");
        assert!(pw.textarea.elements().is_empty());
        assert_eq!(pw.textarea.text(), "ab\ncdab\ncd");
    }

    #[test]
    fn can_send_with_paste_element() {
        let mut pw = PromptWidget::new();
        pw.handle_paste("line1\nline2\nline3\nline4");
        assert!(pw.can_send());
    }

    #[test]
    fn try_send_with_paste_element() {
        let mut pw = PromptWidget::new();
        let text = "line1\nline2\nline3\nline4";
        pw.handle_paste(text);
        let sent = pw.try_send();
        assert_eq!(sent, Some(text.to_string()));
    }

    #[test]
    fn paste_then_type_then_send() {
        let mut pw = PromptWidget::new();
        pw.handle_paste("pasted\nstuff");
        pw.textarea.insert_str(" and more");
        let sent = pw.try_send().unwrap();
        assert!(sent.contains("pasted\nstuff"));
        assert!(sent.contains("and more"));
    }

    #[test]
    fn slash_ranges_map_past_paste_element() {
        use crate::acp::model_state::ModelState;

        let mut pw = PromptWidget::new();
        pw.handle_paste("line1\nline2\nline3\nline4");
        let elem_end = pw.textarea.elements()[0].range.end;
        pw.textarea.insert_str("\n/impl");
        pw.textarea.set_cursor(pw.textarea.text().len());

        let models = ModelState::default();
        pw.refresh_slash(&models);
        let snap = pw.slash_state.snapshot();
        let raw = pw.textarea.text();

        let token_range = snap
            .command_range
            .or_else(|| snap.inline_ghost.as_ref().map(|g| g.token_range.clone()))
            .expect("expected slash state for /impl after paste chip");
        assert!(
            token_range.start >= elem_end,
            "slash range must not point inside the paste element (would replace the pill on Tab)"
        );
        assert_eq!(&raw[token_range], "/impl");
    }

    #[test]
    fn map_clean_offset_skips_paste_element_body() {
        let mut pw = PromptWidget::new();
        pw.handle_paste("aaa\nbbb\nccc\nddd");
        let elem_end = pw.textarea.elements()[0].range.end;
        pw.textarea.insert_str(" /x");
        let raw = pw.textarea.text().to_string();
        let slash_clean_start = strip_all_elements(&raw, raw.len(), &pw.textarea)
            .0
            .find('/')
            .expect("clean text has /x");
        let mapped = map_clean_offset_to_raw(&raw, slash_clean_start, pw.textarea.elements());
        assert!(
            mapped >= elem_end,
            "mapped offset {mapped} must be at or after paste element end {elem_end}"
        );
        assert_eq!(&raw[mapped..mapped + 2], "/x");
    }

    // -- PromptStyle prefix_override tests --

    #[test]
    fn prompt_style_default_has_no_prefix_override() {
        let style = PromptStyle::default();
        assert!(style.prefix_override.is_none());
    }

    #[test]
    fn prompt_style_overlay_has_no_prefix_override() {
        let style = PromptStyle::overlay();
        assert!(style.prefix_override.is_none());
    }

    // ── Voice interim wrapping ───────────────────────────────────────

    #[test]
    fn wrap_voice_interim_wraps_on_word_boundaries() {
        let lines = wrap_voice_interim("the quick brown fox", 10, 5);
        assert_eq!(lines, vec!["the quick", "brown fox"]);
    }

    #[test]
    fn wrap_voice_interim_truncates_with_ellipsis_when_overflowing_rows() {
        let lines = wrap_voice_interim("one two three four five six", 5, 2);
        assert_eq!(lines.len(), 2);
        assert!(
            lines.last().unwrap().ends_with('\u{2026}'),
            "last visible line should be ellipsized: {lines:?}"
        );
    }

    #[test]
    fn wrap_voice_interim_handles_zero_bounds() {
        assert!(wrap_voice_interim("hello", 0, 3).is_empty());
        assert!(wrap_voice_interim("hello", 10, 0).is_empty());
    }

    // ── Slash state integration tests ───────────────────────────────

    #[test]
    fn refresh_slash_produces_snapshot_for_slash_input() {
        let mut pw = PromptWidget::new();
        let models = crate::acp::model_state::ModelState::default();

        pw.textarea.insert_str("/");
        pw.refresh_slash(&models);

        let snap = pw.slash_snapshot();
        assert!(snap.active, "slash should be active after typing /");
        assert!(snap.open, "dropdown should open with available commands");
        assert!(!snap.matches.is_empty());
    }

    #[test]
    fn refresh_slash_inactive_for_normal_text() {
        let mut pw = PromptWidget::new();
        let models = crate::acp::model_state::ModelState::default();

        pw.textarea.insert_str("hello world");
        pw.refresh_slash(&models);

        let snap = pw.slash_snapshot();
        assert!(!snap.active);
        assert!(!snap.open);
    }

    #[test]
    fn sync_acp_commands_updates_registry_and_refreshes() {
        let mut pw = PromptWidget::new();
        let models = crate::acp::model_state::ModelState::default();

        // Type a slash command prefix.
        pw.textarea.insert_str("/flu");
        pw.refresh_slash(&models);

        // Before sync: "flush" is not in builtins.
        let snap = pw.slash_snapshot();
        assert!(
            !snap.matches.iter().any(|r| r.display == "/flush"),
            "flush should not be in builtins"
        );

        // Sync ACP commands with "flush".
        let acp_cmds = vec![agent_client_protocol::AvailableCommand::new(
            "flush".to_string(),
            "Flush memory".to_string(),
        )];
        pw.sync_acp_commands(&acp_cmds, None, &models);

        // After sync: "flush" should appear in matches.
        let snap = pw.slash_snapshot();
        assert!(
            snap.matches.iter().any(|r| r.display == "/flush"),
            "flush should appear after ACP sync, got: {:?}",
            snap.matches
                .iter()
                .map(|r| r.display.as_str())
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn sync_acp_commands_passes_tools_to_registry() {
        // End-to-end: tracker advertises a toolset, sync forwards it,
        // and tool-gated commands like /loop disappear when their
        // tool isn't registered.
        let mut pw = PromptWidget::new();
        let models = crate::acp::model_state::ModelState::default();
        let mut empty_tools = std::collections::HashSet::new();
        empty_tools.insert("read_file".to_string());

        // Sync with a toolset that omits scheduler_create.
        pw.sync_acp_commands(&[], Some(&empty_tools), &models);
        // Now type /loop -- it should be filtered out of the dropdown.
        pw.textarea.insert_str("/loop");
        pw.refresh_slash(&models);
        let snap = pw.slash_snapshot();
        assert!(
            !snap.matches.iter().any(|r| r.display == "/loop"),
            "/loop should be hidden when scheduler_create is missing, got: {:?}",
            snap.matches
                .iter()
                .map(|r| r.display.as_str())
                .collect::<Vec<_>>()
        );

        // Add the tool back and resync -- /loop returns.
        empty_tools.insert("scheduler_create".to_string());
        pw.sync_acp_commands(&[], Some(&empty_tools), &models);
        pw.refresh_slash(&models);
        let snap = pw.slash_snapshot();
        assert!(
            snap.matches.iter().any(|r| r.display == "/loop"),
            "/loop should be visible once scheduler_create is advertised",
        );
    }

    // ── Slash completion acceptance tests ──────────────────────────

    #[test]
    fn accept_completion_inserts_alias_for_alias() {
        let mut pw = PromptWidget::new();
        let models = crate::acp::model_state::ModelState::default();

        // Type alias "/m" → should match the "/m" alias of "/model".
        pw.textarea.insert_str("/m");
        pw.refresh_slash(&models);

        let snap = pw.slash_snapshot();
        assert!(snap.open);
        assert_eq!(snap.selection().unwrap().display, "/m");

        // Accept → text should become "/m " (alias + trailing space since model takes args).
        pw.accept_slash_completion(&models);
        assert!(
            pw.textarea.text().starts_with("/m"),
            "should insert alias command, got: {:?}",
            pw.textarea.text()
        );
    }

    /// Accepting a completion rewrites the token; an unrelated highlight
    /// (Shift+arrows while the dropdown is open) must not survive the accept.
    #[test]
    fn accept_completion_drops_active_highlight() {
        let mut pw = PromptWidget::new();
        let models = crate::acp::model_state::ModelState::default();
        pw.textarea.insert_str("/mod");
        pw.refresh_slash(&models);
        pw.textarea.set_selection(1, 3);

        assert!(pw.accept_slash_completion(&models));
        assert_eq!(pw.textarea.text(), "/model ");
        assert!(pw.textarea.selection_range().is_none());
    }

    #[test]
    fn accept_completion_adds_trailing_space_for_arg_command() {
        let mut pw = PromptWidget::new();
        let models = crate::acp::model_state::ModelState::default();

        // Type "/mod" → should match "/model" which takes_args.
        pw.textarea.insert_str("/mod");
        pw.refresh_slash(&models);

        pw.accept_slash_completion(&models);
        let text = pw.textarea.text().to_string();
        assert_eq!(
            text, "/model ",
            "arg-taking command should get trailing space"
        );
    }

    #[test]
    fn accept_inside_command_with_args_absorbs_existing_separator() {
        let mut pw = PromptWidget::new();
        let models = crate::acp::model_state::ModelState::default();

        // Cursor inside the command token with args already present.
        pw.textarea.insert_str("/mod echo-agent-4");
        pw.textarea.set_cursor(3);
        pw.refresh_slash(&models);

        let snap = pw.slash_snapshot();
        assert!(snap.open, "menu opens with the cursor inside the command");
        let model_idx = snap
            .matches
            .iter()
            .position(|r| r.display == "/model")
            .expect("/model in matches");
        for _ in 0..model_idx {
            pw.slash_move_selection(1);
        }

        assert!(pw.accept_slash_completion(&models));
        assert_eq!(
            pw.textarea.text(),
            "/model echo-agent-4",
            "the row's trailing space must not stack on the existing separator"
        );
        // Absorb (not trim-the-insert): the cursor must land after the
        // separator so the post-accept refresh is in the args phase — the
        // Enter-chains flow. Trimming would leave it at the command end.
        assert_eq!(pw.textarea.cursor(), "/model ".len());
        assert!(!pw.slash_snapshot().cursor_in_command);
    }

    #[test]
    fn accept_never_absorbs_into_adjacent_paste_chip() {
        let mut pw = PromptWidget::new();
        let models = crate::acp::model_state::ModelState::default();

        // Snapshot taken while the composer is just the token…
        pw.textarea.insert_str("/mod");
        pw.refresh_slash(&models);
        let snap = pw.slash_snapshot();
        assert!(snap.open);
        let model_idx = snap
            .matches
            .iter()
            .position(|r| r.display == "/model")
            .expect("/model in matches");
        for _ in 0..model_idx {
            pw.slash_move_selection(1);
        }

        // …then indented code pastes as a chip flush against it, its first
        // byte a plain space (handle_paste does not refresh the snapshot).
        let pasted = " if foo:\n    bar\n    baz\n    qux";
        pw.handle_paste(pasted);
        assert_eq!(pw.textarea.elements().len(), 1);

        // Accepting must not absorb the chip's leading byte: replace_range
        // expands any element overlap to the whole chip, so an absorbed
        // chip byte would silently delete the entire paste.
        assert!(pw.accept_slash_completion(&models));
        let elements = pw.textarea.elements();
        assert_eq!(elements.len(), 1, "paste chip must survive the accept");
        assert_eq!(elements[0].kind, KIND_PASTE);
        assert_eq!(
            pw.textarea.element_text(elements[0].id),
            Some(pasted),
            "chip content must be untouched"
        );
        assert_eq!(pw.textarea.text(), format!("/model {pasted}"));
    }

    #[test]
    fn accept_completion_no_trailing_space_for_no_arg_command() {
        let mut pw = PromptWidget::new();
        let models = crate::acp::model_state::ModelState::default();

        // Type "/qu" → should match "/quit" which does NOT take args.
        pw.textarea.insert_str("/qu");
        pw.refresh_slash(&models);

        let snap = pw.slash_snapshot();
        assert!(snap.matches.iter().any(|r| r.display == "/quit"));

        // Select /quit (it may not be first if other commands match).
        // Find it and navigate to it.
        let quit_idx = snap
            .matches
            .iter()
            .position(|r| r.display == "/quit")
            .unwrap();
        for _ in 0..quit_idx {
            pw.slash_move_selection(1);
        }

        pw.accept_slash_completion(&models);
        let text = pw.textarea.text().to_string();
        assert_eq!(
            text, "/quit",
            "no-arg command should NOT get trailing space"
        );
    }

    #[test]
    fn accept_arg_completion_replaces_arg_range() {
        use std::sync::Arc;

        let mut pw = PromptWidget::new();
        let mut models = crate::acp::model_state::ModelState::default();
        let model_id = agent_client_protocol::ModelId::new(Arc::from("echo-agent-4.5"));
        models.available.insert(
            model_id.clone(),
            agent_client_protocol::ModelInfo::new(model_id, "EchoAgent 4.5".to_string()),
        );

        // Type "/model gr" and position cursor at end (in args).
        pw.textarea.insert_str("/model gr");
        pw.refresh_slash(&models);

        let snap = pw.slash_snapshot();
        assert!(snap.open, "arg suggestions should be open");
        assert!(snap.args_range.is_some());

        // Accept arg completion → should replace "gr" with "EchoAgent 4.5".
        pw.accept_slash_completion(&models);
        let text = pw.textarea.text().to_string();
        assert!(
            text.contains("EchoAgent 4.5"),
            "arg should be replaced, got: {:?}",
            text
        );
        assert!(text.starts_with("/model "));
    }

    #[test]
    fn compact_completable_with_no_args() {
        let mut pw = PromptWidget::new();
        let models = crate::acp::model_state::ModelState::default();

        // Type "/comp" → matches "/compact".
        pw.textarea.insert_str("/comp");
        pw.refresh_slash(&models);

        // Accept → text becomes "/compact " (trailing space since takes_args).
        pw.accept_slash_completion(&models);
        let text = pw.textarea.text().to_string();
        assert_eq!(text, "/compact ");

        // Even without filling args, try_send should succeed (args are optional).
        let sent = pw.try_send();
        assert!(
            sent.is_some(),
            "/compact with no args should be sendable (optional args)"
        );
    }

    #[test]
    fn sync_acp_commands_preserves_builtins() {
        let mut pw = PromptWidget::new();
        let models = crate::acp::model_state::ModelState::default();

        // Sync ACP commands (should not remove builtins).
        pw.sync_acp_commands(&[], None, &models);

        pw.textarea.insert_str("/qu");
        pw.refresh_slash(&models);

        let snap = pw.slash_snapshot();
        assert!(
            snap.matches.iter().any(|r| r.display == "/quit"),
            "builtin /quit should survive ACP sync"
        );
    }

    // ── Regression tests ────────────────────────────────────────────

    #[test]
    fn sync_acp_then_refresh_ordering() {
        // Regression: sync_acp_commands must update registry BEFORE
        // refreshing snapshot. If reversed, the snapshot would use
        // the old registry and miss the new commands.
        let mut pw = PromptWidget::new();
        let models = crate::acp::model_state::ModelState::default();

        // Start with text that would match a new ACP command.
        pw.textarea.insert_str("/ses");

        // Sync adds "session-info" — sync_acp_commands internally
        // calls refresh_slash, so the snapshot should already show it.
        let acp_cmds = vec![agent_client_protocol::AvailableCommand::new(
            "session-info".to_string(),
            "Show session info".to_string(),
        )];
        pw.sync_acp_commands(&acp_cmds, None, &models);

        let snap = pw.slash_snapshot();
        assert!(
            snap.matches.iter().any(|r| r.display == "/session-info"),
            "sync_acp_commands should refresh snapshot with new commands"
        );
    }

    #[test]
    fn repeated_sync_does_not_corrupt_registry() {
        // Regression: calling sync_acp_commands multiple times should
        // not accumulate duplicates or lose builtins.
        let mut pw = PromptWidget::new();
        let models = crate::acp::model_state::ModelState::default();

        let acp_cmds = vec![agent_client_protocol::AvailableCommand::new(
            "flush".to_string(),
            "Flush memory".to_string(),
        )];

        // Sync three times.
        pw.sync_acp_commands(&acp_cmds, None, &models);
        pw.sync_acp_commands(&acp_cmds, None, &models);
        pw.sync_acp_commands(&acp_cmds, None, &models);

        // Should have exactly all pager-local builtins + 1 ACP ("flush").
        // We compute the expected count from `builtin_commands()` so the
        // assertion stays accurate as the builtin set grows.
        let expected = crate::slash::commands::builtin_commands().len() + 1;
        let registry = &pw.slash_controller.registry();
        assert_eq!(registry.command_count(), expected);
        assert!(registry.get("quit").is_some());
        assert!(registry.get("flush").is_some());
    }

    #[test]
    fn empty_prompt_does_not_show_slash_dropdown() {
        // Regression: empty prompt should never show the slash dropdown.
        let mut pw = PromptWidget::new();
        let models = crate::acp::model_state::ModelState::default();

        pw.refresh_slash(&models);
        let snap = pw.slash_snapshot();
        assert!(!snap.active);
        assert!(!snap.open);
    }

    // ── CR normalization tests ────────────────────────────────────

    #[test]
    fn paste_bare_cr_becomes_lf() {
        let mut pw = PromptWidget::new();
        pw.handle_paste("a\rb\rc");
        assert_eq!(pw.textarea.text(), "a\nb\nc");
    }

    #[test]
    fn paste_crlf_preserved() {
        let mut pw = PromptWidget::new();
        pw.handle_paste("a\r\nb\r\nc");
        assert_eq!(pw.textarea.text(), "a\r\nb\r\nc");
    }

    #[test]
    fn paste_mixed_cr_and_crlf() {
        // \r\n stays, standalone \r becomes \n
        let mut pw = PromptWidget::new();
        pw.handle_paste("a\r\nb\rc");
        assert_eq!(pw.textarea.text(), "a\r\nb\nc");
    }

    #[test]
    fn paste_trailing_bare_cr() {
        // Bare \r becomes \n; trailing newline preserved.
        let mut pw = PromptWidget::new();
        pw.handle_paste("hello\r");
        assert_eq!(pw.textarea.text(), "hello\n");
    }

    #[test]
    fn paste_no_cr_unchanged() {
        let mut pw = PromptWidget::new();
        pw.handle_paste("no carriage returns\nhere");
        assert_eq!(pw.textarea.text(), "no carriage returns\nhere");
    }

    // ── Paste chip threshold boundary tests ───────────────────────

    #[test]
    fn paste_3_lines_inline_normal_mode() {
        let mut pw = PromptWidget::new();
        pw.handle_paste("a\nb\nc");
        assert_eq!(pw.textarea.text(), "a\nb\nc");
        assert!(pw.textarea.elements().is_empty());
    }

    #[test]
    fn paste_4_lines_creates_chip_normal_mode() {
        let mut pw = PromptWidget::new();
        pw.handle_paste("a\nb\nc\nd");
        assert_eq!(pw.textarea.text(), "a\nb\nc\nd");
        assert_eq!(pw.textarea.elements().len(), 1);
        assert_eq!(pw.textarea.elements()[0].kind, KIND_PASTE);
    }

    #[test]
    fn paste_1_line_inline_compact_mode() {
        let mut pw = PromptWidget::new();
        pw.set_compact(true);
        pw.handle_paste("single line");
        assert_eq!(pw.textarea.text(), "single line");
        assert!(pw.textarea.elements().is_empty());
    }

    #[test]
    fn paste_2_lines_creates_chip_compact_mode() {
        let mut pw = PromptWidget::new();
        pw.set_compact(true);
        pw.handle_paste("a\nb");
        assert_eq!(pw.textarea.text(), "a\nb");
        assert_eq!(pw.textarea.elements().len(), 1);
        assert_eq!(pw.textarea.elements()[0].kind, KIND_PASTE);
    }

    #[test]
    fn paste_inline_trailing_newline_preserved() {
        // Inline path (< 4 lines) preserves trailing newlines so
        // split batches on Windows keep inter-line newlines.
        let mut pw = PromptWidget::new();
        pw.handle_paste("a\nb\nc\n");
        assert_eq!(pw.textarea.text(), "a\nb\nc\n");
        assert!(pw.textarea.elements().is_empty());
    }

    // ── normalize_cr tests ─────────────────────────────────────────

    #[test]
    fn normalize_cr_bare_cr() {
        assert_eq!(normalize_cr("a\rb\rc"), "a\nb\nc");
    }

    #[test]
    fn normalize_cr_crlf_preserved() {
        assert_eq!(normalize_cr("a\r\nb\r\nc"), "a\r\nb\r\nc");
    }

    #[test]
    fn normalize_cr_mixed() {
        assert_eq!(normalize_cr("a\r\nb\rc"), "a\r\nb\nc");
    }

    #[test]
    fn normalize_cr_no_cr() {
        assert_eq!(normalize_cr("no cr\nhere"), "no cr\nhere");
    }

    // ── Inline paste (handle_paste without element) ──────────────

    #[test]
    fn inline_paste_multiline_no_element() {
        let mut pw = PromptWidget::new();
        let text = "line1\nline2\nline3";
        // Simulate Ctrl+Shift+V: insert_str directly, no element.
        let normalized = normalize_cr(text);
        pw.textarea.insert_str(&normalized);
        assert_eq!(pw.textarea.text(), text);
        assert!(pw.textarea.elements().is_empty());
    }

    #[test]
    fn slash_state_resets_when_text_cleared() {
        // Regression: after typing "/" and getting suggestions, clearing
        // the text should close the dropdown.
        let mut pw = PromptWidget::new();
        let models = crate::acp::model_state::ModelState::default();

        pw.textarea.insert_str("/");
        pw.refresh_slash(&models);
        assert!(pw.slash_snapshot().open);

        pw.set_text("");
        pw.refresh_slash(&models);
        let snap = pw.slash_snapshot();
        assert!(!snap.active, "cleared text should deactivate slash");
        assert!(!snap.open, "cleared text should close dropdown");
    }

    // ── Image chip tests ──────────────────────────────────────────

    /// Helper: create a minimal `PastedImage` for testing.
    fn test_image() -> PastedImage {
        PastedImage {
            element_id: echo_agent_ratatui_textarea::ElementId::from_raw(0), // overwritten by insert_image
            display_number: 0,
            mime_type: "image/png".into(),
            dimensions: Some((100, 80)),
            byte_len: 2048,
            encoded_bytes: Some(vec![0u8; 16].into()),
            source_path: None,
            staged_temp_path: None,
            session_image_path: None,
            preview: crate::prompt_images::PromptImagePreview::default(),
        }
    }

    #[test]
    fn insert_image_creates_chip() {
        let mut pw = PromptWidget::new();
        assert!(pw.insert_image(test_image()).is_ok());

        assert_eq!(pw.textarea.elements().len(), 1);
        let elem = &pw.textarea.elements()[0];
        assert_eq!(elem.kind, KIND_IMAGE);
        assert_eq!(pw.textarea.text(), "[Image #1] ");
        assert_eq!(pw.images.len(), 1);
        assert_eq!(pw.images[0].display_number, 1);
    }

    #[test]
    fn text_without_image_chips_strips_only_images() {
        let mut pw = PromptWidget::new();
        pw.set_text("hello world");
        assert_eq!(pw.text_without_image_chips(), "hello world");
        pw.insert_image(test_image()).unwrap();
        pw.insert_image(test_image()).unwrap();
        assert!(pw.text().contains("[Image #1]") && pw.text().contains("[Image #2]"));
        let stripped = pw.text_without_image_chips();
        assert!(!stripped.contains("[Image #"), "got {stripped:?}");
        assert!(stripped.contains("hello world"), "got {stripped:?}");
    }

    #[test]
    fn insert_image_leaves_trailing_space() {
        let mut pw = PromptWidget::new();
        pw.insert_image(test_image()).unwrap();

        assert_eq!(pw.textarea.text(), "[Image #1] ");
        assert_eq!(pw.textarea.cursor(), pw.textarea.text().len());
    }

    #[test]
    fn image_numbering_increments() {
        let mut pw = PromptWidget::new();
        pw.insert_image(test_image()).unwrap();
        pw.insert_image(test_image()).unwrap();
        pw.insert_image(test_image()).unwrap();

        assert_eq!(pw.images.len(), 3);
        assert_eq!(pw.images[0].display_number, 1);
        assert_eq!(pw.images[1].display_number, 2);
        assert_eq!(pw.images[2].display_number, 3);
        assert!(pw.textarea.text().contains("[Image #1]"));
        assert!(pw.textarea.text().contains("[Image #2]"));
        assert!(pw.textarea.text().contains("[Image #3]"));
    }

    #[test]
    fn set_text_empty_clears_images() {
        let mut pw = PromptWidget::new();
        pw.insert_image(test_image()).unwrap();
        pw.insert_image(test_image()).unwrap();
        assert_eq!(pw.images.len(), 2);
        assert_eq!(pw.image_counter, 2);

        pw.set_text("");

        assert!(pw.images.is_empty());
        assert_eq!(pw.image_counter, 0);
    }

    /// A non-empty `set_text` replacement with NO `[Image #N]`
    /// placeholders implies the prompt no longer holds chips. Image
    /// state is cleared to prevent orphan `PastedImage` records from
    /// surviving (those would never be referenced by a chip in the
    /// buffer and would still appear in `drain_images()` on send).
    #[test]
    fn set_text_nonempty_no_placeholders_clears_image_state() {
        let mut pw = PromptWidget::new();
        pw.insert_image(test_image()).unwrap();
        assert_eq!(pw.images.len(), 1);

        pw.set_text("some replacement");

        assert!(
            pw.images.is_empty(),
            "non-empty replacement with no chip placeholders must drop \
             orphan PastedImage records",
        );
        assert_eq!(pw.image_counter, 0);
    }

    /// A non-empty `set_text` replacement that retains
    /// `[Image #N]` placeholders preserves image state. The
    /// rewind-restore path in `dispatch.rs` relies on this: the
    /// `restore_chip_elements` + `set_images` calls re-bind the
    /// stashed `PastedImage` records to the chips in the text.
    #[test]
    fn set_text_nonempty_with_placeholders_preserves_image_state() {
        let mut pw = PromptWidget::new();
        pw.insert_image(test_image()).unwrap();
        assert_eq!(pw.images.len(), 1);

        // Replacement text still contains `[Image #1]` so image state
        // is meaningful — keep it. (In real rewind flows, the caller
        // follows with restore_chip_elements + set_images.)
        pw.set_text("look at [Image #1] please");

        assert_eq!(pw.images.len(), 1);
        assert_eq!(pw.image_counter, 1);
    }

    #[test]
    fn image_cap_enforced() {
        let mut pw = PromptWidget::new();
        for _ in 0..PromptWidget::IMAGE_CAP {
            assert!(pw.insert_image(test_image()).is_ok());
        }
        // Cap reached
        assert!(pw.insert_image(test_image()).is_err());
        assert_eq!(pw.images.len(), PromptWidget::IMAGE_CAP);
    }

    #[test]
    fn enter_on_image_chip_returns_image_preview() {
        let mut pw = PromptWidget::new();
        pw.insert_image(test_image()).unwrap();

        // Move cursor onto the image element
        pw.textarea.set_cursor(0);
        assert_eq!(
            pw.try_element_interaction(&key!(Enter).to_key_event()),
            Some(ElementInteraction::ImagePreview)
        );
        // Image chip is NOT inlined — element still present
        assert_eq!(pw.textarea.elements().len(), 1);
        assert_eq!(pw.textarea.text(), "[Image #1] ");
    }

    #[test]
    fn drain_images_reconciles_and_drains() {
        let mut pw = PromptWidget::new();
        pw.insert_image(test_image()).unwrap();
        // Move cursor to end so the second insert appends after the first chip.
        let len = pw.textarea.text().len();
        pw.textarea.set_cursor(len);
        pw.insert_image(test_image()).unwrap();

        // Delete the first element via textarea (simulates backspace)
        pw.textarea.set_cursor(0);
        let first_id = pw.textarea.elements()[0].id;
        pw.textarea.inline_element(first_id);
        // Now there's one image element left, but images vec still has 2

        let drained = pw.drain_images();
        // Reconciliation should have removed the stale entry
        assert_eq!(drained.len(), 1);
        assert_eq!(drained[0].display_number, 2);
        assert!(pw.images.is_empty());
    }

    #[test]
    fn image_at_cursor_returns_record() {
        let mut pw = PromptWidget::new();
        pw.insert_image(test_image()).unwrap();

        // After insert, cursor is past the trailing space (range.end + 1).
        // The overlay does NOT show at that position.
        assert!(
            pw.image_at_cursor().is_none(),
            "cursor past trailing space should not trigger overlay"
        );

        // Move cursor onto the element itself (range.start) where the
        // overlay should show.
        let elem_start = pw.textarea.elements()[0].range.start;
        pw.textarea.set_cursor(elem_start);
        let img = pw.image_at_cursor().unwrap();
        assert_eq!(img.display_number, 1);
        assert_eq!(img.mime_type, "image/png");

        // Cursor at range.end (the trailing space) does NOT show overlay.
        let elem_end = pw.textarea.elements()[0].range.end;
        pw.textarea.set_cursor(elem_end);
        assert!(
            pw.image_at_cursor().is_none(),
            "cursor at trailing space should not trigger overlay"
        );
    }

    /// Deleting the highest-numbered chip mid-prompt must NOT drop the counter
    /// back to the surviving max — otherwise the next insertion reuses an old
    /// number, producing sequences like `[Image #1] [Image #2] [Image #1]`.
    /// Deletes `#2` (counter==2, live max==1) to actually exercise the
    /// monotonic guard.
    #[test]
    fn image_counter_stays_monotonic_after_chip_deletion() {
        let mut pw = PromptWidget::new();
        pw.insert_image(test_image()).unwrap();
        pw.insert_image(test_image()).unwrap();
        assert_eq!(pw.image_counter, 2);
        assert_eq!(pw.images.len(), 2);

        // Delete the higher-numbered chip (#2). After deletion live==[#1] but
        // counter must remain 2 so the next insert lands at #3.
        let second_id = pw.textarea.elements()[1].id;
        let second_range_start = pw.textarea.elements()[1].range.start;
        pw.textarea.set_cursor(second_range_start);
        pw.textarea.inline_element(second_id);

        // Force the reconciliation that `handle_key` runs after
        // every text edit.
        pw.sync_images_with_textarea();

        assert_eq!(
            pw.image_counter, 2,
            "high-water counter must stay at 2 after deleting the \
             top-numbered chip; got {}",
            pw.image_counter,
        );
        // Only `#1` survived.
        assert_eq!(pw.images.len(), 1);
        assert_eq!(pw.images[0].display_number, 1);

        // Next insert lands at #3, NOT a reused #2.
        pw.insert_image(test_image()).unwrap();
        assert_eq!(
            pw.images.last().unwrap().display_number,
            3,
            "next insert must be #3, never reuse #2; images = {:?}",
            pw.images
                .iter()
                .map(|i| i.display_number)
                .collect::<Vec<_>>(),
        );
    }

    /// Same monotonic contract as above, but driven through the natural
    /// `handle_key(Backspace)` path so a refactor that moves the sync hook
    /// past the counter assignment fails at the keystroke layer.
    #[test]
    fn image_counter_stays_monotonic_via_backspace_keystroke() {
        let mut pw = PromptWidget::new();
        pw.insert_image(test_image()).unwrap();
        pw.insert_image(test_image()).unwrap();
        assert_eq!(pw.image_counter, 2);

        // Cursor sits past the trailing space of `[Image #2] `.
        // Backspace removes the trailing space → still 2 chips.
        // Another Backspace removes `[Image #2]` chip (atomic
        // element deletion).
        pw.handle_key(&key!(Backspace).to_key_event());
        pw.handle_key(&key!(Backspace).to_key_event());

        assert_eq!(
            pw.images.len(),
            1,
            "only #1 chip should survive; text = {:?}",
            pw.textarea.text(),
        );
        assert_eq!(pw.images[0].display_number, 1);
        assert_eq!(
            pw.image_counter, 2,
            "high-water counter must survive the natural Backspace \
             handle_key hook",
        );
    }

    /// Three drops in the same prompt must yield `1, 2, 3` — never `1, 2, 1`.
    /// A transient delete of the highest chip is performed between drops #2
    /// and #3 so the monotonic guard is actually exercised (without the
    /// delete, the counter would already equal the live max).
    #[test]
    fn three_drops_in_same_prompt_yield_sequential_numbers() {
        let mut pw = PromptWidget::new();
        pw.insert_image(test_image()).unwrap(); // #1
        pw.insert_image(test_image()).unwrap(); // #2

        // Transient delete of `[Image #2]` mid-prompt.
        let id2 = pw.textarea.elements()[1].id;
        let r2_start = pw.textarea.elements()[1].range.start;
        pw.textarea.set_cursor(r2_start);
        pw.textarea.inline_element(id2);
        pw.sync_images_with_textarea();

        pw.insert_image(test_image()).unwrap(); // MUST be #3

        let numbers: Vec<usize> = pw.images.iter().map(|i| i.display_number).collect();
        assert_eq!(
            numbers,
            vec![1, 3],
            "after a transient #2 delete, the next drop must issue \
             #3 (not reuse #2). Got {numbers:?}",
        );
        // `inline_element` leaves the deleted chip's text behind as plain
        // characters, so count chip ELEMENTS not text matches.
        let live_image_numbers: Vec<usize> = pw
            .textarea
            .elements()
            .iter()
            .filter(|e| e.kind == KIND_IMAGE)
            .map(|e| parse_image_display_number(&pw.textarea.text()[e.range.clone()]).unwrap_or(0))
            .collect();
        assert_eq!(
            live_image_numbers,
            vec![1, 3],
            "live chip elements must be #1 and #3 (never #2 reused)",
        );
    }

    /// `sync_images_with_textarea` keys its staging map on `element_id` so
    /// two `PastedImage` records that accidentally share a `display_number`
    /// both survive (rather than one collapsing in a number-keyed map).
    #[test]
    fn sync_handles_two_images_sharing_display_number() {
        let mut pw = PromptWidget::new();
        pw.insert_image(test_image()).unwrap(); // #1
        pw.insert_image(test_image()).unwrap(); // #2
        assert_eq!(pw.images.len(), 2);
        assert_ne!(
            pw.images[0].element_id, pw.images[1].element_id,
            "insert_image must issue unique element_ids",
        );

        // Corrupt: assign `display_number = 1` to the second image, mimicking
        // a future regression that assigned identical numbers.
        pw.images[1].display_number = 1;

        // The textarea still has two elements with distinct element_ids;
        // the corruption is purely in the `PastedImage` records.
        assert_eq!(pw.textarea.elements().len(), 2);

        pw.sync_images_with_textarea();

        assert_eq!(
            pw.images.len(),
            2,
            "both PastedImages must survive an identical-display_number \
             collision via element_id matching; got {:?}",
            pw.images
                .iter()
                .map(|i| (i.element_id, i.display_number))
                .collect::<Vec<_>>(),
        );
    }

    // ── set_images: identity-based pairing ───────────────────────────

    /// Two restored chips with identical placeholder byte length must
    /// get distinct `element_id`s after `set_images`. A naive
    /// `find()`-by-byte-length match would collapse both chips onto
    /// the same `element_id`, and the next
    /// `sync_images_with_textarea` would drop one entry as a
    /// "duplicate element_id" warn.
    #[test]
    fn set_images_with_two_same_length_chips_preserves_distinct_element_ids() {
        use crate::app::agent::ChipElement;

        let mut pw = PromptWidget::new();
        // Build a buffer that matches the rewind-restore shape:
        // `"[Image #1] [Image #2] "`. Both chip placeholders are 10
        // bytes each — without the fix, the linear `find()` returns
        // the same first element for both images.
        let text = "[Image #1] [Image #2] ";
        pw.set_text(text);

        let chip_a = ChipElement {
            range: 0..10,
            kind: KIND_IMAGE,
            display: None,
        };
        let chip_b = ChipElement {
            range: 11..21,
            kind: KIND_IMAGE,
            display: None,
        };
        pw.restore_chip_elements(&[chip_a, chip_b]);

        // Two distinct elements registered.
        let elem_ids: Vec<_> = pw
            .textarea
            .elements()
            .iter()
            .filter(|e| e.kind == KIND_IMAGE)
            .map(|e| e.id)
            .collect();
        assert_eq!(elem_ids.len(), 2);
        assert_ne!(elem_ids[0], elem_ids[1]);

        // Two PastedImage records in the same source order.
        let mut img_a = test_image();
        img_a.display_number = 1;
        let mut img_b = test_image();
        img_b.display_number = 2;
        pw.set_images(vec![img_a, img_b]);

        // Each PastedImage got its OWN element id.
        assert_eq!(pw.images.len(), 2);
        assert_eq!(pw.images[0].element_id, elem_ids[0]);
        assert_eq!(pw.images[1].element_id, elem_ids[1]);
        assert_ne!(pw.images[0].element_id, pw.images[1].element_id);

        // Survive a sync_images_with_textarea: both chips must still
        // be there, neither collapsed onto the other's id.
        pw.sync_images_with_textarea();
        assert_eq!(pw.images.len(), 2);
        let post_sync_ids: Vec<_> = pw.images.iter().map(|i| i.element_id).collect();
        assert_eq!(post_sync_ids[0], elem_ids[0]);
        assert_eq!(post_sync_ids[1], elem_ids[1]);

        // Pin the range-identity binding. A regression that mapped
        // both PastedImages to distinct-but-wrong element_ids would
        // pass the `assert_ne!` above but break this check.
        let elem_a_range = pw
            .textarea
            .elements()
            .iter()
            .find(|e| e.id == pw.images[0].element_id)
            .unwrap()
            .range
            .clone();
        assert_eq!(elem_a_range, 0..10);
        let elem_b_range = pw
            .textarea
            .elements()
            .iter()
            .find(|e| e.id == pw.images[1].element_id)
            .unwrap()
            .range
            .clone();
        assert_eq!(elem_b_range, 11..21);
    }

    /// Inputs longer than `IMAGE_CAP` are truncated at the top of
    /// `set_images`. Dropping the truncate (or off-by-one'ing the
    /// comparison) would silently let a malformed session restore
    /// unbounded images.
    #[test]
    fn set_images_truncates_input_exceeding_image_cap() {
        use crate::app::agent::ChipElement;

        let mut pw = PromptWidget::new();

        // Build text + chip elements for `IMAGE_CAP + 5` chips. The
        // numbering must match what `display_text(N)` produces
        // (10-char `[Image #N]` for N<10, 11-char for N>=10).
        let mut text = String::new();
        let mut chip_elements: Vec<ChipElement> = Vec::new();
        let cap_plus = PromptWidget::IMAGE_CAP + 5;
        for n in 1..=cap_plus {
            let chip = crate::prompt_images::display_text(n);
            let start = text.len();
            text.push_str(&chip);
            let end = text.len();
            text.push(' ');
            chip_elements.push(ChipElement {
                range: start..end,
                kind: KIND_IMAGE,
                display: None,
            });
        }
        pw.set_text(&text);
        pw.restore_chip_elements(&chip_elements);

        let oversized: Vec<PastedImage> = (1..=cap_plus)
            .map(|n| {
                let mut img = test_image();
                img.display_number = n;
                img
            })
            .collect();
        assert!(oversized.len() > PromptWidget::IMAGE_CAP);

        pw.set_images(oversized);

        assert_eq!(
            pw.images.len(),
            PromptWidget::IMAGE_CAP,
            "input above the cap must be truncated to IMAGE_CAP",
        );
        // Truncation preserves the first IMAGE_CAP entries (display
        // numbers 1..=IMAGE_CAP). The last 5 are dropped.
        let numbers: Vec<usize> = pw.images.iter().map(|i| i.display_number).collect();
        assert_eq!(numbers, (1..=PromptWidget::IMAGE_CAP).collect::<Vec<_>>(),);
    }

    /// The bounded-stash cap at `IMAGE_CAP * 2` evicts oldest-first
    /// (by `display_number`) and cleans up the staged temp file on
    /// each evicted record. Dropping the cleanup hook would silently
    /// leak temp files; flipping the sort order would keep the wrong
    /// half of the stash on redo.
    #[test]
    fn sync_caps_image_undo_stash_and_cleans_up_evicted_temp_files() {
        let mut pw = PromptWidget::new();

        // Stuff `IMAGE_CAP * 2 + 3` entries into the stash directly.
        // Each entry has a unique staged_temp_path so we can observe
        // which files were cleaned up by the eviction hook.
        let dir = tempfile::tempdir().unwrap();
        let total = PromptWidget::IMAGE_CAP * 2 + 3;
        let mut temp_paths: Vec<std::path::PathBuf> = Vec::new();
        for n in 1..=total {
            let temp_path = dir.path().join(format!("staged-{}.png", n));
            std::fs::write(&temp_path, b"x").unwrap();
            temp_paths.push(temp_path.clone());
            let mut img = test_image();
            img.display_number = n;
            img.staged_temp_path = Some(temp_path);
            pw.image_undo_stash.push(img);
        }

        // Force the sync path that runs the eviction logic. The
        // textarea has no chip elements, so all stash entries are
        // re-stashed (no live chip to rebind), then capped.
        pw.sync_images_with_textarea();

        assert_eq!(
            pw.image_undo_stash.len(),
            PromptWidget::IMAGE_CAP * 2,
            "stash must be capped at IMAGE_CAP * 2",
        );

        // Surviving entries are the highest `display_number` ones
        // (oldest evicted == lowest numbers).
        let survivors: Vec<usize> = pw
            .image_undo_stash
            .iter()
            .map(|i| i.display_number)
            .collect();
        let mut survivors_sorted = survivors.clone();
        survivors_sorted.sort();
        let expected_lowest = total - PromptWidget::IMAGE_CAP * 2 + 1;
        assert_eq!(survivors_sorted[0], expected_lowest);
        assert_eq!(*survivors_sorted.last().unwrap(), total);

        // The 3 evicted entries (display_number 1, 2, 3) had their
        // staged temp files cleaned up.
        for n in 1..=3 {
            let evicted_path = &temp_paths[n - 1];
            assert!(
                !evicted_path.exists(),
                "evicted stash entry's staged temp file must be cleaned up; \
                 still on disk: {:?}",
                evicted_path,
            );
        }
        // Survivors' files remain on disk.
        for n in 4..=total {
            let kept_path = &temp_paths[n - 1];
            assert!(
                kept_path.exists(),
                "surviving stash entry must keep its staged temp file; \
                 missing: {:?}",
                kept_path,
            );
        }
    }

    /// `self.images` is populated by `insert_image` in
    /// **chronological** order, but `textarea.elements()` is sorted
    /// by **buffer position**. Inserting the second image at the
    /// start of the buffer (cursor-at-Home) is enough to make the
    /// two arrays diverge. The drain → restore → set_images
    /// pipeline must still bind each `PastedImage` to the chip with
    /// the matching `display_number` — a positional zip would
    /// silently swap them.
    #[test]
    fn set_images_pairs_by_display_number_after_out_of_order_insert() {
        let mut pw = PromptWidget::new();

        // Insert chip #1 at end (cursor sits past trailing space).
        pw.insert_image(test_image()).unwrap();

        // Move cursor to start, then insert chip #2. Buffer order is
        // now `[Image #2] [Image #1] ` but `self.images` is
        // chronological: [#1, #2].
        pw.textarea.set_cursor(0);
        pw.insert_image(test_image()).unwrap();
        assert_eq!(pw.images[0].display_number, 1);
        assert_eq!(pw.images[1].display_number, 2);
        // textarea elements are sorted by buffer position: [#2, #1].
        let elems_in_buf_order: Vec<usize> = pw
            .textarea
            .elements()
            .iter()
            .filter(|e| e.kind == KIND_IMAGE)
            .map(|e| {
                let text = pw.textarea.text();
                parse_image_display_number(&text[e.range.clone()]).unwrap()
            })
            .collect();
        assert_eq!(
            elems_in_buf_order,
            vec![2, 1],
            "textarea elements must be in buffer order: #2 before #1",
        );

        // Capture the current binding before the drain/restore round-trip.
        let pre_drain_eid_1 = pw.images[0].element_id;
        let pre_drain_eid_2 = pw.images[1].element_id;

        // Simulate the rewind-restore round-trip: drain images
        // (chronological order), capture chip elements (buffer order),
        // set_text back, restore_chip_elements, set_images.
        let images = pw.drain_images();
        assert_eq!(images[0].display_number, 1);
        assert_eq!(images[1].display_number, 2);
        let chip_elements: Vec<crate::app::agent::ChipElement> = pw
            .textarea
            .elements()
            .iter()
            .map(|e| crate::app::agent::ChipElement {
                range: e.range.clone(),
                kind: e.kind,
                display: e.display.clone(),
            })
            .collect();

        let text = pw.textarea.text().to_string();
        pw.set_text(&text);
        pw.restore_chip_elements(&chip_elements);
        pw.set_images(images);

        // After restore, each PastedImage must bind to the chip whose
        // text parses to its display_number — NOT the chip at the same
        // positional index.
        let buf = pw.textarea.text();
        for img in &pw.images {
            let elem = pw
                .textarea
                .elements()
                .iter()
                .find(|e| e.id == img.element_id)
                .expect("PastedImage.element_id must match a live element");
            let parsed = parse_image_display_number(&buf[elem.range.clone()]);
            assert_eq!(
                parsed,
                Some(img.display_number),
                "PastedImage display_number {} must bind to a chip whose \
                 text parses to the same number; bound element parsed as {:?}",
                img.display_number,
                parsed,
            );
        }

        // The textarea reissues fresh `ElementId`s on
        // `restore_chip_elements`, so each `PastedImage` must have
        // been rebound to a new id rather than carrying the
        // pre-drain identity through.
        let post_eids: Vec<_> = pw.images.iter().map(|i| i.element_id).collect();
        assert!(
            post_eids.iter().all(|eid| *eid != pre_drain_eid_1),
            "restored chips must receive freshly-issued element ids \
             (pre={:?}, post={:?})",
            pre_drain_eid_1,
            post_eids,
        );
        assert!(
            post_eids.iter().all(|eid| *eid != pre_drain_eid_2),
            "restored chips must receive freshly-issued element ids \
             (pre={:?}, post={:?})",
            pre_drain_eid_2,
            post_eids,
        );
    }

    // ── parse_image_display_number ───────────────────────────────────

    #[test]
    fn parse_image_display_number_bracketed_form() {
        assert_eq!(parse_image_display_number("[Image #1]"), Some(1));
        assert_eq!(parse_image_display_number("[Image #12]"), Some(12));
        assert_eq!(parse_image_display_number("[Image #999]"), Some(999));
    }

    #[test]
    fn parse_image_display_number_handles_path_suffix_form() {
        // Path-suffix form `[Image #N: <path>]` must parse the number out.
        // Without the suffix split, the inner ":/foo/bar.png" sub-string
        // breaks `usize::parse` and the function silently returns `None`.
        assert_eq!(
            parse_image_display_number("[Image #3: /foo/bar.png]"),
            Some(3),
        );
        assert_eq!(
            parse_image_display_number("[Image #12: /some/path with spaces.png]"),
            Some(12),
        );
        // Empty path suffix (degenerate but representable).
        assert_eq!(parse_image_display_number("[Image #1:]"), Some(1));
        assert_eq!(parse_image_display_number("[Image #5: ]"), Some(5));
        // Multi-colon path (legal on Unix, common on macOS for
        // certain Time Machine / network mount paths). The split is
        // on the FIRST `:` only, so subsequent colons land in the
        // discarded suffix.
        assert_eq!(
            parse_image_display_number("[Image #7: /odd:name:with:colons.png]"),
            Some(7),
        );
    }

    /// Pin the parser's intentional permissiveness with respect to
    /// whitespace inside the `#N` token. The canonical emitter
    /// (`display_text`) never produces `[Image # 1]` or
    /// `[Image #1 ]` — the permissiveness is purely defence in
    /// depth against a future emitter change or a clipboard payload
    /// that somehow injects a space.
    #[test]
    fn parse_image_display_number_tolerates_internal_whitespace() {
        assert_eq!(parse_image_display_number("[Image # 1]"), Some(1));
        assert_eq!(parse_image_display_number("[Image #1 ]"), Some(1));
        assert_eq!(parse_image_display_number("[Image # 12 ]"), Some(12));
    }

    #[test]
    fn parse_image_display_number_rejects_non_matches() {
        assert_eq!(parse_image_display_number(""), None);
        assert_eq!(parse_image_display_number("[Image #]"), None);
        assert_eq!(parse_image_display_number("[Image abc]"), None);
        assert_eq!(parse_image_display_number("not a chip"), None);
        assert_eq!(parse_image_display_number("[Image #1"), None);
    }

    /// Source-path metadata must survive chip deletion and undo.
    #[test]
    fn source_path_chip_survives_undo_redo() {
        let dir = tempfile::tempdir().unwrap();
        let foo_path = dir.path().join("foo.png");
        let bar_path = dir.path().join("bar.png");

        let mut pw = PromptWidget::new();
        let mut img1 = test_image();
        img1.source_path = Some(foo_path.clone());
        img1.display_number = 0; // overwritten by insert_image
        pw.insert_image(img1).unwrap();

        let mut img2 = test_image();
        img2.source_path = Some(bar_path.clone());
        img2.display_number = 0;
        pw.insert_image(img2).unwrap();
        assert_eq!(pw.images.len(), 2);
        assert_eq!(pw.images[1].display_number, 2);
        assert_eq!(
            pw.images[1].source_path.as_deref(),
            Some(bar_path.as_path())
        );
        let buf_before = pw.textarea.text().to_string();
        assert!(
            !buf_before.contains("foo.png") && !buf_before.contains("bar.png"),
            "chip buffer text is path-free: {buf_before:?}"
        );

        // Move past the spacer so Backspace targets the chip.
        let end = pw.textarea.elements()[1].range.end;
        pw.textarea.set_cursor(end + 1);
        pw.handle_key(&key!(Backspace).to_key_event());
        pw.handle_key(&key!(Backspace).to_key_event());
        assert_eq!(pw.images.len(), 1, "chip #2 should be deleted");
        assert_eq!(pw.images[0].display_number, 1);

        pw.handle_key(&key!('z', CONTROL).to_key_event());
        pw.sync_images_with_textarea();

        assert_eq!(pw.textarea.text(), buf_before);
        assert_eq!(
            pw.images.len(),
            2,
            "undo must restore chip #2 from the undo stash; buf = {:?}, \
             stash = {:?}",
            pw.textarea.text(),
            pw.image_undo_stash
                .iter()
                .map(|i| i.display_number)
                .collect::<Vec<_>>(),
        );
        let restored = pw
            .images
            .iter()
            .find(|i| i.display_number == 2)
            .expect("PastedImage with display_number=2 must be recovered");
        assert_eq!(
            restored.source_path.as_deref(),
            Some(bar_path.as_path()),
            "source_path must survive undo+sync; got {:?}",
            restored.source_path,
        );
    }

    /// Ctrl+C on a non-empty prompt clears all content AND zeroes the image
    /// counter so the next drop starts at #1. Complements
    /// `image_counter_resets_after_clear_and_reinsert` which covers the
    /// `set_text("")` reset path; this one drives Ctrl+C through `handle_key`.
    #[test]
    fn ctrl_c_clear_resets_image_counter_to_zero() {
        let mut pw = PromptWidget::new();
        pw.insert_image(test_image()).unwrap();
        pw.insert_image(test_image()).unwrap();
        assert_eq!(pw.image_counter, 2);

        // Ctrl+C → set_text("") → `crate::prompt_images::clear` zeroes the counter.
        pw.handle_key(&key!('c', CONTROL).to_key_event());

        assert!(pw.textarea.text().is_empty());
        assert!(pw.images.is_empty());
        assert_eq!(
            pw.image_counter, 0,
            "Ctrl+C clear must zero the counter so the next drop \
             starts at #1",
        );

        pw.insert_image(test_image()).unwrap();
        assert_eq!(pw.images[0].display_number, 1);
        assert_eq!(pw.textarea.text(), "[Image #1] ");
    }

    #[test]
    fn image_counter_resets_after_clear_and_reinsert() {
        let mut pw = PromptWidget::new();
        pw.insert_image(test_image()).unwrap();
        pw.insert_image(test_image()).unwrap();
        assert_eq!(pw.image_counter, 2);

        pw.set_text("");
        assert_eq!(pw.image_counter, 0);

        pw.insert_image(test_image()).unwrap();
        assert_eq!(pw.image_counter, 1);
        assert_eq!(pw.images[0].display_number, 1);
        assert_eq!(pw.textarea.text(), "[Image #1] ");
    }

    /// A prompt widget with both contextual-hint tips enabled, so the
    /// per-keystroke detection (gated off by default) actually runs in these
    /// on-path tests.
    fn hinted_prompt() -> PromptWidget {
        let mut pw = PromptWidget::new();
        pw.contextual_hint_undo = true;
        pw.contextual_hint_plan_mode = true;
        pw
    }

    /// Type `n` chars through `handle_key` so the undo-tip clear detector
    /// observes a user-built draft.
    fn type_chars(pw: &mut PromptWidget, n: usize) {
        for _ in 0..n {
            assert_eq!(
                pw.handle_key(&key!('x').to_key_event()),
                PromptEvent::Edited
            );
        }
    }

    /// Ctrl+C wiping a substantial user-typed draft raises the one-shot
    /// undo-tip fire signal, and the wipe is genuinely undoable.
    #[test]
    fn ctrl_c_clear_of_substantial_draft_fires_undo_tip() {
        let mut pw = hinted_prompt();
        type_chars(&mut pw, 25);
        assert!(!pw.take_undo_tip_fire(), "typing must not fire");

        pw.handle_key(&key!('c', CONTROL).to_key_event());
        assert!(pw.textarea.text().is_empty());
        assert!(pw.textarea.can_undo(), "clear must be recoverable");
        assert!(pw.take_undo_tip_fire(), "substantial wipe fires");
        assert!(!pw.take_undo_tip_fire(), "signal is one-shot");
    }

    /// Ctrl+U (kill whole line) is a one-shot wipe too.
    #[test]
    fn ctrl_u_kill_of_substantial_draft_fires_undo_tip() {
        let mut pw = hinted_prompt();
        type_chars(&mut pw, 25);
        pw.handle_key(&key!('u', CONTROL).to_key_event());
        assert!(pw.textarea.text().is_empty());
        assert!(pw.take_undo_tip_fire());
    }

    /// Programmatic clears (submit/queue flows call `set_text` directly)
    /// never fire, and the stale peak cannot leak into the next keystroke.
    #[test]
    fn programmatic_set_text_clear_does_not_fire_undo_tip() {
        let mut pw = hinted_prompt();
        type_chars(&mut pw, 25);
        pw.set_text("");
        assert!(!pw.take_undo_tip_fire());
        type_chars(&mut pw, 1);
        assert!(!pw.take_undo_tip_fire(), "resync absorbs the stale peak");
    }

    /// Ctrl+C with images attached must NOT fire: `set_text("")` drains the
    /// image payloads, so the advertised undo would restore dead chips.
    #[test]
    fn ctrl_c_clear_with_images_suppresses_undo_tip() {
        let mut pw = hinted_prompt();
        pw.insert_image(test_image()).unwrap();
        type_chars(&mut pw, 25);

        pw.handle_key(&key!('c', CONTROL).to_key_event());
        assert!(pw.textarea.text().is_empty());
        assert!(pw.images.is_empty() && pw.image_undo_stash.is_empty());
        assert!(
            !pw.take_undo_tip_fire(),
            "lossy wipe must not advertise undo"
        );
    }

    /// Kill-style wipes stash image payloads (fully restorable), so an
    /// image-bearing ctrl+u still fires.
    #[test]
    fn ctrl_u_kill_with_images_still_fires_undo_tip() {
        let mut pw = hinted_prompt();
        pw.insert_image(test_image()).unwrap();
        type_chars(&mut pw, 25);

        pw.handle_key(&key!('u', CONTROL).to_key_event());
        assert!(pw.textarea.text().is_empty());
        assert!(
            !pw.image_undo_stash.is_empty(),
            "payload stashed for undo recovery"
        );
        assert!(pw.take_undo_tip_fire());
    }

    /// Accepting an @-file completion replaces a long `@query` with a short
    /// ref/chip — a big shrink, but a completion, not a wipe. It must NOT fire
    /// the undo tip (regression: the wrapper used to feed the accept to the
    /// clear detector, spuriously tripping the wipe thresholds).
    #[test]
    fn accepting_file_completion_does_not_fire_undo_tip() {
        let mut pw = hinted_prompt();
        // Build a long `@`-query THROUGH handle_key so the clear detector
        // observes a peak >= FIRE_PEAK_LEN and its last_len matches the
        // on-screen length — the precondition under which a shrink fires.
        pw.handle_key(&key!('@').to_key_event());
        type_chars(&mut pw, 24); // "@" + 24 = 25 chars
        assert!(!pw.take_undo_tip_fire(), "typing must not fire");

        // Force the dropdown visible with a SHORT file result so accepting
        // shrinks the draft into the wipe-residue band (<= FIRE_RESIDUE_LEN).
        let cursor = pw.textarea.text().len();
        let ctx = crate::views::file_search::context::detect(pw.textarea.text(), cursor)
            .expect("@-context must parse");
        pw.file_search
            .set_test_state(ctx, vec![fuzzy_result("z", false)], 0);
        assert!(pw.file_search.is_visible());

        // Accept via Tab: "@xxxx…" -> "@z " is a big shrink but a completion.
        assert_eq!(
            pw.handle_key(&key!(Tab).to_key_event()),
            PromptEvent::Edited
        );
        assert!(
            pw.textarea.text().chars().count() <= 5,
            "fixture must land in the residue band so the bug could fire"
        );
        assert!(
            !pw.take_undo_tip_fire(),
            "accepting a completion must not fire the undo tip"
        );
    }

    /// Typing a draft across into a planning keyword raises the one-shot
    /// plan-nudge fire signal exactly once (rising edge), then stays quiet
    /// while the keyword remains present.
    #[test]
    fn typing_into_planning_keyword_fires_plan_nudge_once() {
        let mut pw = hinted_prompt();
        for ch in "pla".chars() {
            pw.handle_key(&KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
        }
        assert!(!pw.take_plan_nudge_fire(), "partial keyword must not fire");
        pw.handle_key(&KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE));
        assert!(pw.take_plan_nudge_fire(), "crossing into a keyword fires");
        assert!(!pw.take_plan_nudge_fire(), "signal is one-shot");
        for ch in " the refactor".chars() {
            pw.handle_key(&KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
        }
        assert!(
            !pw.take_plan_nudge_fire(),
            "rising edge must not refire while the keyword stays present"
        );
    }

    /// Slash- and bash-prefixed drafts route to a command, not a planning
    /// prompt, so the plan nudge stays silent even with a keyword present.
    #[test]
    fn slash_and_bash_drafts_suppress_plan_nudge() {
        for prefix in ['/', '!'] {
            let mut pw = hinted_prompt();
            pw.handle_key(&KeyEvent::new(KeyCode::Char(prefix), KeyModifiers::NONE));
            for ch in "plan".chars() {
                pw.handle_key(&KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
            }
            assert!(
                !pw.take_plan_nudge_fire(),
                "{prefix}-prefixed draft must suppress the plan nudge"
            );
        }
    }

    /// A programmatic restore (`set_text`) of a draft that already mentions a
    /// planning keyword must NOT fire on the next real edit — the user never
    /// typed the keyword across the rising edge. The before/after scan reads
    /// the pre-edit text fresh, so `before == after == keyword-present` → no
    /// fire (no per-writer resync needed).
    #[test]
    fn restored_keyword_draft_does_not_fire_plan_nudge() {
        let mut pw = hinted_prompt();
        pw.set_text("design the new module");
        assert!(!pw.take_plan_nudge_fire(), "set_text itself never fires");
        // A real edit that keeps the keyword present must not fire.
        pw.handle_key(&KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE));
        assert!(
            !pw.take_plan_nudge_fire(),
            "edit after a restored-keyword draft must not fire (before==after)"
        );
    }

    /// A bracketed paste of a keyword (which bypasses `handle_key`) must not
    /// fire on the following keystroke — the fresh pre-edit read sees the
    /// keyword already present.
    #[test]
    fn pasted_keyword_does_not_fire_plan_nudge_on_next_key() {
        let mut pw = hinted_prompt();
        assert_eq!(pw.handle_paste("design the API"), PromptEvent::Edited);
        assert!(
            !pw.take_plan_nudge_fire(),
            "paste itself never fires the nudge"
        );
        pw.handle_key(&KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE));
        assert!(
            !pw.take_plan_nudge_fire(),
            "edit after a pasted keyword must not fire (before==after)"
        );
    }

    /// Accepting an `@`-file completion whose path contains a planning keyword
    /// (e.g. `@design.py`) sets `completion_accepted` on that keypress, so it
    /// never fires; and the NEXT keystroke must not fire either, because the
    /// fresh pre-edit read sees the keyword the accept put in the buffer. This
    /// is the leaky-latch bug the before/after revert fixes.
    #[test]
    fn completion_accept_of_keyword_path_does_not_fire_plan_nudge() {
        let mut pw = hinted_prompt();
        seed_at_completion(&mut pw, "des", "design.py", false);
        assert_eq!(
            pw.handle_key(&key!(Tab).to_key_event()),
            PromptEvent::Edited
        );
        assert!(
            !pw.take_plan_nudge_fire(),
            "the accept keypress must not fire"
        );
        assert!(
            crate::tips::plan_nudge::prompt_mentions_planning(pw.text()),
            "fixture: the accepted path must leave a keyword in the buffer"
        );
        // A normal keystroke after the accept must NOT fire — the keyword was
        // already present pre-edit.
        pw.handle_key(&KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE));
        assert!(
            !pw.take_plan_nudge_fire(),
            "edit after a completion-accept of a keyword path must not fire"
        );
    }

    /// A paste chord (inline Ctrl+Shift+V or Ctrl/Cmd+V) is not a typed edit,
    /// so it never fires the rising edge even when it brings a keyword into an
    /// empty draft — keeping inline paste consistent with bracketed paste.
    #[test]
    fn paste_chord_edit_does_not_fire_plan_nudge() {
        let mut pw = PromptWidget::new();
        pw.set_text("design the api");
        // The same buffer transition via a TYPED key would fire (before_kw=false)...
        assert!(pw.plan_nudge_fire_for_edit(&key!('x').to_key_event(), false));
        // ...but a paste chord must not.
        assert!(!pw.plan_nudge_fire_for_edit(&key!('v', CONTROL | SHIFT).to_key_event(), false));
        assert!(!pw.plan_nudge_fire_for_edit(&key!('v', CONTROL).to_key_event(), false));
    }

    /// With contextual hints disabled (the default), `handle_key` skips the
    /// per-keystroke detection entirely: a qualifying wipe and a
    /// keyword-crossing edit both stay silent.
    #[test]
    fn contextual_hints_disabled_skips_tip_detection() {
        let mut pw = PromptWidget::new();
        assert!(
            !pw.contextual_hint_undo && !pw.contextual_hint_plan_mode,
            "both tips default off"
        );

        // A substantial draft wiped with ctrl+c would fire the undo tip when on.
        type_chars(&mut pw, 25);
        pw.handle_key(&key!('c', CONTROL).to_key_event());
        assert!(pw.textarea.text().is_empty());
        assert!(
            !pw.take_undo_tip_fire(),
            "undo tip must not fire when disabled"
        );

        // Typing across into a planning keyword would fire the nudge when on.
        for ch in "plan".chars() {
            pw.handle_key(&KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
        }
        assert!(
            !pw.take_plan_nudge_fire(),
            "plan nudge must not fire when disabled"
        );
    }

    /// Per-tip independence: with the plan-mode tip off but the undo tip on,
    /// only the undo detector runs — a substantial wipe fires undo while a
    /// keyword-crossing edit stays silent.
    #[test]
    fn contextual_hints_per_tip_gates_are_independent() {
        let mut pw = PromptWidget::new();
        pw.set_contextual_hints(/* undo */ true, /* plan_mode */ false);

        // Cross into a planning keyword — plan nudge is off, so it must not fire.
        for ch in "plan".chars() {
            pw.handle_key(&KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
        }
        assert!(
            !pw.take_plan_nudge_fire(),
            "plan nudge must stay silent when only undo is enabled"
        );

        // A substantial draft wiped with ctrl+c must still fire the undo tip.
        type_chars(&mut pw, 25);
        pw.handle_key(&key!('c', CONTROL).to_key_event());
        assert!(pw.textarea.text().is_empty());
        assert!(
            pw.take_undo_tip_fire(),
            "undo tip must fire when its own gate is on"
        );
    }

    #[test]
    fn typing_slash_before_image_keeps_slash_menu_open() {
        let mut pw = PromptWidget::new();
        let models = crate::acp::model_state::ModelState::default();

        pw.insert_image(test_image()).unwrap();
        pw.textarea.set_cursor(0);
        assert_eq!(
            pw.handle_key(&key!('/').to_key_event()),
            PromptEvent::Edited
        );

        pw.refresh_slash(&models);

        assert_eq!(pw.textarea.text(), "/[Image #1] ");
        let snap = pw.slash_snapshot();
        assert!(snap.active, "slash should activate before an image chip");
        assert!(
            snap.open,
            "slash dropdown should stay visible before an image chip"
        );
        assert!(!snap.matches.is_empty());
    }

    #[test]
    fn typing_space_after_slash_before_image_keeps_slash_menu_open() {
        let mut pw = PromptWidget::new();
        let models = crate::acp::model_state::ModelState::default();

        pw.insert_image(test_image()).unwrap();
        pw.textarea.set_cursor(0);
        assert_eq!(
            pw.handle_key(&key!('/').to_key_event()),
            PromptEvent::Edited
        );
        assert_eq!(
            pw.handle_key(&key!(' ').to_key_event()),
            PromptEvent::Edited
        );

        pw.refresh_slash(&models);

        assert_eq!(pw.textarea.text(), "/ [Image #1] ");
        let snap = pw.slash_snapshot();
        assert!(
            snap.active,
            "slash should stay active with whitespace before an image chip"
        );
        assert!(
            snap.open,
            "slash dropdown should stay visible with whitespace before an image chip"
        );
        assert!(!snap.matches.is_empty());
    }

    // ── T8: lifecycle edge-case tests ──────────────────────────────

    #[test]
    fn ctrl_c_clears_image_state() {
        let mut pw = PromptWidget::new();
        pw.insert_image(test_image()).unwrap();
        pw.textarea.insert_str(" some text");
        assert_eq!(pw.images.len(), 1);

        // Ctrl-C on non-empty prompt clears everything.
        pw.handle_key(&key!('c', CONTROL).to_key_event());
        assert!(pw.textarea.text().is_empty());
        assert!(pw.images.is_empty());
        assert_eq!(pw.image_counter, 0);
    }

    #[test]
    fn undo_image_then_drain_excludes_undone_image() {
        let mut pw = PromptWidget::new();
        pw.insert_image(test_image()).unwrap();
        assert_eq!(pw.images.len(), 1);
        assert_eq!(pw.textarea.elements().len(), 1);

        // Undo the image insertion — removes the element from TextArea
        // but images vec may still hold the stale entry.
        pw.handle_key(&key!('z', CONTROL).to_key_event());
        assert!(pw.textarea.elements().is_empty());

        // drain_images reconciles against live elements.
        let drained = pw.drain_images();
        assert!(drained.is_empty(), "undone image must not be drained");
    }

    #[test]
    fn redo_image_restores_image_state() {
        let mut pw = PromptWidget::new();
        pw.insert_image(test_image()).unwrap();

        assert_eq!(pw.images.len(), 1);
        assert_eq!(pw.textarea.elements().len(), 1);

        pw.handle_key(&key!('z', CONTROL).to_key_event());
        assert!(pw.textarea.elements().is_empty());

        pw.handle_key(&key!('z', CONTROL | SHIFT).to_key_event());
        assert_eq!(pw.textarea.elements().len(), 1);
        assert_eq!(pw.images.len(), 1, "redo should restore image state");

        let drained = pw.drain_images();
        assert_eq!(drained.len(), 1, "redo should restore image for submission");
    }

    #[test]
    fn deleted_chip_does_not_produce_content_block() {
        let mut pw = PromptWidget::new();
        pw.insert_image(test_image()).unwrap();
        pw.insert_image(test_image()).unwrap();
        assert_eq!(pw.images.len(), 2);

        // Delete first element by inlining (simulates backspace removal path).
        pw.textarea.set_cursor(0);
        let first_id = pw.textarea.elements()[0].id;
        pw.textarea.inline_element(first_id);

        // Drain and build content blocks.
        let images = pw.drain_images();
        let blocks =
            crate::prompt_images::build_content_blocks_with_workspace("text".into(), images, None);
        // Text block + 1 valid image = 2 blocks (not 3).
        assert_eq!(blocks.len(), 2);
    }

    #[test]
    fn ctrl_r_redoes_image_state() {
        let mut pw = PromptWidget::new();
        pw.insert_image(test_image()).unwrap();

        pw.handle_key(&key!('z', CONTROL).to_key_event());
        assert!(pw.textarea.elements().is_empty());

        pw.handle_key(&key!('r', CONTROL).to_key_event());
        assert_eq!(pw.textarea.elements().len(), 1);
        assert_eq!(pw.images.len(), 1, "Ctrl-R redo should restore image state");

        let drained = pw.drain_images();
        assert_eq!(
            drained.len(),
            1,
            "Ctrl-R redo should restore image for submission"
        );
    }

    #[test]
    fn redo_image_with_surrounding_text_restores_for_submission() {
        let mut pw = PromptWidget::new();
        pw.handle_key(&key!('/').to_key_event());
        pw.handle_key(&key!(' ').to_key_event());
        pw.handle_key(&key!('w').to_key_event());
        pw.handle_key(&key!('h').to_key_event());
        pw.handle_key(&key!('o').to_key_event());
        pw.handle_key(&key!(' ').to_key_event());
        pw.insert_image(test_image()).unwrap();
        pw.handle_key(&key!(' ').to_key_event());
        pw.handle_key(&key!('x').to_key_event());

        let before_undo = pw.textarea.text().to_string();
        assert!(before_undo.contains("[Image #1]"));

        pw.handle_key(&key!('z', CONTROL).to_key_event());
        pw.handle_key(&key!('z', CONTROL | SHIFT).to_key_event());

        assert_eq!(pw.textarea.text(), before_undo);
        let drained = pw.drain_images();
        assert_eq!(
            drained.len(),
            1,
            "redo should preserve image with surrounding text"
        );
    }

    #[test]
    fn hovered_image_preview_survives_redo() {
        let mut pw = PromptWidget::new();
        pw.insert_image(test_image()).unwrap();

        pw.handle_key(&key!('z', CONTROL).to_key_event());
        pw.handle_key(&key!('z', CONTROL | SHIFT).to_key_event());

        assert_eq!(pw.textarea.elements().len(), 1);
        let restored_id = pw.images[0].element_id;

        // Simulate mouse hover on the restored image element.
        pw.hovered_image_element_id = Some(restored_id);
        assert!(
            pw.images.iter().any(|img| img.element_id == restored_id),
            "hovered restored image should be in images vec for preview"
        );
    }

    #[test]
    fn deleting_all_text_keeps_image_counter_high_water_mark() {
        // Monotonic counter contract (Bug C): within a single prompt
        // lifetime the counter only ever advances upward. Backspacing
        // through the textarea content removes the chip elements but
        // does NOT trigger a counter reset — only an explicit prompt
        // reset (`set_text("")`, Ctrl+C) zeros the counter. This
        // prevents the bug where a brief empty-buffer state between
        // drops let a fresh insertion reuse `#1`, producing the
        // user-reported sequence `[Image #1] [Image #2] [Image #1]`
        // in a single prompt.
        let mut pw = PromptWidget::new();
        pw.insert_image(test_image()).unwrap();
        pw.handle_key(&key!(' ').to_key_event());
        pw.handle_key(&key!('x').to_key_event());

        while !pw.textarea.text().is_empty() {
            assert_eq!(
                pw.handle_key(&key!(Backspace).to_key_event()),
                PromptEvent::Edited
            );
        }

        pw.sync_images_with_textarea();
        assert!(
            pw.images.is_empty(),
            "deleting all content should clear image state"
        );
        assert_eq!(
            pw.image_counter, 1,
            "deleting all content must NOT reset the high-water counter \
             (only set_text(\"\") / Ctrl+C resets)"
        );

        // The next inserted image continues from the high-water mark.
        pw.insert_image(test_image()).unwrap();
        assert_eq!(pw.textarea.text(), "[Image #2] ");
        assert_eq!(pw.images[0].display_number, 2);
    }

    // ── File search Right Arrow (drill-down) ────────────────────────────

    /// Build a `FuzzyMatchResult` for use in test fixtures.
    fn fuzzy_result(path: &str, is_dir: bool) -> echo_agent_workspace::file_system::FuzzyMatchResult {
        echo_agent_workspace::file_system::FuzzyMatchResult {
            path: nucleo::Utf32String::from(path),
            score: 100,
            indices: Vec::new(),
            is_dir,
        }
    }

    /// Seed the prompt with `@<query>` text, place the cursor at the end, and
    /// inject a single fake fuzzy result so the dropdown is "visible" and has
    /// a valid selection.
    fn seed_at_completion(
        pw: &mut PromptWidget,
        query: &str,
        result_path: &str,
        result_is_dir: bool,
    ) {
        let typed = format!("@{query}");
        pw.textarea.insert_str(&typed);
        let cursor = pw.textarea.text().len();
        pw.textarea.set_cursor(cursor);

        let ctx = crate::views::file_search::context::detect(pw.textarea.text(), cursor)
            .expect("text + cursor must form a valid @-context");

        pw.file_search
            .set_test_state(ctx, vec![fuzzy_result(result_path, result_is_dir)], 0);
    }

    #[test]
    fn right_arrow_with_no_popup_does_not_drill_down() {
        // No @-context, no popup. Right Arrow must NOT trigger acceptance —
        // it should fall through to normal cursor-movement handling.
        let mut pw = PromptWidget::new();
        pw.textarea.insert_str("hello");
        pw.textarea.set_cursor(0);

        assert!(!pw.file_search.is_visible());
        let event = pw.handle_key(&key!(Right).to_key_event());

        // Text must be unchanged (no path was inserted).
        assert_eq!(pw.textarea.text(), "hello");
        // The cursor should have advanced one character (normal Right behavior).
        assert_eq!(pw.textarea.cursor(), 1);
        // And the event type is whatever normal cursor movement produces — we
        // don't assert on it, just that it didn't blow up or insert text.
        let _ = event;
    }

    #[test]
    fn right_arrow_with_popup_visible_but_no_valid_selection_passes_through() {
        // Defensive case: popup is visible (results non-empty) but the
        // selection index is out of bounds. `file_search_has_selection`
        // should return false and Right Arrow must fall through to the
        // textarea's normal cursor-right behavior. In production the
        // selection invariant is maintained by FileSearchState (see the
        // comment in handle_file_search_key); this test locks in the gate.
        let mut pw = PromptWidget::new();
        let typed = "@src";
        pw.textarea.insert_str(typed);
        let cursor = pw.textarea.text().len();
        pw.textarea.set_cursor(cursor);
        let ctx = crate::views::file_search::context::detect(pw.textarea.text(), cursor)
            .expect("@-context must parse");
        // Seed one result but mark `selected = 1` (out of bounds) so
        // `file_search_has_selection` returns false even though the popup
        // is visible.
        pw.file_search
            .set_test_state(ctx, vec![fuzzy_result("src", true)], 1);
        assert!(pw.file_search.is_visible());
        assert!(!file_search_has_selection(&pw.file_search));

        pw.handle_key(&key!(Right).to_key_event());

        // No drill-down occurred: text is unchanged, no `/` was appended.
        assert_eq!(pw.textarea.text(), typed);
        // PassThrough: the textarea handled the key. Cursor was already
        // at end-of-line so it cannot advance further.
        assert_eq!(pw.textarea.cursor(), cursor);
    }

    #[test]
    fn right_arrow_at_end_of_line_with_no_popup_preserved() {
        // Cursor is already at end-of-line and there's no popup. Right Arrow
        // must be harmless and must not insert anything.
        let mut pw = PromptWidget::new();
        pw.textarea.insert_str("abc");
        let cursor = pw.textarea.text().len();
        pw.textarea.set_cursor(cursor);

        assert!(!pw.file_search.is_visible());
        pw.handle_key(&key!(Right).to_key_event());

        assert_eq!(pw.textarea.text(), "abc");
        assert_eq!(pw.textarea.cursor(), cursor);
    }

    #[test]
    fn right_arrow_drills_into_directory_result() {
        // User typed `@src`, the highlighted suggestion is the directory
        // `src`. Right Arrow replaces the path portion of the @-token with
        // the full selected path, WITHOUT a trailing `/`. The missing
        // slash is intentional: it keeps the context out of dir-mode so
        // the dropdown re-populates with both files and directories under
        // `src`. If the user wants to filter to directories only, they
        // can type `/` themselves.
        let mut pw = PromptWidget::new();
        seed_at_completion(&mut pw, "src", "src", true);
        assert!(pw.file_search.is_visible());

        let event = pw.handle_key(&key!(Right).to_key_event());
        assert_eq!(event, PromptEvent::Edited);

        assert_eq!(pw.textarea.text(), "@src");
        assert_eq!(pw.textarea.cursor(), "@src".len());
        // The text is plain (no atomic file-ref element wrapping it), so
        // the user can keep editing path segments freely.
        assert!(
            pw.textarea
                .elements()
                .iter()
                .all(|e| e.kind != KIND_FILE_REF),
            "directory drill-down must not create a file-ref element"
        );
        // Context kept alive so the dropdown can repopulate, and NOT in
        // dir-mode (since we deliberately did not append `/`).
        assert!(
            pw.file_search.context().is_some(),
            "directory drill-down must not clear the @-context"
        );
        assert!(
            !pw.file_search.is_dir_mode(),
            "drill-down must drop dir-mode so files are visible in the dropdown",
        );
    }

    #[test]
    fn right_arrow_in_dir_mode_drills_one_level_deeper() {
        // Already in dir mode (`@src/`). Highlighted suggestion is the
        // nested `src/foo` directory. Right Arrow replaces the @-token's
        // path portion with `src/foo` -- no trailing `/`. This drops the
        // user out of dir-mode, so the dropdown will then show files AND
        // dirs whose path matches `src/foo`. To keep filtering to dirs
        // only, the user types `/` themselves.
        let mut pw = PromptWidget::new();
        seed_at_completion(&mut pw, "src/", "src/foo", true);
        assert!(pw.file_search.is_visible());
        assert!(pw.file_search.is_dir_mode());

        pw.handle_key(&key!(Right).to_key_event());

        assert_eq!(pw.textarea.text(), "@src/foo");
        assert_eq!(pw.textarea.cursor(), "@src/foo".len());
        assert!(
            pw.file_search.context().is_some(),
            "dir-mode drill-down must not clear the @-context"
        );
        assert!(
            !pw.file_search.is_dir_mode(),
            "drill-down must drop dir-mode (no trailing `/`) so files are visible",
        );
    }

    #[test]
    fn right_arrow_drills_into_dir_with_space_in_name() {
        // Regression: drilling into `my dir` (spaced name) must keep the dropdown open.
        let mut pw = PromptWidget::new();
        seed_at_completion(&mut pw, "my", "my dir", true);
        assert!(pw.file_search.is_visible());

        let event = pw.handle_key(&key!(Right).to_key_event());
        assert_eq!(event, PromptEvent::Edited);

        assert_eq!(pw.textarea.text(), "@my dir");
        assert_eq!(pw.textarea.cursor(), "@my dir".len());
        let ctx = pw
            .file_search
            .context()
            .expect("drilling into a dir whose name has a space must not close the dropdown");
        assert_eq!(ctx.query, "my dir");
        // No trailing `/` → shows files and dirs.
        assert!(!pw.file_search.is_dir_mode());
    }

    #[test]
    fn right_arrow_drills_into_hidden_dir_with_space_in_name() {
        // Hidden mode + spaced name: drill must keep `!`, stay open, and treat
        // the space as path content (exercises `after_bang` end-to-end).
        let mut pw = PromptWidget::new();
        seed_at_completion(&mut pw, "!my", "my dir", true);
        assert!(pw.file_search.is_visible());

        pw.handle_key(&key!(Right).to_key_event());

        assert_eq!(pw.textarea.text(), "@!my dir");
        assert_eq!(pw.textarea.cursor(), "@!my dir".len());
        let ctx = pw
            .file_search
            .context()
            .expect("hidden-mode spaced drill must not close the dropdown");
        assert!(ctx.is_hidden_mode(), "the `!` marker must be preserved");
        assert_eq!(ctx.matcher_query(), "my dir");
    }

    #[test]
    fn tab_in_dir_mode_drills_into_nested_dir_with_space() {
        // Discriminating: the child's own segment carries the space (`sub dir`)
        // from a space-free parent, so no residual anchor masks it. Without the
        // Tab `set_drill_prefix`, the space terminates and the `.expect` panics.
        let mut pw = PromptWidget::new();
        seed_at_completion(&mut pw, "src/", "src/sub dir", true); // dir mode, no prior anchor
        assert!(pw.file_search.is_dir_mode());

        pw.handle_key(&key!(Tab).to_key_event());

        assert_eq!(pw.textarea.text(), "@src/sub dir/");
        let ctx = pw
            .file_search
            .context()
            .expect("Tab drill into a spaced child must stay open");
        assert_eq!(ctx.query, "src/sub dir/");
    }

    #[test]
    fn esc_clears_drill_anchor_after_spaced_drill() {
        // Esc → clear_context must also drop the anchor, else the closed
        // `@my dir` would re-detect as a context.
        let mut pw = PromptWidget::new();
        seed_at_completion(&mut pw, "my", "my dir", true);
        pw.handle_key(&key!(Right).to_key_event()); // → "@my dir", anchor "my dir"
        assert!(pw.file_search.context().is_some());

        pw.handle_key(&key!(Esc).to_key_event());
        assert!(pw.file_search.context().is_none());

        // With the anchor cleared, re-detecting `@my dir` must not resurrect it.
        pw.update_file_search_context();
        assert!(
            pw.file_search.context().is_none(),
            "Esc must clear the drill anchor so a stale prefix can't re-open the dropdown"
        );
    }

    #[test]
    fn leaving_at_mode_clears_drill_anchor() {
        // The `(Some, None)` leaving-@-mode arm must drop the anchor with the context.
        let mut pw = PromptWidget::new();
        seed_at_completion(&mut pw, "my", "my dir", true);
        pw.handle_key(&key!(Right).to_key_event()); // → "@my dir", anchor "my dir"
        assert!(pw.file_search.context().is_some());

        // Cursor before `@` → leaving @-mode.
        pw.textarea.set_cursor(0);
        pw.update_file_search_context();
        assert!(pw.file_search.context().is_none());

        // Anchor gone: re-detecting `@my dir` at EOL must not resurrect a context.
        pw.textarea.set_cursor(pw.textarea.text().len());
        pw.update_file_search_context();
        assert!(
            pw.file_search.context().is_none(),
            "leaving @-mode must clear the drill anchor, not just the context"
        );
    }

    #[test]
    fn drill_anchor_dropped_when_text_reverts_below_prefix() {
        // Undo/paste can revert a drilled path while the @-token stays alive
        // (`@my dir` → `@my`). The anchor must drop then, so reconstructing
        // `@my dir` in one edit terminates at the space (closed) like plain
        // typing, instead of silently re-matching the stale anchor.
        let mut pw = PromptWidget::new();
        seed_at_completion(&mut pw, "my", "my dir", true);
        pw.handle_key(&key!(Right).to_key_event()); // → "@my dir", anchor "my dir"
        assert_eq!(pw.textarea.text(), "@my dir");
        assert!(pw.file_search.context().is_some());

        // Revert to `@my` (as undo would); `@my` is still a live token.
        pw.textarea
            .replace_range(0..pw.textarea.text().len(), "@my");
        pw.textarea.set_cursor("@my".len());
        pw.update_file_search_context();
        assert!(pw.file_search.context().is_some());

        // Reconstruct `@my dir` in one edit: must close (stale anchor dropped).
        pw.textarea
            .replace_range(0..pw.textarea.text().len(), "@my dir");
        pw.textarea.set_cursor("@my dir".len());
        pw.update_file_search_context();
        assert!(
            pw.file_search.context().is_none(),
            "a stale drill anchor must not survive a revert and re-open the dropdown"
        );
    }

    #[test]
    fn tab_on_file_in_dir_mode_references_it() {
        // Files now show under a `path/` query; Tab/Enter on a file must
        // reference it as an atomic element, not append `/` to descend into it.
        let mut pw = PromptWidget::new();
        seed_at_completion(&mut pw, "src/", "src/main.rs", false); // file in dir-mode
        assert!(pw.file_search.is_dir_mode());

        pw.handle_key(&key!(Tab).to_key_event());

        assert_eq!(pw.textarea.text(), "@src/main.rs ");
        assert!(
            pw.file_search.context().is_none(),
            "referencing a file must dismiss the dropdown, not descend"
        );
    }

    #[test]
    fn right_arrow_in_hidden_mode_preserves_bang_prefix() {
        // Hidden mode (`@!src`) + dir result `src` + Right Arrow must
        // produce `@!src` (NOT `@src` with the `!` silently dropped). The
        // re-detected context must still be in hidden mode so further
        // fuzzy results continue to include hidden/gitignored entries.
        let mut pw = PromptWidget::new();
        seed_at_completion(&mut pw, "!src", "src", true);
        assert!(pw.file_search.is_visible());
        assert!(
            pw.file_search
                .context()
                .map(|c| c.is_hidden_mode())
                .unwrap_or(false),
            "test fixture must start in hidden mode"
        );

        pw.handle_key(&key!(Right).to_key_event());

        assert_eq!(pw.textarea.text(), "@!src");
        assert_eq!(pw.textarea.cursor(), "@!src".len());
        assert!(
            pw.file_search
                .context()
                .map(|c| c.is_hidden_mode())
                .unwrap_or(false),
            "re-detected context must still be in hidden mode (bang preserved)"
        );
    }

    #[test]
    fn right_arrow_on_file_behaves_like_tab() {
        // Highlighted suggestion is a file. There is nothing nested under
        // a file to drill into, so Right Arrow on a file is intentionally
        // identical to Tab: insert the file-ref element + a trailing space
        // and dismiss the dropdown. (The Right Arrow drill-down behavior
        // is reserved for directory results.)
        let mut pw = PromptWidget::new();
        seed_at_completion(&mut pw, "READ", "README.md", false);

        pw.handle_key(&key!(Right).to_key_event());

        assert_eq!(pw.textarea.text(), "@README.md ");
        assert_eq!(pw.textarea.cursor(), "@README.md ".len());
        assert!(
            pw.textarea
                .elements()
                .iter()
                .any(|e| e.kind == KIND_FILE_REF),
            "file selection should produce a file-ref element"
        );
        assert!(
            pw.file_search.context().is_none(),
            "file selection must clear the @-context"
        );
        assert!(!pw.file_search.is_visible());
    }

    #[test]
    fn right_arrow_on_file_matches_tab_exactly() {
        // Belt-and-suspenders: prove Right and Tab produce byte-identical
        // text + cursor + element output for a file selection, so they
        // cannot drift apart in the future.
        let mut pw_right = PromptWidget::new();
        seed_at_completion(&mut pw_right, "READ", "README.md", false);
        pw_right.handle_key(&key!(Right).to_key_event());

        let mut pw_tab = PromptWidget::new();
        seed_at_completion(&mut pw_tab, "READ", "README.md", false);
        pw_tab.handle_key(&key!(Tab).to_key_event());

        assert_eq!(pw_right.textarea.text(), pw_tab.textarea.text());
        assert_eq!(pw_right.textarea.cursor(), pw_tab.textarea.cursor());
        assert_eq!(
            pw_right
                .textarea
                .elements()
                .iter()
                .filter(|e| e.kind == KIND_FILE_REF)
                .count(),
            pw_tab
                .textarea
                .elements()
                .iter()
                .filter(|e| e.kind == KIND_FILE_REF)
                .count(),
        );
    }

    #[test]
    fn tab_still_appends_trailing_space_on_file() {
        // Regression guard: Tab's existing behavior — insert file ref + a
        // trailing space — must not be affected by the Right Arrow addition.
        let mut pw = PromptWidget::new();
        seed_at_completion(&mut pw, "READ", "README.md", false);

        pw.handle_key(&key!(Tab).to_key_event());

        assert_eq!(pw.textarea.text(), "@README.md ");
        assert_eq!(pw.textarea.cursor(), "@README.md ".len());
    }

    // ── Ghost text tests ────────────────────────────────────────────

    /// Chromeless prompt style for rendering tests (no borders, no prefix,
    /// no vpad — textarea starts at area origin).
    fn ghost_test_style() -> PromptStyle {
        PromptStyle {
            focused: true,
            show_prefix: false,
            vpad_top: 0,
            chrome: false,
            ..Default::default()
        }
    }

    /// Extract a substring from the buffer at the given row.
    fn buf_text_at(buf: &Buffer, x_start: u16, x_end: u16, y: u16) -> String {
        (x_start..x_end)
            .filter_map(|x| buf.cell((x, y)).map(|c| c.symbol().to_string()))
            .collect()
    }

    #[test]
    fn set_and_has_ghost_text() {
        let mut pw = PromptWidget::new();
        assert!(!pw.has_ghost_text());

        pw.set_ghost_text(Some("suggestion".into()));
        assert!(pw.has_ghost_text());

        pw.set_ghost_text(None);
        assert!(!pw.has_ghost_text());
    }

    #[test]
    fn has_ghost_text_false_for_empty_string() {
        let mut pw = PromptWidget::new();
        pw.set_ghost_text(Some(String::new()));
        assert!(!pw.has_ghost_text());
    }

    #[test]
    fn ghost_text_cleared_on_prompt_reset() {
        let mut pw = PromptWidget::new();
        pw.textarea.insert_str("hello");
        pw.set_ghost_text(Some("world".into()));
        assert!(pw.has_ghost_text());

        pw.set_text("");
        assert!(!pw.has_ghost_text());
    }

    #[test]
    fn ghost_text_cleared_on_any_set_text_swap() {
        // Non-empty swaps too — see the `set_text` invariant comment.
        let mut pw = PromptWidget::new();
        pw.textarea.insert_str("git st");
        pw.set_ghost_text(Some("atus".into()));
        let gen_before = pw.suggestions.generation();

        pw.set_text("make");
        assert!(!pw.has_ghost_text());
        assert!(
            pw.suggestions.generation() > gen_before,
            "in-flight fetches for the old draft must be discarded"
        );
    }

    #[test]
    fn ghost_text_renders_at_cursor_when_at_end() {
        let mut pw = PromptWidget::new();
        pw.textarea.insert_str("hello");
        pw.set_ghost_text(Some(" world".into()));

        let area = Rect::new(0, 0, 40, 1);
        let mut buf = Buffer::empty(area);
        pw.draw(&mut buf, area, None, &ghost_test_style(), None, None);

        // "hello" occupies x=0..5, ghost " world" at x=5..11
        assert_eq!(buf_text_at(&buf, 5, 11, 0), " world");

        // Verify ghost cells have the correct style (dimmed italic).
        let theme = Theme::current();
        let cell = buf.cell((5, 0)).unwrap();
        let cell_style = cell.style();
        assert_eq!(cell_style.fg, Some(theme.gray_dim),);
        assert!(cell_style.add_modifier.contains(Modifier::ITALIC));
    }

    #[test]
    fn ghost_text_not_rendered_when_cursor_mid_text() {
        let mut pw = PromptWidget::new();
        pw.textarea.insert_str("hello");
        pw.textarea.set_cursor(3);
        pw.set_ghost_text(Some("GHOST".into()));

        let area = Rect::new(0, 0, 40, 1);
        let mut buf = Buffer::empty(area);
        pw.draw(&mut buf, area, None, &ghost_test_style(), None, None);

        // No ghost text should appear anywhere after the typed text.
        assert_eq!(buf_text_at(&buf, 5, 10, 0).trim(), "");
    }

    #[test]
    fn ghost_text_suppressed_when_slash_active() {
        let mut pw = PromptWidget::new();
        pw.textarea.insert_str("hello");
        pw.set_ghost_text(Some("GHOST".into()));

        pw.slash_state.replace(crate::slash::SlashSnapshot {
            active: true,
            ..Default::default()
        });

        let area = Rect::new(0, 0, 40, 1);
        let mut buf = Buffer::empty(area);
        pw.draw(&mut buf, area, None, &ghost_test_style(), None, None);

        assert_eq!(buf_text_at(&buf, 5, 10, 0).trim(), "");
    }

    #[test]
    fn ghost_text_suppressed_when_slash_inline_ghost_present() {
        let mut pw = PromptWidget::new();
        pw.textarea.insert_str("hello");
        pw.set_ghost_text(Some("GHOST".into()));

        pw.slash_state.replace(crate::slash::SlashSnapshot {
            inline_ghost: Some(crate::slash::InlineGhost {
                text: "suffix".into(),
                token_range: 0..2,
                full_name: "cmd".into(),
            }),
            ..Default::default()
        });

        let area = Rect::new(0, 0, 40, 1);
        let mut buf = Buffer::empty(area);
        pw.draw(&mut buf, area, None, &ghost_test_style(), None, None);

        // The slash inline ghost may paint cells here, but our shell
        // suggestion "GHOST" must not appear.
        let ghost_region = buf_text_at(&buf, 5, 10, 0);
        assert!(
            !ghost_region.contains("GHOST"),
            "shell ghost text must be suppressed when slash inline ghost is present, got: {ghost_region:?}"
        );
    }

    #[test]
    fn ghost_text_truncated_to_available_width() {
        let mut pw = PromptWidget::new();
        pw.textarea.insert_str("hello");
        pw.set_ghost_text(Some(" world and more stuff".into()));

        // Width 12: "hello" takes 5, leaving 7 columns for ghost.
        let area = Rect::new(0, 0, 12, 1);
        let mut buf = Buffer::empty(area);
        pw.draw(&mut buf, area, None, &ghost_test_style(), None, None);

        // truncate_str(" world and more stuff", 7) -> " world…"
        let ghost = buf_text_at(&buf, 5, 12, 0);
        assert_eq!(ghost, " world…");
    }

    #[test]
    fn ghost_text_not_rendered_when_unfocused() {
        let mut pw = PromptWidget::new();
        pw.textarea.insert_str("hello");
        pw.set_ghost_text(Some(" world".into()));

        let mut style = ghost_test_style();
        style.focused = false;

        let area = Rect::new(0, 0, 40, 1);
        let mut buf = Buffer::empty(area);
        pw.draw(&mut buf, area, None, &style, None, None);

        // Unfocused -> cursor_pos is None -> ghost text skipped.
        assert_eq!(buf_text_at(&buf, 5, 11, 0).trim(), "");
    }

    #[test]
    fn ghost_text_renders_on_wrapped_line() {
        let mut pw = PromptWidget::new();
        // 10 'a's fill the first visual line at width 10, "bb" wraps.
        pw.textarea.insert_str("aaaaaaaaaabb");
        pw.set_ghost_text(Some("cc".into()));

        let area = Rect::new(0, 0, 10, 2);
        let mut buf = Buffer::empty(area);
        pw.draw(&mut buf, area, None, &ghost_test_style(), None, None);

        // Cursor at end of "bb" on row 1 -> ghost "cc" at (2, 1).
        assert_eq!(buf_text_at(&buf, 2, 4, 1), "cc");
    }

    #[test]
    fn ghost_text_with_zero_available_width() {
        let mut pw = PromptWidget::new();
        // Text fills the entire width — no room for ghost text.
        pw.textarea.insert_str("abcdefghij");
        pw.set_ghost_text(Some("GHOST".into()));

        let area = Rect::new(0, 0, 10, 1);
        let mut buf = Buffer::empty(area);
        pw.draw(&mut buf, area, None, &ghost_test_style(), None, None);

        // All 10 columns used by text — ghost has 0 avail, nothing rendered.
        assert_eq!(buf_text_at(&buf, 0, 10, 0), "abcdefghij");
    }

    #[test]
    fn ghost_text_with_unicode_content() {
        let mut pw = PromptWidget::new();
        pw.textarea.insert_str("hi");
        pw.set_ghost_text(Some("→ok".into()));

        let area = Rect::new(0, 0, 20, 1);
        let mut buf = Buffer::empty(area);
        pw.draw(&mut buf, area, None, &ghost_test_style(), None, None);

        // "hi" at x=0..2, ghost "→ok" at x=2..
        let ghost = buf_text_at(&buf, 2, 5, 0);
        assert!(ghost.contains('→'), "unicode ghost text should render");
        assert!(ghost.contains('o'), "unicode ghost text should render");
    }

    #[test]
    fn ghost_text_empty_string_not_rendered() {
        let mut pw = PromptWidget::new();
        pw.textarea.insert_str("hello");
        pw.set_ghost_text(Some(String::new()));

        let area = Rect::new(0, 0, 40, 1);
        let mut buf = Buffer::empty(area);
        pw.draw(&mut buf, area, None, &ghost_test_style(), None, None);

        assert_eq!(buf_text_at(&buf, 5, 10, 0).trim(), "");
    }

    // --- paint_slash_token_highlight (wrap-aware token painting) ---

    /// Sentinel highlight color — never produced by the textarea's own render.
    const TOKEN_FG: ratatui::style::Color = ratatui::style::Color::Rgb(9, 99, 199);

    /// Render `text` into a buffer at `area`, then paint `range` with
    /// [`TOKEN_FG`] the way `draw` does for recognized slash tokens.
    fn painted_textarea_buf(text: &str, area: Rect, range: std::ops::Range<usize>) -> Buffer {
        let mut ta = TextArea::new();
        ta.insert_str(text);
        ta.show_scrollbar = false;
        let mut state = TextAreaState::default();
        let mut buf = Buffer::empty(area);
        StatefulWidgetRef::render_ref(&(&ta), area, &mut buf, &mut state);
        paint_slash_token_highlight(&ta, state, area, &mut buf, range, TOKEN_FG);
        buf
    }

    #[test]
    fn slash_highlight_paints_all_rows_of_wrapped_token() {
        // Width 8 forces the 12-wide token to soft-wrap at the line end; the
        // continuation row's cells must be painted too. The text is only the
        // token, so every non-blank cell must carry the highlight.
        let area = Rect::new(0, 0, 8, 4);
        let token = "/pr-workflow";
        let buf = painted_textarea_buf(token, area, 0..token.len());

        let mut rows_with_content = 0;
        for y in area.y..area.y + area.height {
            let mut row_has_content = false;
            for x in area.x..area.x + area.width {
                let cell = buf.cell((x, y)).unwrap();
                if cell.symbol().trim().is_empty() {
                    continue;
                }
                row_has_content = true;
                assert_eq!(
                    cell.fg,
                    TOKEN_FG,
                    "token cell ({x},{y}) {:?} must be highlighted",
                    cell.symbol()
                );
            }
            if row_has_content {
                rows_with_content += 1;
            }
        }
        assert!(
            rows_with_content >= 2,
            "token must have wrapped across rows"
        );
    }

    #[test]
    fn slash_highlight_leaves_body_cells_unpainted() {
        // "xx"/"yy" share no characters with the token, so cells classify by
        // symbol alone — independent of where the wrap boundary lands.
        let area = Rect::new(0, 0, 8, 4);
        let buf = painted_textarea_buf("xx /pr-workflow yy", area, 3..15);

        let mut token_cells = 0;
        for y in area.y..area.y + area.height {
            for x in area.x..area.x + area.width {
                let cell = buf.cell((x, y)).unwrap();
                let sym = cell.symbol();
                if sym.trim().is_empty() {
                    continue;
                }
                if sym == "x" || sym == "y" {
                    assert_ne!(cell.fg, TOKEN_FG, "body cell ({x},{y}) must stay unpainted");
                } else {
                    assert_eq!(
                        cell.fg, TOKEN_FG,
                        "token cell ({x},{y}) must be highlighted"
                    );
                    token_cells += 1;
                }
            }
        }
        assert_eq!(
            token_cells, 12,
            "every token cell must be visible and painted"
        );
    }

    #[test]
    fn slash_highlight_paints_visible_tail_of_scrolled_token() {
        // Height-2 viewport with the cursor at the end: the token's first row
        // scrolls off the top, but its on-screen tail must still be painted.
        let area = Rect::new(0, 0, 8, 2);
        let buf = painted_textarea_buf("/pr-workflow abc", area, 0..12);

        let mut token_cells = 0;
        for y in area.y..area.y + area.height {
            for x in area.x..area.x + area.width {
                let cell = buf.cell((x, y)).unwrap();
                let sym = cell.symbol();
                if sym.trim().is_empty() {
                    continue;
                }
                if sym == "a" || sym == "b" || sym == "c" {
                    assert_ne!(cell.fg, TOKEN_FG, "body cell ({x},{y}) must stay unpainted");
                } else {
                    assert_eq!(
                        cell.fg, TOKEN_FG,
                        "token cell ({x},{y}) must be highlighted"
                    );
                    token_cells += 1;
                }
            }
        }
        assert!(token_cells > 0, "visible token tail must be painted");
    }

    #[test]
    fn ghost_text_ctrl_c_clears() {
        let mut pw = PromptWidget::new();
        pw.textarea.insert_str("hello");
        pw.set_ghost_text(Some("world".into()));
        assert!(pw.has_ghost_text());

        pw.handle_key(&key!('c', CONTROL).to_key_event());
        assert!(!pw.has_ghost_text());
    }

    // -- Ghost acceptance through PromptWidget --------------------------------

    #[test]
    fn accept_ghost_full_appends_to_textarea() {
        let mut pw = PromptWidget::new();
        pw.textarea.insert_str("git ");
        pw.set_ghost_text(Some("commit -m 'fix'".into()));

        let accepted = pw.accept_ghost(AcceptMode::Full);
        assert_eq!(accepted.as_deref(), Some("commit -m 'fix'"));
        assert_eq!(pw.text(), "git commit -m 'fix'");
        assert!(!pw.has_ghost_text());
    }

    #[test]
    fn accept_ghost_one_word_appends_partial() {
        let mut pw = PromptWidget::new();
        pw.textarea.insert_str("git ");
        pw.set_ghost_text(Some("commit --amend".into()));

        let accepted = pw.accept_ghost(AcceptMode::OneWord);
        assert_eq!(accepted.as_deref(), Some("commit"));
        assert_eq!(pw.text(), "git commit");
        assert!(pw.has_ghost_text());
    }

    #[test]
    fn accept_ghost_cursor_not_at_end_returns_none() {
        let mut pw = PromptWidget::new();
        pw.textarea.insert_str("hello");
        pw.textarea.set_cursor(2);
        pw.set_ghost_text(Some("world".into()));

        assert!(pw.accept_ghost(AcceptMode::Full).is_none());
        // Ghost not consumed — still present.
        assert!(pw.has_ghost_text());
        // Text unchanged.
        assert_eq!(pw.text(), "hello");
    }

    #[test]
    fn accept_ghost_empty_returns_none() {
        let mut pw = PromptWidget::new();
        pw.textarea.insert_str("hello");
        assert!(pw.accept_ghost(AcceptMode::Full).is_none());
    }

    #[test]
    fn clear_ghost_dismisses() {
        let mut pw = PromptWidget::new();
        pw.set_ghost_text(Some("suggestion".into()));
        assert!(pw.has_ghost_text());

        pw.clear_ghost();
        assert!(!pw.has_ghost_text());
    }

    // -- completion accept / splice application ---------------------------------

    /// Wire-shaped token item: whole-line `insert_text`, `token_text` span
    /// replacement (what a range-emitting shell sends).
    fn token_completion(
        line: &str,
        token: &str,
        range: std::ops::Range<usize>,
    ) -> crate::views::suggestion_controller::CompletionItemParsed {
        crate::views::suggestion_controller::CompletionItemParsed {
            display: token.to_owned(),
            description: String::new(),
            insert_text: line.to_owned(),
            source: SuggestionSource::PathExecutable,
            priority: 0,
            replace_range: Some(range),
            token_text: Some(token.to_owned()),
            truncated: false,
        }
    }

    #[test]
    fn apply_completion_splice_replaces_token_in_place() {
        let mut pw = PromptWidget::new();
        pw.textarea.insert_str("ls | gr");

        assert!(pw.apply_completion_splice(CompletionSplice::Token(5..7, "grep".into())));
        assert_eq!(pw.text(), "ls | grep");
        assert_eq!(pw.cursor(), "ls | grep".len());
    }

    /// Mid-text token: everything after the spliced span survives.
    #[test]
    fn apply_completion_splice_preserves_text_after_token() {
        let mut pw = PromptWidget::new();
        pw.textarea.insert_str("cat hel | wc -l");
        pw.textarea.set_cursor(7);

        assert!(pw.apply_completion_splice(CompletionSplice::Token(4..7, "hello.txt".into())));
        assert_eq!(pw.text(), "cat hello.txt | wc -l");
        assert_eq!(pw.cursor(), "cat hello.txt".len());
    }

    #[test]
    fn apply_completion_splice_whole_line_replaces_and_ends_cursor() {
        let mut pw = PromptWidget::new();
        pw.textarea.insert_str("git st");

        assert!(pw.apply_completion_splice(CompletionSplice::WholeLine(
            "git status --porcelain".into()
        )));
        assert_eq!(pw.text(), "git status --porcelain");
        assert_eq!(pw.cursor(), pw.text().len());
    }

    #[test]
    fn apply_completion_splice_stale_is_a_noop() {
        let mut pw = PromptWidget::new();
        pw.textarea.insert_str("echo something else");

        assert!(!pw.apply_completion_splice(CompletionSplice::Stale));
        assert_eq!(pw.text(), "echo something else");
    }

    /// The widget accept resolves against its own draft: a token item whose
    /// range still fits comes back as an in-place splice…
    #[test]
    fn dropdown_accept_resolves_against_current_draft() {
        let mut pw = PromptWidget::new();
        pw.textarea.insert_str("ls | gr");
        pw.suggestions.dropdown.request_text = "ls | gr".into();
        pw.suggestions.dropdown.items = vec![token_completion("ls | grep", "grep", 5..7)];

        assert_eq!(
            pw.completion_dropdown_accept(),
            Some(CompletionSplice::Token(5..7, "grep".into()))
        );
    }

    /// …and one whose range no longer fits resolves `Stale` — the draft is
    /// never clobbered by a stale token.
    #[test]
    fn dropdown_accept_stale_range_resolves_stale() {
        let mut pw = PromptWidget::new();
        pw.textarea.insert_str("echo something else");
        pw.suggestions.dropdown.request_text = "ls | gr".into();
        pw.suggestions.dropdown.items = vec![token_completion("ls | grep", "grep", 5..7)];

        assert_eq!(
            pw.completion_dropdown_accept(),
            Some(CompletionSplice::Stale)
        );
        assert_eq!(pw.text(), "echo something else");
    }

    // -- apply_completion_fill ---------------------------------------------

    /// The widget-level fill writes the decided LCP over the typed token and
    /// parks the cursor after it (the decision matrix lives in
    /// `suggestion_controller`'s `tab_decision` tests).
    #[test]
    fn apply_completion_fill_writes_and_positions_cursor() {
        let mut pw = PromptWidget::new();
        pw.textarea.insert_str("cat al");

        assert!(pw.apply_completion_fill(4..6, "alpha_"));
        assert_eq!(pw.text(), "cat alpha_");
        assert_eq!(pw.cursor(), "cat alpha_".len());
    }

    /// A completion range clipping an atomic element must no-op: the
    /// textarea expands any element overlap to the WHOLE element, so
    /// splicing a token range that ends inside a paste chip would replace
    /// the entire pasted block (data loss). Both write paths reject.
    #[test]
    fn completion_splice_and_fill_reject_range_clipping_paste_chip() {
        let mut pw = PromptWidget::new();
        pw.handle_paste("line one\nline two\nline three\nline four");
        let chip = pw.textarea.elements()[0].range.clone();
        let text_before = pw.textarea.text().to_owned();

        let clipping = chip.start..chip.start + 2;
        assert!(!pw.apply_completion_splice(CompletionSplice::Token(
            clipping.clone(),
            "src/main.rs".into()
        )));
        assert!(!pw.apply_completion_fill(clipping, "src/main.rs"));
        assert_eq!(pw.textarea.text(), text_before, "chip must survive intact");
        assert_eq!(pw.textarea.elements().len(), 1);

        // An abutting range (outside the element) still splices normally.
        pw.textarea.insert_str_at(chip.end, "tail");
        assert!(pw.apply_completion_splice(CompletionSplice::Token(
            chip.end..chip.end + 4,
            "notes.md".into()
        )));
        assert_eq!(pw.textarea.text(), format!("{text_before}notes.md"));
    }


    // -- Predicted-next-prompt suggestion through PromptWidget ----------------

    /// Widget with an active gate and a loaded suggestion — the state right
    /// after a turn ends with `echo.agent/suggestPrompt` resolved.
    fn widget_with_prompt_suggestion(text: &str) -> PromptWidget {
        let mut pw = PromptWidget::new();
        pw.prompt_suggestion_active = true;
        pw.prompt_suggestion.set_suggestion_for_test(text);
        pw
    }

    #[test]
    fn prompt_suggestion_ghost_on_empty_prompt() {
        let pw = widget_with_prompt_suggestion("run the tests");
        assert_eq!(pw.prompt_suggestion_ghost(), Some("run the tests"));
        assert!(pw.prompt_suggestion_visible());
    }

    #[test]
    fn prompt_suggestion_hidden_when_gate_closed() {
        let mut pw = widget_with_prompt_suggestion("run the tests");
        pw.prompt_suggestion_active = false;
        assert_eq!(pw.prompt_suggestion_ghost(), None);
    }

    #[test]
    fn prompt_suggestion_shrinks_with_matching_prefix_and_hides_on_divergence() {
        let mut pw = widget_with_prompt_suggestion("run the tests");
        pw.textarea.insert_str("run ");
        assert_eq!(pw.prompt_suggestion_ghost(), Some("the tests"));

        pw.textarea.insert_str("x");
        assert_eq!(pw.prompt_suggestion_ghost(), None);
    }

    #[test]
    fn prompt_suggestion_hidden_when_typed_out_fully() {
        let mut pw = widget_with_prompt_suggestion("run the tests");
        pw.textarea.insert_str("run the tests");
        assert_eq!(pw.prompt_suggestion_ghost(), None);
    }

    #[test]
    fn prompt_suggestion_hidden_when_cursor_not_at_end() {
        let mut pw = widget_with_prompt_suggestion("run the tests");
        pw.textarea.insert_str("run");
        pw.textarea.set_cursor(1);
        assert_eq!(pw.prompt_suggestion_ghost(), None);
    }

    #[test]
    fn prompt_suggestion_yields_to_shell_ghost() {
        let mut pw = widget_with_prompt_suggestion("run the tests");
        pw.set_ghost_text(Some("ls -la".into()));
        assert_eq!(pw.prompt_suggestion_ghost(), None);
    }

    #[test]
    fn accept_prompt_suggestion_inserts_remainder() {
        let mut pw = widget_with_prompt_suggestion("run the tests");
        pw.textarea.insert_str("run ");

        assert!(pw.accept_prompt_suggestion());
        assert_eq!(pw.text(), "run the tests");
        assert_eq!(pw.cursor(), pw.text().len());
        // Consumed: no ghost left, nothing re-offered on later edits.
        assert!(!pw.prompt_suggestion_visible());
        assert!(!pw.prompt_suggestion.has_suggestion());
    }

    #[test]
    fn accept_prompt_suggestion_noop_without_ghost() {
        let mut pw = PromptWidget::new();
        pw.prompt_suggestion_active = true;
        assert!(!pw.accept_prompt_suggestion());
        assert_eq!(pw.text(), "");
    }

    #[test]
    fn progressive_match_through_widget() {
        let mut pw = PromptWidget::new();
        pw.set_ghost_text(Some("hello".into()));
        pw.suggestions.set_last_request_text("");

        assert!(pw.try_progressive_match("h"));
        assert_eq!(pw.suggestions.ghost_text(), Some("ello"));

        assert!(pw.try_progressive_match("he"));
        assert_eq!(pw.suggestions.ghost_text(), Some("llo"));
    }

    #[test]
    fn inline_ghost_renders_on_second_line() {
        let mut pw = PromptWidget::new();
        pw.textarea.insert_str("hello\n/mo");

        pw.slash_state.replace(crate::slash::SlashSnapshot {
            inline_ghost: Some(crate::slash::InlineGhost {
                text: "del".into(),
                token_range: 6..9,
                full_name: "model".into(),
            }),
            ..Default::default()
        });

        let area = Rect::new(0, 0, 40, 3); // 3 rows tall
        let mut buf = Buffer::empty(area);
        pw.draw(&mut buf, area, None, &ghost_test_style(), None, None);

        // Ghost "del" should appear on row 1 (line 2).
        let ghost_row1 = buf_text_at(&buf, 3, 6, 1);
        assert_eq!(
            ghost_row1, "del",
            "ghost should render on line 2 (y=1), got: {ghost_row1:?}"
        );
    }

    #[test]
    #[serial_test::serial]
    fn teal_highlighting_on_second_line() {
        // Asserts the full-TUI accent color. The slash highlight now reads the
        // global `embedded` flag (monochrome when set), so pin it off and
        // serialize against the modal_window embedded test that toggles it.
        crate::views::modal_window::set_embedded(false);
        let mut pw = PromptWidget::new();
        pw.textarea.insert_str("hello\n/model");

        pw.slash_state.replace(crate::slash::SlashSnapshot {
            recognized_tokens: vec![std::ops::Range { start: 6, end: 12 }],
            ..Default::default()
        });

        let area = Rect::new(0, 0, 40, 3);
        let mut buf = Buffer::empty(area);
        pw.draw(&mut buf, area, None, &ghost_test_style(), None, None);

        // Verify the token text rendered on row 1 (line 2) at the correct position,
        // regardless of color support in the test environment.
        let token_text = buf_text_at(&buf, 0, 6, 1);
        assert_eq!(token_text, "/model", "token should render on row 1");

        // When the theme has color support, also verify teal foreground.
        let theme = crate::theme::Theme::current();
        if theme.accent_skill != ratatui::style::Color::Reset {
            for x in 0..6u16 {
                let cell = buf.cell((x, 1)).expect("cell exists");
                assert_eq!(
                    cell.fg, theme.accent_skill,
                    "col {x} on row 1 should be teal, got {:?}",
                    cell.fg
                );
            }
        }
    }

    #[test]
    fn progressive_match_mismatch_clears() {
        let mut pw = PromptWidget::new();
        pw.set_ghost_text(Some("hello".into()));
        pw.suggestions.set_last_request_text("");

        assert!(!pw.try_progressive_match("x"));
        assert!(!pw.has_ghost_text());
    }

    // ── Inline title on the top border ──────────────────────────────

    /// Bordered chrome style (the agent-view prompt shape) with an optional
    /// session title.
    fn title_test_style(title: Option<&str>) -> PromptStyle {
        PromptStyle {
            title: title.map(str::to_string),
            ..Default::default()
        }
    }

    /// Draw a bordered prompt into a fresh `width`×4 buffer and return it.
    fn draw_bordered(width: u16, style: &PromptStyle) -> Buffer {
        let mut pw = PromptWidget::new();
        let area = Rect::new(0, 0, width, 4);
        let mut buf = Buffer::empty(area);
        pw.draw(&mut buf, area, None, style, None, None);
        buf
    }

    #[test]
    fn title_renders_on_top_border_with_corners_intact() {
        let buf = draw_bordered(40, &title_test_style(Some("my session")));

        // ` my session ` is 12 cols, right-aligned ending 2 cells before ╮:
        // label at x 25..=36, dashes at 37..=38, corner at 39.
        assert_eq!(buf_text_at(&buf, 25, 37, 0), " my session ");
        assert_eq!(buf.cell((0, 0)).unwrap().symbol(), "\u{256d}");
        assert_eq!(buf.cell((39, 0)).unwrap().symbol(), "\u{256e}");
        assert_eq!(buf_text_at(&buf, 37, 39, 0), "\u{2500}\u{2500}");

        // Info-line treatment: dimmed secondary text on the prompt bg (same
        // blend as `render_info_line`'s model name), no bold, no inverse.
        let theme = Theme::current();
        let expected_fg =
            crate::render::color::blend_color(theme.bg_base, theme.text_secondary, 0.6)
                .unwrap_or(theme.gray);
        let title_cell = buf.cell((26, 0)).unwrap().style();
        assert_eq!(title_cell.fg, Some(expected_fg));
        assert_eq!(title_cell.bg, Some(theme.bg_base));
        assert!(!title_cell.add_modifier.contains(Modifier::BOLD));
        assert!(!title_cell.add_modifier.contains(Modifier::REVERSED));
        let border = buf.cell((1, 0)).unwrap().style();
        assert_eq!(border.bg, title_cell.bg);
        // Fg delta vs the border rule (like the bottom info line vs its ╰─╯
        // rule) — only meaningful with color support, same guard as the
        // slash-highlight test above (monochrome themes resolve to Reset).
        if theme.text_secondary != ratatui::style::Color::Reset {
            assert_ne!(border.fg, title_cell.fg);
        }
    }

    #[test]
    fn no_title_keeps_plain_top_border() {
        let buf = draw_bordered(40, &title_test_style(None));

        assert_eq!(buf.cell((0, 0)).unwrap().symbol(), "\u{256d}");
        assert_eq!(buf.cell((39, 0)).unwrap().symbol(), "\u{256e}");
        assert_eq!(buf_text_at(&buf, 1, 39, 0), "\u{2500}".repeat(38));
    }

    #[test]
    fn long_title_truncates_on_top_border_and_keeps_corners() {
        let long = "a".repeat(60);
        let buf = draw_bordered(40, &title_test_style(Some(&long)));

        // max_w = 40 - 6 = 34: label spans x 3..=36 with a trailing ellipsis.
        let row = buf_text_at(&buf, 0, 40, 0);
        assert!(row.contains('\u{2026}'), "expected ellipsis in: {row}");
        assert_eq!(buf.cell((0, 0)).unwrap().symbol(), "\u{256d}");
        assert_eq!(buf.cell((39, 0)).unwrap().symbol(), "\u{256e}");
        assert_eq!(buf_text_at(&buf, 1, 3, 0), "\u{2500}\u{2500}");
        assert_eq!(buf_text_at(&buf, 37, 39, 0), "\u{2500}\u{2500}");
    }

    #[test]
    fn blank_title_or_narrow_area_skips_border_title() {
        // Whitespace-only titles never paint on the border.
        let buf = draw_bordered(40, &title_test_style(Some("   ")));
        assert_eq!(buf_text_at(&buf, 1, 39, 0), "\u{2500}".repeat(38));

        // Too narrow for the min label width (max_w < 6): plain border, no panic.
        let buf = draw_bordered(11, &title_test_style(Some("my session")));
        assert_eq!(buf_text_at(&buf, 1, 10, 0), "\u{2500}".repeat(9));
    }

    // ── PromptBg::Panel chip remap (inline surfaces) ────────────────

    fn any_cell_with_bg(buf: &Buffer, bg: ratatui::style::Color) -> bool {
        let area = *buf.area();
        (area.top()..area.bottom())
            .any(|y| (area.left()..area.right()).any(|x| buf.cell((x, y)).is_some_and(|c| c.bg == bg)))
    }

    /// Inline surfaces repaint the chip's baked-in `paste_bg` to the panel
    /// background; without the flag the chip keeps its own background. Uses
    /// a sentinel panel color so the test holds under terminal-default,
    /// where every palette entry quantizes to `Color::Reset`.
    #[test]
    fn panel_bg_repaints_paste_chip_to_panel_bg() {
        let theme = Theme::current();
        let panel = ratatui::style::Color::Rgb(12, 34, 56);
        assert_ne!(theme.paste_bg, panel, "fixture: sentinel must differ");

        let mut pw = PromptWidget::new();
        pw.handle_paste("a\nb\nc\nd\ne"); // 5 lines >= chip threshold (4)
        let area = Rect::new(0, 0, 40, 2);

        let inline = PromptStyle::inline(panel);
        assert!(
            matches!(inline.bg, PromptBg::Panel(_)),
            "inline surfaces are panels"
        );
        let mut buf = Buffer::empty(area);
        pw.draw(&mut buf, area, None, &inline, None, None);
        assert!(
            !any_cell_with_bg(&buf, theme.paste_bg),
            "chip cells must be repainted to the panel background"
        );
        assert!(
            any_cell_with_bg(&buf, panel),
            "the chip row renders on the panel background"
        );

        let no_remap = PromptStyle {
            bg: PromptBg::Canvas(panel),
            ..PromptStyle::inline(panel)
        };
        let mut buf = Buffer::empty(area);
        pw.draw(&mut buf, area, None, &no_remap, None, None);
        assert!(
            any_cell_with_bg(&buf, theme.paste_bg),
            "without the remap the chip keeps its own background"
        );
    }

    /// The shift-selection chords must reach the textarea through the widget.
    #[test]
    fn shift_movement_chords_extend_selection_through_widget() {
        let mut pw = PromptWidget::new();
        pw.textarea.insert_str("alpha beta");
        pw.textarea.set_cursor(0);

        pw.handle_key(&KeyEvent::new(
            KeyCode::Right,
            KeyModifiers::ALT | KeyModifiers::SHIFT,
        ));
        assert_eq!(pw.textarea.selection_range(), Some(0..5), "word extend");

        pw.handle_key(&KeyEvent::new(
            KeyCode::Right,
            KeyModifiers::SUPER | KeyModifiers::SHIFT,
        ));
        assert_eq!(
            pw.textarea.selection_range(),
            Some(0..10),
            "line-end extend keeps the anchor"
        );

        pw.handle_key(&KeyEvent::new(KeyCode::Left, KeyModifiers::SHIFT));
        assert_eq!(pw.textarea.selection_range(), Some(0..9), "grapheme shrink");
    }

    /// A selection-only change must report Edited or the highlight goes stale on screen.
    #[test]
    fn selection_only_change_reports_edited() {
        let mut pw = PromptWidget::new();
        pw.textarea.insert_str("alpha beta");
        pw.textarea.set_cursor(0);
        pw.handle_key(&KeyEvent::new(
            KeyCode::Right,
            KeyModifiers::ALT | KeyModifiers::SHIFT,
        ));
        assert_eq!(pw.textarea.selection_range(), Some(0..5));

        // Cursor (head) is at 5; plain Right collapses to 5 — same text,
        // same cursor, selection cleared.
        let event = pw.handle_key(&KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
        assert_eq!(event, PromptEvent::Edited);
        assert_eq!(pw.textarea.selection_range(), None);
        assert_eq!(pw.textarea.cursor(), 5);
    }

    /// Esc drops the highlight but is never consumed — the same press still cancels.
    #[test]
    fn esc_with_selection_clears_highlight_and_declines() {
        let mut pw = PromptWidget::new();
        pw.textarea.insert_str("alpha beta");
        pw.textarea.set_selection(0, 5);
        let event = pw.handle_key(&key!(Esc).to_key_event());
        assert_eq!(event, PromptEvent::Ignored);
        assert_eq!(pw.textarea.selection_range(), None);
    }

    /// Modified Esc has no structural consumer: the widget consumes it via
    /// the textarea catch-all so the cleared highlight repaints (Edited).
    #[test]
    fn modified_esc_with_selection_clears_and_repaints() {
        let mut pw = PromptWidget::new();
        pw.textarea.insert_str("alpha beta");
        pw.textarea.set_selection(0, 5);
        let event = pw.handle_key(&KeyEvent::new(KeyCode::Esc, KeyModifiers::ALT));
        assert_eq!(event, PromptEvent::Edited);
        assert_eq!(pw.textarea.selection_range(), None);
    }

    /// Same contract for Tab: highlight drops and focus switches on one press.
    #[test]
    fn tab_with_selection_clears_highlight_and_declines() {
        let mut pw = PromptWidget::new();
        pw.textarea.insert_str("alpha beta");
        pw.textarea.set_selection(0, 5);
        let event = pw.handle_key(&key!(Tab).to_key_event());
        assert_eq!(event, PromptEvent::Ignored);
        assert_eq!(pw.textarea.selection_range(), None);
    }

    /// Shift/Alt+Enter replaces the selection like typing does.
    #[test]
    fn mod_enter_replaces_selection_with_newline() {
        let mut pw = PromptWidget::new();
        pw.textarea.insert_str("alpha beta");
        pw.textarea.set_selection(0, 5);
        pw.textarea.set_cursor(5);
        let event = pw.handle_key(&KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT));
        assert_eq!(event, PromptEvent::Edited);
        assert_eq!(pw.textarea.text(), "\n beta");
        assert_eq!(pw.textarea.selection_range(), None);
    }
