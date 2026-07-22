"""回归：转录容器不应抢占焦点，否则方向键会被其滚动绑定劫走。"""

import pytest

from echo_agent.cli.tui.app import EchoTUI
from echo_agent.cli.tui.prompt_input import PromptInput
from echo_agent.cli.tui.protocol import CogEvent
from echo_agent.cli.tui.transcript import TranscriptView


def _cog(i: int) -> CogEvent:
    return CogEvent(
        cog_type="thinking",
        cog_event_id=f"t{i}",
        inbound_event_id="e",
        data={"text": f"thought {i}"},
        summary=f"s{i}",
    )


@pytest.mark.asyncio
async def test_transcript_container_not_focusable():
    # 容器只是子 block 的滚动视口，本身不该是焦点目标。
    app = EchoTUI()
    async with app.run_test(size=(80, 20)) as pilot:
        await pilot.pause()
        assert app.query_one(TranscriptView).can_focus is False


@pytest.mark.asyncio
async def test_tab_from_prompt_lands_on_block_not_container():
    # 关键回归：焦点进输入框后再 Tab 回来，应落到某个 block，而不是
    # TranscriptView 容器——否则方向键命中容器的 scroll_up/scroll_down 绑定，
    # 滚动条把上下键劫走，block 选择失效。
    app = EchoTUI()
    async with app.run_test(size=(80, 20)) as pilot:
        await pilot.pause()
        tv = app.query_one(TranscriptView)
        for i in range(3):
            tv.add_cognitive(_cog(i))
        await pilot.pause()

        app.query_one(PromptInput).focus()
        await pilot.pause()

        # 走完一整轮 Tab，容器不应出现在焦点链里。
        seen = []
        for _ in range(6):
            await pilot.press("tab")
            await pilot.pause()
            seen.append(type(app.focused).__name__)
        assert "TranscriptView" not in seen
        assert "CognitiveBlock" in seen


@pytest.mark.asyncio
async def test_blocks_remain_focusable():
    # 容器不可聚焦，但子 block 仍必须可聚焦（can_focus_children 默认为 True）。
    app = EchoTUI()
    async with app.run_test(size=(80, 20)) as pilot:
        await pilot.pause()
        tv = app.query_one(TranscriptView)
        for i in range(3):
            tv.add_cognitive(_cog(i))
        await pilot.pause()
        blocks = list(tv.query("CognitiveBlock"))
        blocks[-1].focus()
        await pilot.pause()
        assert app.focused is blocks[-1]
