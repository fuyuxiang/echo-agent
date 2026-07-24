"""TurnRegistry unit tests — the P0 fix for the TUI's multi-event state model.

These pin the exact scenarios the old single-`_active_event_id` string got
wrong: the approval reply's accepted frame overwriting the active id, a queued
second turn clobbering the interrupt target, and control replies stopping the
original turn's timer.
"""

from __future__ import annotations

from echo_agent.cli.tui.turns import TurnRegistry


def test_single_turn_lifecycle():
    r = TurnRegistry()
    r.note_send("primary")
    assert r.has_active_primary is True          # active from submit
    assert r.active_turn_id == ""                 # id unknown until accepted
    r.on_accepted("turn-1")
    assert r.active_turn_id == "turn-1"
    assert r.has_active_primary is True
    assert r.on_final("turn-1") == "primary"
    assert r.has_active_primary is False
    assert r.active_turn_id == ""


def test_approval_reply_does_not_clobber_active_turn():
    """The core P0: while a primary turn is parked in approval, the /approve
    control reply gets its own accepted frame. It must NOT become the interrupt
    target, and its final ack must NOT end the primary turn."""
    r = TurnRegistry()
    r.note_send("primary")
    r.on_accepted("turn-1")
    # User approves → control send + its accepted frame (new event id).
    r.note_send("control")
    assert r.on_accepted("approve-evt") == "control"
    # Interrupt still targets the original turn, not the approval reply.
    assert r.active_turn_id == "turn-1"
    assert r.has_active_primary is True
    # The approval ack reply lands — must not stop the primary turn.
    assert r.on_final("approve-evt") == "control"
    assert r.has_active_primary is True
    assert r.active_turn_id == "turn-1"
    # Only the original turn's own final ends it.
    assert r.on_final("turn-1") == "primary"
    assert r.has_active_primary is False


def test_queued_second_turn_does_not_steal_interrupt_target():
    """A second turn submitted while the first runs must queue behind it; Ctrl+C
    still targets the running (oldest) turn, not the queued one."""
    r = TurnRegistry()
    r.note_send("primary")
    r.on_accepted("turn-1")
    r.note_send("primary")
    r.on_accepted("turn-2")
    assert r.active_turn_id == "turn-1"           # oldest = running
    assert r.queued_count == 1
    r.on_final("turn-1")                           # running turn completes
    assert r.active_turn_id == "turn-2"           # queued turn promoted
    assert r.queued_count == 0
    assert r.has_active_primary is True


def test_clarify_reply_is_control():
    r = TurnRegistry()
    r.note_send("primary")
    r.on_accepted("turn-1")
    r.note_send("control")                         # clarify answer
    assert r.on_accepted("clarify-evt") == "control"
    assert r.active_turn_id == "turn-1"
    r.on_final("clarify-evt")
    assert r.has_active_primary is True


def test_terminal_error_clears_primary_state():
    r = TurnRegistry()
    r.note_send("primary")
    r.on_accepted("turn-1")
    r.on_terminal_error()
    assert r.has_active_primary is False
    assert r.active_turn_id == ""


def test_accepted_without_prior_send_defaults_primary():
    """A stray accept (older gateway, no matching send) is treated as a primary
    turn so it never silently vanishes."""
    r = TurnRegistry()
    assert r.on_accepted("turn-x") == "primary"
    assert r.active_turn_id == "turn-x"


def test_unknown_final_is_not_fatal():
    r = TurnRegistry()
    r.note_send("primary")
    r.on_accepted("turn-1")
    assert r.on_final("some-other-id") == "unknown"
    # The real turn is untouched.
    assert r.active_turn_id == "turn-1"
    assert r.has_active_primary is True


def test_reset_on_reconnect_clears_all_inflight_state():
    # 真 bug 回归：一个 turn 已被 accepted，随后 socket 断开、其 final 在断连
    # 期间被丢弃永远到不了 CLI。若不清理，_primary 残留 → has_active_primary
    # 永久为真 → 之后每次提交都被 queue-guard 拦到重启为止。
    r = TurnRegistry()
    r.note_send("primary")
    r.on_accepted("turn-1")
    assert r.has_active_primary is True
    r.reset_on_reconnect()
    assert r.has_active_primary is False
    assert r.active_turn_id == ""


def test_reset_on_reconnect_clears_pending_before_accepted():
    # 另一可达序列：发出后 accepted 尚未到达就断线，_pending_primary 卡在 1。
    r = TurnRegistry()
    r.note_send("primary")
    assert r.has_active_primary is True  # 靠 _pending_primary
    r.reset_on_reconnect()
    assert r.has_active_primary is False


def test_reset_on_reconnect_clears_control_and_fifo():
    # 跨断线的 control（approve/deny/clarify）ack 同样不可恢复，一并清掉；
    # 未匹配的 pending_kinds 也清空，避免下条 accepted 被错位分类。
    r = TurnRegistry()
    r.note_send("control")
    r.on_accepted("ctrl-1")
    r.note_send("primary")  # 尚未 accepted 的悬空 kind
    r.reset_on_reconnect()
    assert r.has_active_primary is False
    # FIFO 已清空：新一轮的 accepted 按默认 primary 处理，不受旧残留影响。
    assert r.on_accepted("fresh") == "primary"
    assert r.active_turn_id == "fresh"
