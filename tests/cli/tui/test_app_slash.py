import pytest

from echo_agent.cli.tui.app import EchoTUI
from echo_agent.cli.tui.prompt_input import PromptInput
from echo_agent.cli.tui.transcript import TranscriptView


@pytest.mark.asyncio
async def test_slash_opens_panel_with_filtered_commands():
    app = EchoTUI()
    async with app.run_test() as pilot:
        await pilot.pause()
        app.query_one(PromptInput).text = "/ap"
        await pilot.pause()
        panel = app.query_one("#slash_panel")
        assert panel.display is True
        assert panel.option_count == 2   # /approve /approvals


@pytest.mark.asyncio
async def test_panel_hidden_for_non_slash_text():
    app = EchoTUI()
    async with app.run_test() as pilot:
        await pilot.pause()
        app.query_one(PromptInput).text = "普通消息"
        await pilot.pause()
        assert app.query_one("#slash_panel").display is False


@pytest.mark.asyncio
async def test_local_clear_command_clears_transcript_and_not_sent():
    sent = []
    async def fake_send(t): sent.append(t)
    app = EchoTUI(send_coro=fake_send)
    async with app.run_test() as pilot:
        await pilot.pause()
        tv = app.query_one(TranscriptView)
        tv.add_user("旧消息")
        await pilot.pause()
        app.post_message(PromptInput.Submitted("/clear"))
        await pilot.pause()
        assert len(tv.children) == 0
        assert sent == []                # 本地命令不发上行


@pytest.mark.asyncio
async def test_local_quit_command_not_sent():
    sent = []
    async def fake_send(t): sent.append(t)
    app = EchoTUI(send_coro=fake_send)
    async with app.run_test() as pilot:
        await pilot.pause()
        app.post_message(PromptInput.Submitted("/quit"))
        await pilot.pause()
        assert sent == []


@pytest.mark.asyncio
async def test_normal_message_still_sent():
    sent = []
    async def fake_send(t): sent.append(t)
    app = EchoTUI(send_coro=fake_send)
    async with app.run_test() as pilot:
        await pilot.pause()
        app.post_message(PromptInput.Submitted("你好"))
        await pilot.pause()
        assert sent == ["你好"]
