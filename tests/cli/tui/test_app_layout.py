import pytest

from echo_agent.cli.tui.app import EchoTUI
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
