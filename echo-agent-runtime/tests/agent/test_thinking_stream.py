"""ThinkingStream pacing: how reasoning deltas become publishable snapshots."""

from __future__ import annotations

from echo_agent.agent.thinking_stream import MAX_CHARS, ThinkingStream


def _stream(**kw) -> tuple[ThinkingStream, list]:
    now = [0.0]
    s = ThinkingStream(clock=lambda: now[0], **kw)
    return s, now


def test_first_delta_publishes_immediately():
    s, _ = _stream(flush_chars=100, flush_interval=10.0)
    # Neither budget is met, but an opening frame that waits is a frame the user
    # never sees while the model is actually thinking.
    assert s.add("嗯") == "嗯"
    assert s.streamed is True


def test_small_deltas_are_held_until_the_char_budget_is_met():
    s, _ = _stream(flush_chars=10, flush_interval=10.0)
    s.add("0123456789")  # opening frame
    assert s.add("abc") is None
    assert s.add("defghi") is None
    assert s.add("j") == "0123456789abcdefghij"


def test_the_interval_releases_a_short_trailing_delta():
    s, now = _stream(flush_chars=1000, flush_interval=0.5)
    s.add("start")
    assert s.add("x") is None
    now[0] = 0.6
    assert s.add("y") == "startxy"


def test_snapshots_carry_the_whole_trace_not_the_tail():
    s, _ = _stream(flush_chars=3, flush_interval=10.0)
    s.add("aaa")
    second = s.add("bbb")
    # Self-contained frames mean a dropped one costs nothing downstream.
    assert second == "aaabbb"


def test_text_is_capped_and_further_deltas_stop_publishing():
    s, now = _stream(flush_chars=1, flush_interval=0.0)
    s.add("x" * (MAX_CHARS + 500))
    assert len(s.text) == MAX_CHARS
    now[0] = 99.0
    # Nothing visible can change past the cap, so repainting would be noise.
    assert s.add("more") is None
    assert len(s.text) == MAX_CHARS


def test_empty_delta_is_ignored():
    s, _ = _stream()
    assert s.add("") is None
    assert s.streamed is False


def test_never_streamed_when_no_delta_arrived():
    s, _ = _stream()
    assert s.streamed is False
    assert s.text == ""


def test_each_stream_gets_its_own_id():
    a, _ = _stream()
    b, _ = _stream()
    assert a.thinking_id != b.thinking_id
    assert a.thinking_id.startswith("th_")
