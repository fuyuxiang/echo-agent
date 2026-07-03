import pytest

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


@pytest.mark.asyncio
async def test_transcript_heartbeat_updates_in_place():
    from textual.app import App
    from echo_agent.cli.tui.transcript import TranscriptView

    class T(App):
        def compose(self):
            yield TranscriptView()

    app = T()
    async with app.run_test():
        tv = app.query_one(TranscriptView)
        tv.heartbeat_line("in_1", "检索中")
        first = tv.heartbeat_line("in_1", "生成中")  # 同一 id 复用
        assert first.renderable_note == "生成中"
        assert tv.heartbeat_count == 1  # 未新增第二条


@pytest.mark.asyncio
async def test_transcript_tracks_last_memory_and_thinking():
    from textual.app import App
    from echo_agent.cli.tui.transcript import TranscriptView

    class T(App):
        def compose(self):
            yield TranscriptView()

    app = T()
    async with app.run_test():
        tv = app.query_one(TranscriptView)
        assert tv.last_memory_block() is None
        assert tv.last_thinking_block() is None
        mem = tv.add_cognitive(CogEvent("memory_recalled", "e1", "in_1", {}, "召回"))
        think = tv.add_cognitive(CogEvent("thinking", "e2", "in_1", {}, "思考"))
        tv.add_user("你好")
        tv.start_reply()
        tv.add_approval("req1", "shell", {"cmd": "ls"}, "低风险")
        assert tv.last_memory_block() is mem
        assert tv.last_thinking_block() is think


@pytest.mark.asyncio
async def test_prompt_input_enter_submits_shift_enter_newlines():
    from textual.app import App
    from echo_agent.cli.tui.prompt_input import PromptInput

    submitted: list[str] = []

    class T(App):
        def compose(self):
            yield PromptInput()

        def on_prompt_input_submitted(self, msg: PromptInput.Submitted) -> None:
            submitted.append(msg.text)

    app = T()
    async with app.run_test() as pilot:
        pi = app.query_one(PromptInput)
        pi.focus()
        # Shift+Enter inserts a newline instead of submitting.
        await pilot.press("a")
        await pilot.press("shift+enter")
        await pilot.press("b")
        assert pi.text == "a\nb"
        assert submitted == []
        # Enter submits the (stripped) text and clears the field.
        await pilot.press("enter")
        await pilot.pause()
        assert submitted == ["a\nb"]
        assert pi.text == ""
        # Enter on empty/whitespace input does nothing.
        await pilot.press("enter")
        await pilot.pause()
        assert submitted == ["a\nb"]


@pytest.mark.asyncio
async def test_status_bar_setters_update_render():
    from textual.app import App
    from echo_agent.cli.tui.status_bar import StatusBar

    class T(App):
        def compose(self):
            yield StatusBar()

    app = T()
    async with app.run_test():
        sb = app.query_one(StatusBar)
        sb.set_session("sess_1")
        sb.set_model("opus")
        sb.set_cost(0.1234)
        sb.set_connection(False)
        text = str(sb.render())
        assert "sess_1" in text
        assert "opus" in text
        assert "0.1234" in text
        assert "断开" in text
        sb.set_connection(True)
        assert "连接" in str(sb.render())
