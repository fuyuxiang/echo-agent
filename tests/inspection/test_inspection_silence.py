from echo_agent.agent.inspection.policy import INSPECT_OK_SENTINEL, should_deliver
from echo_agent.agent.inspection.prompt import build_inspection_prompt
from echo_agent.agent.inspection.store import InspectItem


def test_prompt_instructs_silence_contract():
    """巡检 prompt 必须携带静音契约：无事回 sentinel 且不主动发消息。"""
    items = [InspectItem(name="x", interval_sec=60, check="check y")]
    prompt = build_inspection_prompt(items, {})
    assert INSPECT_OK_SENTINEL in prompt
    assert "不要主动发消息" in prompt or "不主动发消息" in prompt


def test_should_deliver_is_silence_fallback_hook():
    """should_deliver 作为代码侧兜底钩子的语义契约。"""
    assert should_deliver(INSPECT_OK_SENTINEL) is False
    assert should_deliver("发现异常：官网 500") is True
    assert should_deliver("") is False
