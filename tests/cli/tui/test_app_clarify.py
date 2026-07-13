import pytest

from echo_agent.cli.tui.app import EchoTUI
from echo_agent.cli.tui.protocol import CogEvent


def _clarify_ev(clarify_id, question, options):
    return CogEvent(
        "clarify_request", "evt_1", "in_1",
        {"clarify_id": clarify_id, "question": question, "options": options},
        question,
    )


@pytest.mark.asyncio
async def test_clarify_request_mounts_block_and_blurs_prompt():
    sent: list[str] = []

    async def fake_send(text):
        sent.append(text)

    app = EchoTUI(send_coro=fake_send, session_key="s1")
    async with app.run_test():
        app.on_cognitive(_clarify_ev("c1", "选哪个?", ["A", "B"]))
        assert app._pending_clarify is not None
        assert app.focused is None  # prompt 已被 blur


@pytest.mark.asyncio
async def test_number_key_selects_option_and_sends_command():
    sent: list[str] = []

    async def fake_send(text):
        sent.append(text)

    app = EchoTUI(send_coro=fake_send, session_key="s1")
    async with app.run_test() as pilot:
        app.on_cognitive(_clarify_ev("c1", "选哪个?", ["方案A", "方案B"]))
        await pilot.press("2")
        await pilot.pause()
        assert sent == ["/clarify c1 方案B"]
        assert app._pending_clarify is None


@pytest.mark.asyncio
async def test_arrow_then_enter_selects_highlighted():
    sent: list[str] = []

    async def fake_send(text):
        sent.append(text)

    app = EchoTUI(send_coro=fake_send, session_key="s1")
    async with app.run_test() as pilot:
        app.on_cognitive(_clarify_ev("c1", "选哪个?", ["A", "B", "C"]))
        await pilot.press("down")     # 高亮 -> B
        await pilot.press("enter")
        await pilot.pause()
        assert sent == ["/clarify c1 B"]


@pytest.mark.asyncio
async def test_free_text_input_routes_to_clarify_answer():
    sent: list[str] = []

    async def fake_send(text):
        sent.append(text)

    app = EchoTUI(send_coro=fake_send, session_key="s1")
    async with app.run_test() as pilot:
        app.on_cognitive(_clarify_ev("c1", "选哪个?", ["A", "B"]))
        # 按可打印字符进入自由输入(非数字/导航键)
        await pilot.press("x")
        await pilot.press("y")
        await pilot.press("enter")
        await pilot.pause()
        assert sent == ["/clarify c1 xy"]
        assert app._pending_clarify is None


@pytest.mark.asyncio
async def test_out_of_range_number_falls_back_to_free_input():
    sent: list[str] = []

    async def fake_send(text):
        sent.append(text)

    app = EchoTUI(send_coro=fake_send, session_key="s1")
    async with app.run_test() as pilot:
        from echo_agent.cli.tui.prompt_input import PromptInput
        app.on_cognitive(_clarify_ev("c1", "选哪个?", ["方案A", "方案B"]))
        await pilot.press("5")  # 越界:只有 2 个选项
        await pilot.pause()
        # 未发送任何命令,pending 仍在,已降级进入自由输入
        assert sent == []
        assert app._pending_clarify is not None
        assert app._clarify_free_input is True
        pi = app.query_one(PromptInput)
        assert app.focused is pi
        assert "5" in pi.text


@pytest.mark.asyncio
async def test_out_of_range_number_then_typing_sends_free_answer():
    sent: list[str] = []

    async def fake_send(text):
        sent.append(text)

    app = EchoTUI(send_coro=fake_send, session_key="s1")
    async with app.run_test() as pilot:
        app.on_cognitive(_clarify_ev("c1", "选哪个?", ["方案A", "方案B"]))
        await pilot.press("5")     # 越界 -> 自由输入首字符
        await pilot.press("6")     # 继续打字
        await pilot.press("enter")
        await pilot.pause()
        assert sent == ["/clarify c1 56"]
        assert app._pending_clarify is None


@pytest.mark.asyncio
async def test_number_keys_pass_through_when_no_pending_clarify():
    sent: list[str] = []

    async def fake_send(text):
        sent.append(text)

    app = EchoTUI(send_coro=fake_send, session_key="s1")
    async with app.run_test() as pilot:
        from echo_agent.cli.tui.prompt_input import PromptInput
        app.query_one(PromptInput).focus()
        await pilot.press("2")
        await pilot.pause()
        # 无 pending clarify 时,数字键正常进入输入框,不发命令
        assert sent == []
        assert app.query_one(PromptInput).text == "2"
