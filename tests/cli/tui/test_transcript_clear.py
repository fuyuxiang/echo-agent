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
        assert tv.heartbeat_count == 0
