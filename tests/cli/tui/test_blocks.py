from echo_agent.cli.tui.protocol import CogEvent
from echo_agent.cli.tui.blocks import CognitiveBlock, ApprovalBlock, UserTurn


def _ev(cog_type, data, summary):
    return CogEvent(cog_type, "evt_1", "in_1", data, summary)


def test_cognitive_block_summary_and_toggle():
    ev = _ev("memory_recalled",
             {"items": [{"content": "喜欢深色", "source": "user_stated", "score": 0.9}]},
             "召回 1 条记忆")
    b = CognitiveBlock(ev)
    assert b.expanded is False
    assert "召回 1 条记忆" in b.render_summary()
    b.toggle()
    assert b.expanded is True
    detail = b.render_detail()
    assert "喜欢深色" in detail
    assert "user_stated" in detail


def test_user_turn_prefix():
    assert UserTurn("你好").text_content == "❯ 你好"


def test_approval_block_marks_decision():
    a = ApprovalBlock("req1", "shell", {"cmd": "rm x"}, "EXEC 高风险")
    assert a.decision is None
    a.mark("approve")
    assert a.decision == "approve"
