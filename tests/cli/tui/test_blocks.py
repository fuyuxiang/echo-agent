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


def test_cognitive_summary_does_not_double_up_the_glyph():
    """网关侧的 summary 曾自带 emoji，客户端又在行首加自己的符号，同一个图标
    渲染两遍。现在网关只发文本，客户端负责行首符号；但两端各自发版，所以对
    旧网关的历史前缀仍要剥掉。"""
    from echo_agent.cli.tui.blocks import strip_legacy_glyph

    assert strip_legacy_glyph("💭 Thought for 3.2s") == "Thought for 3.2s"
    assert strip_legacy_glyph("✍ 写入 2 条记忆") == "写入 2 条记忆"
    assert strip_legacy_glyph("💰 $0.42") == "$0.42"
    assert strip_legacy_glyph("🧬 skill: 演化") == "skill: 演化"
    # 无前缀的新格式原样保留
    assert strip_legacy_glyph("思考 3.2s") == "思考 3.2s"

    b = CognitiveBlock(_ev("thinking", {}, "💭 Thought for 3.2s"))
    assert "💭" not in b.render_summary()


def test_expandable_cognitive_types_advertise_their_shortcut():
    """记忆/思考两类可展开，行尾给出对应快捷键，否则用户无从知道还有细节。"""
    mem = CognitiveBlock(_ev("memory_recalled", {"items": ["x"]}, "召回 1 条记忆"))
    think = CognitiveBlock(_ev("thinking", {"text": "x"}, "思考 1s"))
    other = CognitiveBlock(_ev("evolution", {}, "skill: 演化"))
    assert "ctrl+r" in mem.render_summary()
    assert "ctrl+o" in think.render_summary()
    assert "ctrl+" not in other.render_summary()


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
    """set_final is the status-line path (notices/errors): plain text, never
    markdown, so hand-built Rich markup keeps working."""
    from echo_agent.cli.tui.blocks import AgentReply

    r = AgentReply.__new__(AgentReply)
    r._buf = ""
    captured = {}
    r.update = lambda content, **kw: captured.setdefault("c", content)
    r.set_final("服务端错误")
    assert isinstance(captured["c"], str)
    assert "服务端错误" in captured["c"]


def test_approval_block_marks_decision():
    a = ApprovalBlock("req1", "shell", {"cmd": "rm x"}, "EXEC 高风险")
    assert a.decision is None
    a.mark("approve")
    assert a.decision == "approve"


def test_themed_markdown_maps_headings_to_theme_palette():
    """Headings must render in the brand palette (teal primary), not Rich's
    default magenta. Render to a plain Console and assert the primary hue shows
    up in the styled output for an h1."""
    from rich.console import Console
    from echo_agent.cli.tui.blocks import ThemedMarkdown

    md = ThemedMarkdown("# 标题", palette={
        "primary": "#4fd1c5", "secondary": "#7f9cf5",
        "accent": "#4fd1c5", "muted": "#8b949e",
    })
    # no_color must be pinned: Rich honours a NO_COLOR env var globally and drops
    # every colour code even under force_terminal, so without this the assertion
    # below fails on any machine (or CI job) that exports NO_COLOR — a property of
    # the environment, not of the palette wiring under test.
    console = Console(
        width=40, color_system="truecolor", force_terminal=True, no_color=False,
    )
    with console.capture() as cap:
        console.print(md)
    out = cap.get()
    # The teal primary (#4fd1c5 → 79;209;197) drives the h1, not magenta.
    assert "79;209;197" in out
    assert "标题" in out


def test_themed_markdown_no_palette_does_not_crash():
    """Empty palette (no app attached, e.g. unit tests) must still render."""
    from rich.console import Console
    from echo_agent.cli.tui.blocks import ThemedMarkdown

    md = ThemedMarkdown("# 标题\n\n正文", palette={})
    console = Console(width=40)
    with console.capture() as cap:
        console.print(md)
    assert "标题" in cap.get()


def test_banner_collapses_to_wordmark_when_narrow():
    """The 5-row block-letter logo collapses to a one-line wordmark on a narrow
    terminal so it doesn't dominate a short screen."""
    from echo_agent.cli.tui.blocks import Banner, _LOGO_ART

    b = Banner.__new__(Banner)
    b.session_key = ""
    b.brand_name = "echo"
    b.brand_tagline = "agent"
    b.brand_welcome = "hi"
    b._narrow = False
    wide = b.build_text()
    assert _LOGO_ART[0] in wide          # full ASCII art on wide

    b._narrow = True
    narrow = b.build_text()
    assert _LOGO_ART[0] not in narrow    # collapsed
    assert "echo" in narrow


