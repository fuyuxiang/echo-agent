import pytest

from echo_agent.cli.tui.protocol import CogEvent
from echo_agent.cli.tui.blocks import CognitiveBlock, ApprovalBlock, UserTurn, ChoiceBlock


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


def test_agent_reply_streaming_plain_final_markdown():
    """Streaming tokens stay plain (escaped) text; the finished reply renders
    as a markdown grid. Guards the split that fixes un-rendered markdown."""
    from rich.markdown import Markdown
    from rich.table import Table
    from echo_agent.cli.tui.blocks import AgentReply

    r = AgentReply.__new__(AgentReply)
    r._buf = ""
    captured = {}
    r.update = lambda content, **kw: captured.__setitem__("c", content)

    r.append_token("**x**")
    # Streaming path: markdown is NOT parsed — raw ``**`` survive as plain text.
    assert isinstance(captured["c"], str)
    assert "**x**" in captured["c"]

    # No app attached: _bullet_color falls back to its fixed hue.
    r.set_markdown("# 标题\n\n**加粗**")
    grid = captured["c"]
    # Final path: a two-column grid whose body column is a Markdown visual.
    assert isinstance(grid, Table)
    cells = [c for col in grid.columns for c in col.cells]
    assert any(isinstance(c, Markdown) for c in cells)
    assert r._buf == "# 标题\n\n**加粗**"


def test_agent_reply_set_final_stays_plain():
    """set_final is the status-line path (heartbeat/error): plain text, never
    markdown, so hand-built Rich markup keeps working."""
    from echo_agent.cli.tui.blocks import AgentReply

    r = AgentReply.__new__(AgentReply)
    r._buf = ""
    captured = {}
    r.update = lambda content, **kw: captured.setdefault("c", content)
    r.set_final("⏳ 生成中")
    assert isinstance(captured["c"], str)
    assert "⏳ 生成中" in captured["c"]


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
async def test_transcript_auto_follows_new_content():
    """Mounting content past the viewport keeps the view scrolled to the bottom
    (anchored), so long replies stay visible instead of growing below the fold."""
    from textual.app import App
    from textual.widgets import Static
    from echo_agent.cli.tui.transcript import TranscriptView

    class T(App):
        def compose(self):
            yield TranscriptView()

    app = T()
    async with app.run_test(size=(40, 6)) as pilot:
        tv = app.query_one(TranscriptView)
        for i in range(20):
            tv.mount(Static(f"line {i}"))
        await pilot.pause()
        assert tv.max_scroll_y > 0  # content actually overflows the viewport
        assert round(tv.scroll_y) == round(tv.max_scroll_y)
        assert tv.is_vertical_scroll_end


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
        assert sb._model == "opus"  # 存下但不渲染
        assert "0.1234" in text
        assert "断开" in text
        sb.set_connection(True)
        assert "连接" in str(sb.render())


def test_choice_block_renders_numbered_options():
    b = ChoiceBlock("c1", "用哪个方案?", ["方案A", "方案B", "方案C"])
    body = b.render_body()
    assert "用哪个方案?" in body
    assert "1. 方案A" in body
    assert "2. 方案B" in body
    assert "3. 方案C" in body
    assert "按数字" in body  # 操作提示存在


def test_choice_block_empty_options_is_free_input_only():
    b = ChoiceBlock("c2", "请描述你的需求", [])
    body = b.render_body()
    assert "请描述你的需求" in body
    assert "1." not in body
    assert "请输入回答" in body


def test_choice_block_number_mapping():
    b = ChoiceBlock("c1", "q", ["A", "B"])
    assert b.option_for_number(1) == "A"
    assert b.option_for_number(2) == "B"
    assert b.option_for_number(3) is None
    assert b.option_for_number(0) is None


def test_choice_block_highlight_move_and_clamp():
    b = ChoiceBlock("c1", "q", ["A", "B", "C"])
    assert b.highlighted == 0
    b.move(1)
    assert b.highlighted == 1
    assert b.highlighted_option() == "B"
    b.move(-5)                    # 下越界钳制
    assert b.highlighted == 0
    b.move(99)                    # 上越界钳制
    assert b.highlighted == 2


def test_choice_block_mark_switches_render():
    b = ChoiceBlock("c1", "q", ["A", "B"])
    assert b.answer is None
    b.mark("A")
    assert b.answer == "A"
    assert "已选" in b.render_body()
    assert "A" in b.render_body()


@pytest.mark.asyncio
async def test_transcript_add_clarify_mounts_block():
    from textual.app import App
    from echo_agent.cli.tui.transcript import TranscriptView

    class T(App):
        def compose(self):
            yield TranscriptView()

    app = T()
    async with app.run_test():
        tv = app.query_one(TranscriptView)
        blk = tv.add_clarify("c1", "选哪个?", ["A", "B"])
        assert blk.clarify_id == "c1"
        assert blk.options == ["A", "B"]
