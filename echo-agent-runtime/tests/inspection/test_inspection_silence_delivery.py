"""Silence gate at the reply convergence point.

Inspection rounds (metadata _inspection=True) must be silenced when the agent's
final reply is empty or carries the INSPECT_OK sentinel. Normal rounds must be
completely unaffected, even if their text happens to contain "INSPECT_OK".
"""

from echo_agent.agent.loop import _should_publish_reply
from echo_agent.bus.events import InboundEvent


def _inspection_event() -> InboundEvent:
    return InboundEvent(channel="cron", chat_id="inspection", metadata={"_inspection": True})


def _normal_event() -> InboundEvent:
    return InboundEvent(channel="cli", chat_id="c1", metadata={})


def test_inspection_round_silenced_on_sentinel():
    """巡检轮次 + INSPECT_OK -> 不投递（静音）。"""
    assert _should_publish_reply(_inspection_event(), "INSPECT_OK") is False


def test_inspection_round_silenced_on_empty():
    """巡检轮次 + 空文本 -> 不投递。"""
    assert _should_publish_reply(_inspection_event(), "") is False


def test_inspection_round_delivers_real_alert():
    """巡检轮次 + 实质告警 -> 正常投递。"""
    assert _should_publish_reply(_inspection_event(), "发现异常：官网返回 500") is True


def test_normal_round_not_touched_even_with_sentinel_text():
    """普通轮次即便文本恰含 INSPECT_OK 也照常投递（关键回归保护）。"""
    assert _should_publish_reply(_normal_event(), "调试提示：sentinel 是 INSPECT_OK") is True


def test_normal_round_delivers_ordinary_text():
    """普通轮次普通文本照常投递。"""
    assert _should_publish_reply(_normal_event(), "你好") is True
