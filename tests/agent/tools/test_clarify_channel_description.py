"""clarify 的工具描述必须与渠道的真实渲染能力一致。

回归背景：描述曾无条件宣称 options 会渲染成可点击 picker，模型据此在
微信渠道推了 4 个选项，用户只看到一行文字、无法点选。
"""
from echo_agent.agent.clarify_manager import ClarifyManager
from echo_agent.agent.tools.clarify import CLI_CHANNEL, ClarifyTool
from echo_agent.agent.tools.registry import ToolRegistry


def _clarify_schema(channel):
    registry = ToolRegistry()
    registry.register(ClarifyTool(ClarifyManager()))
    defs = registry.get_definitions(channel=channel)
    return next(d for d in defs if d["function"]["name"] == "clarify")


def test_cli_channel_keeps_picker_wording():
    desc = _clarify_schema(CLI_CHANNEL)["function"]["description"]
    assert "picker" in desc


def test_im_channel_drops_picker_wording():
    desc = _clarify_schema("weixin:group")["function"]["description"]
    assert "picker" not in desc
    assert "click" not in desc


def test_im_channel_description_states_the_real_contract():
    desc = _clarify_schema("weixin:group")["function"]["description"]
    assert "letter" in desc.lower()


def test_no_channel_defaults_to_text_wording():
    # 未知渠道按最保守的能力处理，不能默认宣称有 picker。
    desc = _clarify_schema(None)["function"]["description"]
    assert "picker" not in desc


def test_render_text_uses_the_same_letters_as_answer_binding():
    # AgentLoop._maybe_bind_im_clarify_answer renders the remembered options as
    # "A. x；B. y" when quoting them back to the model. What the user is shown
    # must use the same labels, or their "A" answers a question labelled "1".
    rendered = ClarifyTool._render_text("选哪个？", ["先跑测试", "先改代码"])
    assert "A. 先跑测试" in rendered
    assert "B. 先改代码" in rendered
    assert "1." not in rendered


def test_render_text_tells_the_user_how_to_answer():
    rendered = ClarifyTool._render_text("选哪个？", ["甲", "乙"])
    assert "回复" in rendered


def test_render_text_without_options_is_just_the_question():
    assert ClarifyTool._render_text("你想先做什么？", []) == "你想先做什么？"
