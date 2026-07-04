import pytest
from textual.app import App

from echo_agent.cli.tui.prompt_input import PromptInput


class _Host(App):
    def compose(self):
        yield PromptInput()


@pytest.mark.asyncio
async def test_no_border_title():
    app = _Host()
    async with app.run_test() as pilot:
        await pilot.pause()
        pi = app.query_one(PromptInput)
        assert not pi.border_title  # 提示符已移到行首兄弟 widget，标题清空


@pytest.mark.asyncio
async def test_is_empty_tracks_text():
    app = _Host()
    async with app.run_test() as pilot:
        await pilot.pause()
        pi = app.query_one(PromptInput)
        assert pi.is_empty is True
        pi.text = "hi"
        assert pi.is_empty is False


@pytest.mark.asyncio
async def test_enter_submits_and_records_history():
    app = _Host()
    async with app.run_test() as pilot:
        await pilot.pause()
        pi = app.query_one(PromptInput)
        pi.text = "第一条"
        await pilot.press("enter")
        await pilot.pause()
        assert pi.text == ""
        assert pi._history[-1] == "第一条"


@pytest.mark.asyncio
async def test_up_recalls_previous_message():
    app = _Host()
    async with app.run_test() as pilot:
        await pilot.pause()
        pi = app.query_one(PromptInput)
        pi.text = "早先"
        await pilot.press("enter")
        await pilot.pause()
        await pilot.press("up")
        await pilot.pause()
        assert pi.text == "早先"


@pytest.mark.asyncio
async def test_down_restores_draft_at_end():
    app = _Host()
    async with app.run_test() as pilot:
        await pilot.pause()
        pi = app.query_one(PromptInput)
        pi.text = "历史项"
        await pilot.press("enter")
        await pilot.pause()
        pi.text = "草稿中"          # 未发送的草稿
        await pilot.press("up")       # 进入历史，暂存草稿
        await pilot.pause()
        assert pi.text == "历史项"
        await pilot.press("down")     # 走回草稿
        await pilot.pause()
        assert pi.text == "草稿中"
