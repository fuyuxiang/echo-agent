import pytest
from textual.app import App

from echo_agent.cli.tui.transcript import TranscriptView
from echo_agent.cli.tui.protocol import CogEvent


@pytest.mark.asyncio
async def test_clear_removes_children_and_indexes():
    class T(App):
        def compose(self):
            yield TranscriptView()

    app = T()
    async with app.run_test() as pilot:
        tv = app.query_one(TranscriptView)
        tv.add_user("你好")
        tv.add_cognitive(CogEvent("memory_recalled", "e1", "in1", {}, "召回"))
        await pilot.pause()
        assert len(tv.children) > 0
        tv.clear()
        await pilot.pause()
        assert len(tv.children) == 0
        assert tv.last_memory_block() is None
        # 分组间距的游标也要复位：清屏后下一个块按"新开一屏"排布，
        # 而不是延续被清掉的那个块的分组。
        assert tv._prev_group is None


@pytest.mark.asyncio
async def test_blank_line_only_at_group_boundaries():
    """空行由 transcript 在分组切换处插入，而不是每个块自带 CSS margin：
    一串工具/思考行内部不能被空行拆开，"trace 收尾 → 答案开始"必须有分界。"""
    class T(App):
        def compose(self):
            yield TranscriptView()

    app = T()
    async with app.run_test() as pilot:
        tv = app.query_one(TranscriptView)
        first = tv.add_cognitive(CogEvent("thinking", "e1", "in1", {}, "思考 1s"))
        second = tv.add_cognitive(CogEvent("thinking", "e2", "in1", {}, "思考 2s"))
        reply = tv.start_reply()
        await pilot.pause()
        # 屏首不留空行；同组连续不留空行；跨组（trail → model）才留
        assert first.styles.margin.top == 0
        assert second.styles.margin.top == 0
        assert reply.styles.margin.top == 1


@pytest.mark.asyncio
async def test_user_turn_spacing_is_left_to_css():
    """UserTurn 在 layout 里归为自带间距：CSS 已给它上下 margin
    (test_app_layout 覆盖)，transcript 不能再叠一行，否则每轮标题上方是两行
    空白。这里用不加载 tcss 的裸 App，断言的正是"transcript 自己没加"。"""
    class T(App):
        def compose(self):
            yield TranscriptView()

    app = T()
    async with app.run_test() as pilot:
        tv = app.query_one(TranscriptView)
        tv.add_cognitive(CogEvent("thinking", "e1", "in1", {}, "思考 1s"))
        turn = tv.add_user("下一轮")
        # 紧跟用户标题之后的首个 trace 行也不顶空行（标题下方已有 CSS margin）
        after = tv.add_cognitive(CogEvent("thinking", "e2", "in2", {}, "思考 2s"))
        await pilot.pause()
        assert turn.styles.margin.top == 0
        assert after.styles.margin.top == 0
