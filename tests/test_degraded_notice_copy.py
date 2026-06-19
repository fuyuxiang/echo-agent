from __future__ import annotations

from echo_agent.agent.degraded_notice import (
    GENERIC_FALLBACK_TEXT,
    REASON_APPROVAL_TIMEOUT,
    REASON_APPROVAL_UNAVAILABLE,
    REASON_REPEAT_BLOCKED,
    combine_notices,
    is_generic_fallback,
    notice_for,
)


def test_notice_approval_unavailable_is_chinese():
    text = notice_for(REASON_APPROVAL_UNAVAILABLE)
    assert "安全审批暂时不可用" in text
    assert text.startswith("⚠️")


def test_notice_approval_timeout_includes_tool_and_id():
    text = notice_for(REASON_APPROVAL_TIMEOUT, tool="exec", request_id="abc123")
    assert "exec" in text
    assert "abc123" in text
    assert "/approve" in text


def test_notice_repeat_blocked_is_chinese():
    text = notice_for(REASON_REPEAT_BLOCKED)
    assert "多次尝试" in text


def test_notice_unknown_reason_falls_back():
    assert notice_for("something_else") == GENERIC_FALLBACK_TEXT


def test_combine_dedupes_preserving_order():
    a = notice_for(REASON_APPROVAL_UNAVAILABLE)
    b = notice_for(REASON_REPEAT_BLOCKED)
    combined = combine_notices([a, b, a])
    assert combined.count(a) == 1
    assert combined.index(a) < combined.index(b)


def test_combine_empty_returns_empty():
    assert combine_notices([]) == ""


def test_is_generic_fallback_true_for_empty():
    assert is_generic_fallback("") is True
    assert is_generic_fallback("   ") is True


def test_is_generic_fallback_true_for_english_filler():
    assert is_generic_fallback(
        "I encountered an issue processing your request. Please try again or rephrase your question."
    ) is True


def test_is_generic_fallback_false_for_real_answer():
    assert is_generic_fallback("调研完成,结论是 ...") is False
