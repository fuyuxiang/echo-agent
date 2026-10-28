"""Tests for echo_agent/utils/text.py."""

from __future__ import annotations


from echo_agent.utils.text import (
    estimate_tokens,
    html_to_text,
    normalize_markdown,
    split_message,
    strip_thinking,
)


# ---------------------------------------------------------------------------
# split_message
# ---------------------------------------------------------------------------

class TestSplitMessage:
    def test_short_no_split(self):
        assert split_message("hello", max_len=100) == ["hello"]

    def test_split_at_paragraph_boundary(self):
        text = "A" * 90 + "\n\n" + "B" * 50
        chunks = split_message(text, max_len=100)
        assert len(chunks) >= 2
        assert chunks[0].endswith("A" * 10) or "A" in chunks[0]
        combined = "".join(c.strip() for c in chunks)
        assert "A" * 90 in combined
        assert "B" * 50 in combined

    def test_split_at_sentence_boundary(self):
        text = "Hello world. " * 20
        chunks = split_message(text, max_len=100)
        assert len(chunks) >= 2
        for chunk in chunks:
            assert len(chunk) <= 100

    def test_cjk_punctuation(self):
        text = "你好世界。" * 60
        chunks = split_message(text, max_len=100)
        assert len(chunks) >= 2
        for chunk in chunks:
            assert len(chunk) <= 100

    def test_hard_cut_no_boundary(self):
        text = "A" * 300
        chunks = split_message(text, max_len=100)
        assert len(chunks) >= 3
        for chunk in chunks:
            assert len(chunk) <= 100

    def test_min_chunk_ratio(self):
        # With a very early newline, min_chunk_ratio should prevent a tiny first chunk
        text = "Hi\n" + "X" * 200
        chunks = split_message(text, max_len=100, min_chunk_ratio=0.75)
        assert len(chunks) >= 2
        # First chunk should not be just "Hi" (too small relative to max_len)
        assert len(chunks[0]) >= 50 or len(text) <= 100

    def test_empty_string(self):
        assert split_message("") == [""]

    def test_exact_max_len(self):
        text = "A" * 100
        assert split_message(text, max_len=100) == [text]


# ---------------------------------------------------------------------------
# estimate_tokens
# ---------------------------------------------------------------------------

class TestEstimateTokens:
    def test_ascii(self):
        result = estimate_tokens("hello world")  # 11 ascii chars
        assert result == 11 // 4 + 1  # 3

    def test_cjk(self):
        text = "你好世界"  # 4 non-ascii chars
        result = estimate_tokens(text)
        assert result == 4 // 2 + 1  # 3

    def test_empty(self):
        assert estimate_tokens("") == 0

    def test_mixed(self):
        text = "hello你好"  # 5 ascii + 2 non-ascii
        result = estimate_tokens(text)
        assert result == 5 // 4 + 2 // 2 + 1  # 1 + 1 + 1 = 3


# ---------------------------------------------------------------------------
# strip_thinking
# ---------------------------------------------------------------------------

class TestStripThinking:
    def test_removes_think_block(self):
        text = "Before <think>internal reasoning</think> After"
        assert strip_thinking(text) == "Before  After"

    def test_multiline_think(self):
        text = "Start\n<think>\nline1\nline2\n</think>\nEnd"
        result = strip_thinking(text)
        assert "<think>" not in result
        assert "End" in result

    def test_no_think_block(self):
        assert strip_thinking("plain text") == "plain text"


# ---------------------------------------------------------------------------
# html_to_text
# ---------------------------------------------------------------------------

class TestHtmlToText:
    def test_br_tag(self):
        result = html_to_text("line1<br>line2")
        assert "line1\nline2" == result

    def test_p_tag(self):
        result = html_to_text("<p>paragraph one</p><p>paragraph two</p>")
        assert "paragraph one" in result
        assert "paragraph two" in result

    def test_entities(self):
        result = html_to_text("&amp; &lt; &gt; &quot; &#39;")
        assert "& < > \"" in result

    def test_nbsp(self):
        result = html_to_text("hello&nbsp;world")
        assert "hello world" == result

    def test_strips_tags(self):
        result = html_to_text("<div><span>text</span></div>")
        assert result == "text"


# ---------------------------------------------------------------------------
# normalize_markdown
# ---------------------------------------------------------------------------

