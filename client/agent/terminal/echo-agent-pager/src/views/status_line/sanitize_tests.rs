use super::super::painted_line_width;
use super::*;

#[test]
fn runaway_is_capped_in_lines_and_characters_but_keeps_its_prefix() {
    let wide = SanitizedText::new(&"x".repeat(64 * 1024));
    let tall = SanitizedText::new(&"x\n".repeat(20));

    assert_eq!(tall.line_count(), MAX_STATUS_LINE_LINES);
    assert_eq!(wide.line_count(), 1);
    // Every character here is one column, so this is the character cap.
    assert!(painted_line_width(&wide.lines[0]) <= MAX_SANITIZED_CHARS);
    // A `<=` bound on its own is satisfied by an empty line.
    assert!(wide.lines[0].spans[0].content.starts_with("xxx"));
}

/// A label, the input, the text that survives, and the link columns.
type ScanCase = (
    &'static str,
    &'static str,
    &'static str,
    &'static [(u16, u16, &'static str)],
);

#[test]
fn scanner_strips_escapes_and_records_link_columns() {
    let cases: &[ScanCase] = &[
        (
            "a bel-terminated link after plain text",
            "[EchoAgent] \x1b]8;;https://example.com/repo\x07repo\x1b]8;;\x07",
            "[EchoAgent] repo",
            &[(7, 11, "https://example.com/repo")],
        ),
        (
            "an st-terminated link whose colour paints no columns",
            "\x1b]8;;https://echo-agent.invalid\x1b\\\x1b[32mecho.agent\x1b[0m\x1b]8;;\x1b\\",
            "\x1b[32mecho.agent\x1b[0m",
            &[(0, 4, "https://echo-agent.invalid")],
        ),
        (
            "two links on one line",
            "\x1b]8;;https://a.example\x07aa\x1b]8;;https://b.example\x07bb\x1b]8;;\x07",
            "aabb",
            &[(0, 2, "https://a.example"), (2, 4, "https://b.example")],
        ),
        (
            "an emoji ahead of the link is two columns wide, not one",
            "\u{26a0}\u{fe0f}\x1b]8;;https://echo-agent.invalid\x07ok\x1b]8;;\x07",
            "\u{26a0}\u{fe0f}ok",
            &[(2, 4, "https://echo-agent.invalid")],
        ),
        (
            // `tput sgr0` emits `ESC ( B`, which paints a literal `(B` if kept
            // and shifts the link right if counted.
            "a charset escape is swallowed and takes no columns",
            "\x1b(B\x1b]8;;https://echo-agent.invalid\x07echo.agent\x1b]8;;\x07",
            "echo.agent",
            &[(0, 4, "https://echo-agent.invalid")],
        ),
        (
            "an erase csi never reaches the parser, the colour does",
            "\x1b[2Kx\x1b[31mred\x1b[0m",
            "x\x1b[31mred\x1b[0m",
            &[],
        ),
        (
            // Ends on a non-alphabetic final byte: stopping at the next letter
            // would swallow the text after it.
            "a csi ending in ~ is dropped whole and paints no columns",
            "\x1b[3~\x1b]8;;https://echo-agent.invalid\x07ok\x1b]8;;\x07",
            "ok",
            &[(0, 2, "https://echo-agent.invalid")],
        ),
    ];

    for &(what, input, want_clean, want_links) in cases {
        let (clean, links) = extract_osc8_links(input);
        let spans: Vec<_> = links
            .iter()
            .map(|l| (l.col_start, l.col_end, &*l.url))
            .collect();
        assert_eq!(clean, want_clean, "{what}");
        assert_eq!(spans, want_links, "{what}");
    }
}

#[test]
fn link_on_the_second_line_is_measured_from_that_line() {
    let text = SanitizedText::new(
        "first\nsee \x1b]8;;https://echo-agent.invalid\x07echo.agent\x1b]8;;\x07",
    );
    let link = &text.links[0];

    assert_eq!(text.line_count(), 2);
    // Column 4 of the second line, not column 10 of the whole text.
    assert_eq!(
        (link.line, link.col_start, link.col_end, &*link.url),
        (1, 4, 8, "https://echo-agent.invalid")
    );
}

#[test]
fn link_is_dropped_with_the_line_the_cap_cuts() {
    let mut input = "x\n".repeat(MAX_STATUS_LINE_LINES as usize);
    input.push_str("\x1b]8;;https://echo-agent.invalid\x07late\x1b]8;;\x07");
    let (_, scanned) = extract_osc8_links(&input);
    let text = SanitizedText::new(&input);

    // The scanner found it one line past the last line kept.
    assert_eq!(scanned[0].line, MAX_STATUS_LINE_LINES);
    assert_eq!(text.line_count(), MAX_STATUS_LINE_LINES);
    assert!(text.links.is_empty());
}

#[test]
fn a_web_link_with_no_host_is_not_a_link() {
    // The shared gate allows these on the scheme alone; they open a browser on
    // nothing.
    for hostless in ["http://", "https://", "https:// "] {
        let input = format!("\x1b]8;;{hostless}\x07nowhere\x1b]8;;\x07");
        let (clean, links) = extract_osc8_links(&input);
        assert_eq!(clean, "nowhere", "the text still paints: {hostless:?}");
        assert!(links.is_empty(), "{hostless:?} became a link");
    }
}

#[test]
fn script_cannot_smuggle_a_scheme_past_the_link_allowlist() {
    for hostile in [
        "file:///etc/passwd",
        "vscode://file/etc/passwd",
        "javascript:alert(1)",
        "smb://attacker.example/share",
    ] {
        let input = format!("\x1b]8;;{hostile}\x07click\x1b]8;;\x07");
        let (clean, links) = extract_osc8_links(&input);
        assert_eq!(clean, "click", "{hostile}");
        assert!(links.is_empty(), "{hostile}");
    }

    // The allowlist's third scheme, and the control for the loop above: a gate
    // that dropped every link would pass it without this.
    let (clean, links) = extract_osc8_links("\x1b]8;;mailto:a@b.example\x07mail\x1b]8;;\x07");
    assert_eq!(clean, "mail");
    assert_eq!(
        links.iter().map(|l| &*l.url).collect::<Vec<_>>(),
        ["mailto:a@b.example"]
    );

    // The check trims, and falls back to a bare `://` test when `Url::parse`
    // refuses, so the validated and stored strings must be the same one.
    let (_, smuggled) = extract_osc8_links(
        "\x1b]8;;https://echo-agent.invalid\x1b]52;c;cHduZWQ=\x07ok\x1b]8;;\x07",
    );
    assert!(smuggled.is_empty());
}
