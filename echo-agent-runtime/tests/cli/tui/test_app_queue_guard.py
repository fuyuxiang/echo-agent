"""L2 回归：一轮对话仍在进行时，用户在输入框敲入的普通文本不应被无脑当成
新一轮立即发出（gateway 按 session 串行排队，且这条消息并未绑定到模型刚问出
的问题）。要求二次回车确认后，才作为排队的新一轮发送。

覆盖的真实故障：模型用纯文本问"全删还是逐条勾"（未调用 clarify），用户马上敲
"删掉A类"，过去 on_prompt_input_submitted 直接 note_send("primary") 发出，导致
抢答被错判成一条脱离上下文的新 primary turn。"""

import pytest

from echo_agent.cli.tui.app import EchoTUI
from echo_agent.cli.tui.prompt_input import PromptInput
from echo_agent.cli.tui.protocol import CogEvent


def _submitted(text: str) -> PromptInput.Submitted:
    return PromptInput.Submitted(text)


def _mark_turn_running(app: EchoTUI, event_id: str = "evt-run") -> None:
    """把一条 primary turn 标为在飞，使 has_active_primary 为真。"""
    app._turns.note_send("primary")
    app.on_turn_accepted(event_id)


@pytest.mark.asyncio
async def test_first_submit_while_turn_running_asks_confirm_not_send():
    sent: list[str] = []

    async def fake_send(text):
        sent.append(text)

    app = EchoTUI(send_coro=fake_send, session_key="s1")
    async with app.run_test() as pilot:
        await pilot.pause()
        _mark_turn_running(app)
        await app.on_prompt_input_submitted(_submitted("删掉A类"))
        await pilot.pause()
        # 首次提交：不发送，武装确认窗口，并把文本回填输入框以便二次回车重发
        assert sent == []
        assert app._last_queue_confirm > 0.0
        assert app.query_one(PromptInput).text == "删掉A类"


@pytest.mark.asyncio
async def test_second_submit_within_window_sends_as_queued_turn():
    sent: list[str] = []

    async def fake_send(text):
        sent.append(text)

    app = EchoTUI(send_coro=fake_send, session_key="s1")
    async with app.run_test() as pilot:
        await pilot.pause()
        _mark_turn_running(app)
        await app.on_prompt_input_submitted(_submitted("删掉A类"))   # 首次：确认
        await app.on_prompt_input_submitted(_submitted("删掉A类"))   # 二次：真正发送
        await pilot.pause()
        assert sent == ["删掉A类"]
        # 确认窗口已解除
        assert app._last_queue_confirm == 0.0


@pytest.mark.asyncio
async def test_submit_when_idle_sends_immediately():
    # 无在飞 turn 时保持原行为：直接发送，不需要二次确认。
    sent: list[str] = []

    async def fake_send(text):
        sent.append(text)

    app = EchoTUI(send_coro=fake_send, session_key="s1")
    async with app.run_test() as pilot:
        await pilot.pause()
        await app.on_prompt_input_submitted(_submitted("你好"))
        await pilot.pause()
        assert sent == ["你好"]


@pytest.mark.asyncio
async def test_stale_confirm_rearms_not_sends():
    # 首次确认后超过窗口再提交，视为新的首次确认（重新武装），仍不发送。
    sent: list[str] = []

    async def fake_send(text):
        sent.append(text)

    app = EchoTUI(send_coro=fake_send, session_key="s1")
    async with app.run_test() as pilot:
        await pilot.pause()
        _mark_turn_running(app)
        await app.on_prompt_input_submitted(_submitted("删掉A类"))
        # 把上次确认时间推到窗口之前
        app._last_queue_confirm -= app.QUEUE_CONFIRM_WINDOW + 1.0
        await app.on_prompt_input_submitted(_submitted("删掉A类"))
        await pilot.pause()
        assert sent == []
        assert app._last_queue_confirm > 0.0


@pytest.mark.asyncio
async def test_local_command_works_while_turn_running():
    # 本地命令（/clear 等）在一轮进行中仍应立即执行，不受排队确认拦截。
    sent: list[str] = []

    async def fake_send(text):
        sent.append(text)

    app = EchoTUI(send_coro=fake_send, session_key="s1")
    async with app.run_test() as pilot:
        await pilot.pause()
        _mark_turn_running(app)
        await app.on_prompt_input_submitted(_submitted("/clear"))
        await pilot.pause()
        # 本地命令不上行、不触发排队确认
        assert sent == []
        assert app._last_queue_confirm == 0.0


@pytest.mark.asyncio
async def test_reconnect_clears_stuck_turn_and_unblocks_submit():
    # 真 bug 端到端回归：turn 在飞时断线，其 final 断连期间丢失永远到不了 CLI。
    # 重连后必须清掉残留的 in-flight 关联，否则 has_active_primary 永久为真、
    # queue-guard 永久拦截，用户直到重启才能再发消息。
    sent: list[str] = []

    async def fake_send(text):
        sent.append(text)

    async def fake_reconnect():
        return True

    app = EchoTUI(send_coro=fake_send, session_key="s1", reconnect_coro=fake_reconnect)
    async with app.run_test() as pilot:
        await pilot.pause()
        _mark_turn_running(app, "evt-lost")
        assert app._turns.has_active_primary is True
        # 断线 → 重连成功。
        app.notify_disconnected()
        app.notify_reconnected()
        await pilot.pause()
        # 残留已清，重连后第一条消息应直接发送，不被 queue-guard 拦。
        assert app._turns.has_active_primary is False
        await app.on_prompt_input_submitted(_submitted("重连后的新消息"))
        await pilot.pause()
        assert sent == ["重连后的新消息"]
        assert app._last_queue_confirm == 0.0


@pytest.mark.asyncio
async def test_clarify_free_input_takes_precedence_over_queue_guard():
    # clarify 自由文本答复优先于排队确认：即使有在飞 turn，也直接作为 clarify 答案。
    sent: list[str] = []

    async def fake_send(text):
        sent.append(text)

    app = EchoTUI(send_coro=fake_send, session_key="s1")
    async with app.run_test() as pilot:
        await pilot.pause()
        _mark_turn_running(app)
        app.on_cognitive(CogEvent(
            "clarify_request", "evt_c", "in_c",
            {"clarify_id": "c1", "question": "选哪个?", "options": ["A", "B"]},
            "选哪个?",
        ))
        app._clarify_free_input = True
        await app.on_prompt_input_submitted(_submitted("自定义答案"))
        await pilot.pause()
        assert sent == ["/clarify c1 自定义答案"]
        assert app._last_queue_confirm == 0.0