@pytest.mark.asyncio
async def test_transcript_mounts_no_block_for_progress():
    """进度不再是 transcript 里的块：心跳块的位置在第一次心跳时就固定了，
    之后的工具行追加在它下面，"还在处理" 反而显示在已完成内容的上方。
    现在进度由停靠的 ActivityLine 承担，transcript 只放真实内容。"""
    from textual.app import App
    from echo_agent.cli.tui.transcript import TranscriptView

    class T(App):
        def compose(self):
            yield TranscriptView()

    app = T()
    async with app.run_test():
        tv = app.query_one(TranscriptView)
        assert not hasattr(tv, "heartbeat_line")
        assert not hasattr(tv, "clear_heartbeats")
        tv.add_user("你好")
        await app.workers.wait_for_complete()
        # 只有用户这一条内容块，没有任何进度块
        assert len(tv.children) == 1


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
        tv.add_user("你好")
        mem = tv.add_cognitive(CogEvent("memory_recalled", "e1", "in_1", {}, "召回"))
        think = tv.add_cognitive(CogEvent("thinking", "e2", "in_1", {}, "思考"))
        tv.start_reply()
        tv.add_approval("req1", "shell", {"cmd": "ls"}, "低风险")
        # 本轮内的其他块不影响 ctrl+r / ctrl+o 的目标
        assert tv.last_memory_block() is mem
        assert tv.last_thinking_block() is think


@pytest.mark.asyncio
async def test_expand_shortcuts_are_scoped_to_the_current_turn():
    """ctrl+r / ctrl+o 原来指向任意历史轮次的最后一个记忆/思考块，于是新一轮
    刚开始就按，会静默展开滚动区上方（屏幕外）的一条旧 trace —— 看起来像按键
    没反应。新一轮开始即清空目标。"""
    from textual.app import App
    from echo_agent.cli.tui.transcript import TranscriptView

    class T(App):
        def compose(self):
            yield TranscriptView()

    app = T()
    async with app.run_test():
        tv = app.query_one(TranscriptView)
        tv.add_user("第一轮")
        old = tv.add_cognitive(CogEvent("thinking", "e1", "in_1", {}, "思考"))
        assert tv.last_thinking_block() is old
        tv.add_user("第二轮")
        assert tv.last_thinking_block() is None
        assert tv.last_memory_block() is None
        fresh = tv.add_cognitive(CogEvent("thinking", "e2", "in_2", {}, "思考"))
        assert tv.last_thinking_block() is fresh


@pytest.mark.asyncio
async def test_blocks_carry_their_turn_and_indent():
    """轮次是打在扁平块上的标签，而不是嵌套容器：pending clarify/approval 必须
    无条件常显且可聚焦（容器一旦折叠或被 /clear 移除就会连带隐藏它们），
    children 也要保持扁平供 /copy、/save、焦点环直接遍历。"""
    from textual.app import App
    from echo_agent.cli.tui.transcript import TranscriptView
    from echo_agent.cli.tui.turn_layout import TRACE_DEPTH

    class T(App):
        def compose(self):
            yield TranscriptView()

    app = T()
    async with app.run_test():
        tv = app.query_one(TranscriptView)
        first = tv.add_user("第一轮")
        trace = tv.add_cognitive(CogEvent("thinking", "e1", "in_1", {}, "思考"))
        reply = tv.start_reply()
        second = tv.add_user("第二轮")
        assert first.turn_seq == trace.turn_seq == reply.turn_seq
        assert second.turn_seq == first.turn_seq + 1
        # trace 比标题/答案缩进一级，答案与对话本身齐平
        assert trace.depth == TRACE_DEPTH
        assert trace.rail != ""
        # 扁平：所有块都是 transcript 的直接子节点
        for w in (first, trace, reply, second):
            assert w in tv.children


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
    # 末尾追加虚拟"其他(自行输入)"哨兵项(编号紧随真实选项)
    assert "4. 其他（自行输入）" in body
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
    # 3 是末尾"其他"哨兵项:不返回答案值,而是标记为自由输入入口
    assert b.option_for_number(3) is None
    assert b.is_free_input_option(3) is True
    assert b.is_free_input_option(2) is False
    assert b.option_for_number(0) is None


def test_choice_block_highlight_move_and_clamp():
    b = ChoiceBlock("c1", "q", ["A", "B", "C"])
    assert b.highlighted == 0
    b.move(1)
    assert b.highlighted == 1
    assert b.highlighted_option() == "B"
    b.move(-5)                    # 下越界钳制
    assert b.highlighted == 0
    b.move(99)                    # 上越界钳制:钳到末尾哨兵项(index 3),非最后一个真实选项
    assert b.highlighted == 3
    assert b.highlighted_is_free_input() is True
    assert b.highlighted_option() is None


def test_choice_block_mark_switches_render():
    b = ChoiceBlock("c1", "q", ["A", "B"])
    assert b.answer is None
    b.mark("A")
    assert b.answer == "A"
    assert "已选" in b.render_body()
    assert "A" in b.render_body()


def test_choice_block_coerces_dict_options_and_marks_without_crash():
    # The model sometimes returns dict options despite the string-only schema.
    # They must render, select, and mark as plain strings (no TypeError from
    # rich.escape on a dict).
    opts = [
        {"value": "A", "description": "深度展开每个章节"},
        {"value": "B"},
        "普通字符串",
    ]
    b = ChoiceBlock("c1", "选哪个?", opts)
    # Display labels show "value — description" for dicts.
    assert b.options == ["A — 深度展开每个章节", "B", "普通字符串"]
    body = b.render_body()
    assert "A — 深度展开每个章节" in body
    # But selecting feeds back the bare VALUE, not the rendered label: the
    # description is a human hint, never an option the model offered.
    picked = b.highlighted_option()
    assert picked == "A"
    assert b.option_for_number(1) == "A"
    b.mark(picked)
    assert b.answer == "A"
    assert "已选" in b.render_body()


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
