#![no_main]

use libfuzzer_sys::fuzz_target;
use echo_agent_markdown::style::test_style::STYLE;
use echo_agent_markdown::{render_markdown_ratatui_full, StreamingMarkdownRenderer};

const CHUNK_SIZES: [usize; 3] = [1, 16, 32];

fuzz_target!(|data: &[u8]| {
    let Ok(s) = std::str::from_utf8(data) else {
        return;
    };

    // Full render: pretty / non-pretty
    for pretty in [true, false] {
        let _ = render_markdown_ratatui_full(s, STYLE, pretty, None);
    }

    // Streaming with rotating chunk sizes: pretty / non-pretty
    for pretty in [true, false] {
        let mut r = StreamingMarkdownRenderer::new(STYLE, pretty);
        let mut pos = 0;
        let mut ci = 0;
        while pos < s.len() {
            let mut end = (pos + CHUNK_SIZES[ci]).min(s.len());
            // snap to char boundary
            while end < s.len() && !s.is_char_boundary(end) {
                end += 1;
            }
            r.push_and_render(&s[pos..end], None);
            pos = end;
            ci = (ci + 1) % CHUNK_SIZES.len();
        }
    }
});
