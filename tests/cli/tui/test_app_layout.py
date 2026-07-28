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
async def test_sigil_and_input_do_not_overlap():
    # 回归：PromptInput 一旦被塞进某个 layer（如 layer: base）就脱离 Horizontal
    # 排布流，改按容器原点 x=0 定位，与宽 2 格的 prompt_sigil 重叠。中文首字是
    # 宽字符占 0-1 列，正好被 sigil 盖住，表现为“输入‘你好’只显示‘好’”。
    # 断言两者渲染区域左边界不同，即输入框排在 sigil 右侧而非叠在其上。
    app = EchoTUI()
    async with app.run_test(size=(80, 24)) as pilot:
        await pilot.pause()
        sig = app.query_one("#prompt_sigil")
        pi = app.query_one(PromptInput)
        assert pi.region.x >= sig.region.x + sig.region.width


@pytest.mark.asyncio
async def test_first_wide_char_is_rendered():
    # 回归：逐键输入宽字符后，输入框首个可视行须完整含首字（此前首字被 sigil 覆盖）。
    app = EchoTUI()
    async with app.run_test(size=(80, 24)) as pilot:
        await pilot.pause()
        pi = app.query_one(PromptInput)
        await pilot.press("你", "好")
        await pilot.pause()
        assert pi.text == "你好"
        assert pi.render_line(0).text.startswith("你好")


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
    # 用户任务标题需有上下空行(margin)与左侧强调条(border-left)以隔离每一轮。
    # margin 保留在 CSS 里而不交给 transcript 的分组间距规则：layout.py 把
    # "user" 归为自带间距，两侧都由这里负责，transcript 不再叠加空行。
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