class TestNormalizeMarkdown:
    def test_empty(self):
        assert normalize_markdown("") == ""

    def test_plain_strips_inline(self):
        text = "This is **bold** and *italic* and `code` here"
        assert normalize_markdown(text) == "This is bold and italic and code here"

    def test_keep_inline_preserves_markers(self):
        text = "This is **bold** and `code`"
        assert normalize_markdown(text, keep_inline=True) == text

    def test_link_plain(self):
        assert normalize_markdown("see [docs](http://x.com)") == "see docs"

    def test_link_keep_inline(self):
        # Links are kept verbatim when inline markers are preserved (QQ renders them).
        assert normalize_markdown("see [docs](http://x.com)", keep_inline=True) == (
            "see [docs](http://x.com)"
        )

    def test_heading_removed_both_modes(self):
        assert normalize_markdown("## Title\nbody") == "Title\nbody"
        assert normalize_markdown("## Title\nbody", keep_inline=True) == "Title\nbody"

    def test_heading_with_inline(self):
        assert normalize_markdown("### **Big** header") == "Big header"

    def test_hr_removed(self):
        assert normalize_markdown("above\n---\nbelow") == "above\nbelow"
        assert normalize_markdown("a\n***\nb") == "a\nb"

    def test_table_to_fields(self):
        text = (
            "| Name | Age |\n"
            "|------|-----|\n"
            "| Alice | 30 |\n"
            "| Bob | 25 |"
        )
        result = normalize_markdown(text)
        assert "Name: Alice" in result
        assert "Age: 30" in result
        assert "Name: Bob" in result
        assert "Age: 25" in result
        assert "|" not in result

    def test_table_colon_aligned_separator(self):
        text = (
            "| A | B |\n"
            "|:--|--:|\n"
            "| 1 | 2 |"
        )
        result = normalize_markdown(text)
        assert "A: 1" in result
        assert "B: 2" in result

    def test_table_short_separator(self):
        # A 1-dash separator is valid GFM and must be recognized.
        text = "| X | Y |\n|-|-|\n| a | b |"
        result = normalize_markdown(text)
        assert "X: a" in result
        assert "Y: b" in result

    def test_table_strips_inline_in_cells_plain(self):
        text = "| Name |\n|------|\n| **Alice** |"
        result = normalize_markdown(text)
        assert "Name: Alice" in result
        assert "*" not in result

    def test_table_keeps_inline_in_cells(self):
        text = "| Name |\n|------|\n| **Alice** |"
        result = normalize_markdown(text, keep_inline=True)
        assert "Name: **Alice**" in result

    def test_table_followed_by_text(self):
        text = "| A |\n|---|\n| 1 |\n\nafter table"
        result = normalize_markdown(text)
        assert "A: 1" in result
        assert "after table" in result

    def test_escaped_pipe_in_cell(self):
        text = "| Expr | Val |\n|------|-----|\n| a \\| b | 3 |"
        result = normalize_markdown(text)
        assert "Expr: a | b" in result
        assert "Val: 3" in result

    def test_code_fence_passthrough_plain(self):
        text = "```python\nx = **not bold**\n```"
        result = normalize_markdown(text)
        # Content inside the fence is untouched, even in plain mode.
        assert "x = **not bold**" in result
        assert "```python" in result

    def test_pipe_line_inside_fence_not_table(self):
        text = "```\n| not | a | table |\n```"
        result = normalize_markdown(text)
        assert "| not | a | table |" in result

    def test_pseudo_table_without_separator_preserved(self):
        # A single pipe row without a separator is not a real table; the
        # content must survive (stripped in plain mode), not be dropped.
        text = "| just | pipes |"
        result = normalize_markdown(text)
        assert "just" in result and "pipes" in result

    def test_collapses_blank_lines(self):
        text = "a\n\n\n\nb"
        assert normalize_markdown(text) == "a\n\nb"

    def test_realistic_mixed_document(self):
        text = (
            "## 报告\n"
            "这是 **重点** 内容\n\n"
            "| 项目 | 状态 |\n"
            "|------|------|\n"
            "| 部署 | 完成 |\n\n"
            "---\n"
            "结束"
        )
        result = normalize_markdown(text)
        assert "报告" in result
        assert "这是 重点 内容" in result
        assert "项目: 部署" in result
        assert "状态: 完成" in result
        assert "结束" in result
        assert "|" not in result
        assert "#" not in result
        assert "---" not in result

