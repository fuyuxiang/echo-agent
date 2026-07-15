import pytest

from echo_agent.cli.tui.app import EchoTUI
from echo_agent.cli.tui.blocks import UserTurn
from echo_agent.cli.tui.prompt_input import PromptInput


@pytest.mark.asyncio
async def test_sigil_and_input_are_siblings_in_row():
    app = EchoTUI()
    async with app.run_test() as pilot:
        await pilot.pause()
        row = app.query_one("#input_row")
        ids = {getattr(w, "id", None) or type(w).__name__ for w in row.children}
        assert "prompt_sigil" in ids
        assert any(isinstance(w, PromptInput) for w in row.children)


@pytest.mark.asyncio
async def test_placeholder_visible_when_empty_hidden_when_typed():
    app = EchoTUI()
    async with app.run_test() as pilot:
        await pilot.pause()
        ph = app.query_one("#placeholder")
        assert ph.display is True            # 空态显示占位
        app.query_one(PromptInput).text = "有字了"
        await pilot.pause()
        assert ph.display is False           # 有字隐藏


@pytest.mark.asyncio
async def test_query_one_prompt_input_still_resolves():
    # 包一层 Horizontal 后，app.py 现有 query_one(PromptInput) 仍须命中
    app = EchoTUI()
    async with app.run_test() as pilot:
        await pilot.pause()
        assert isinstance(app.query_one(PromptInput), PromptInput)


@pytest.mark.asyncio
async def test_user_turn_has_visual_separation():
    # 用户任务标题需有上下空行(margin)与左侧强调条(border-left)以隔离每一轮
    app = EchoTUI()
    async with app.run_test() as pilot:
        await pilot.pause()
        tv = app.query_one("TranscriptView")
        w = UserTurn("帮我分析这个项目的启动流程")
        await tv.mount(w)
        await pilot.pause()
        s = w.styles
        assert s.margin.top == 1 and s.margin.bottom == 1   # 上下空行
        assert s.border_left[0] != ""                        # 左侧强调条存在


@pytest.mark.asyncio
async def test_echo_theme_registered_and_active():
    # 现代极简主题作为设计 token 基础层，须在 on_mount 后生效
    app = EchoTUI()
    async with app.run_test() as pilot:
        await pilot.pause()
        assert app.theme == "echo"


@pytest.mark.asyncio
async def test_banner_mounted_on_first_screen():
    # 进入时应展示品牌 banner，且带会话号
    from echo_agent.cli.tui.blocks import Banner

    app = EchoTUI(session_key="sess_x")
    async with app.run_test() as pilot:
        await pilot.pause()
        banners = list(app.query(Banner))
        assert len(banners) == 1
        assert "sess_x" in banners[0].build_text()
