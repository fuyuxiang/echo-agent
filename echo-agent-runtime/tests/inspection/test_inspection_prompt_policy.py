from echo_agent.agent.inspection.policy import INSPECT_OK_SENTINEL, should_deliver
from echo_agent.agent.inspection.prompt import build_inspection_prompt
from echo_agent.agent.inspection.store import InspectItem


def test_prompt_includes_checks_and_last_conclusion():
    items = [InspectItem(name="官网", interval_sec=600, check="访问 x.com")]
    state = {"官网": {"last_checked_at": 100, "last_conclusion": "上次200正常"}}
    prompt = build_inspection_prompt(items, state)
    assert "官网" in prompt
    assert "访问 x.com" in prompt
    assert "上次200正常" in prompt
    assert INSPECT_OK_SENTINEL in prompt  # silence instruction present


def test_prompt_handles_no_last_conclusion():
    items = [InspectItem(name="新项", interval_sec=600, check="查 y")]
    prompt = build_inspection_prompt(items, {})
    assert "新项" in prompt and "查 y" in prompt


def test_should_deliver_silences_sentinel():
    assert should_deliver("INSPECT_OK") is False
    assert should_deliver("  INSPECT_OK  ") is False
    assert should_deliver("巡检完成，INSPECT_OK") is False  # contains sentinel


def test_should_deliver_silences_empty():
    assert should_deliver("") is False
    assert should_deliver("   ") is False


def test_should_deliver_passes_real_content():
    assert should_deliver("官网返回 500，可能宕机了") is True
