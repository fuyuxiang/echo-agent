import pytest
from textual.app import App

from echo_agent.cli.tui.prompt_input import PromptInput


@pytest.mark.asyncio
async def test_prompt_input_has_visible_hint():
    class T(App):
        def compose(self):
            yield PromptInput()

    app = T()
    async with app.run_test() as pilot:
        await pilot.pause()
        pi = app.query_one(PromptInput)
        title = str(pi.border_title or "")
        assert "输入" in title  # 明确告知这是输入区
        assert "Enter" in title  # 说明如何发送
