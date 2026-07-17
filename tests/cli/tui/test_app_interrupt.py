import pytest

from echo_agent.cli.tui.app import EchoTUI
from echo_agent.cli.tui.prompt_input import PromptInput
from echo_agent.cli.tui.protocol import CogEvent


def _approval_ev(request_id="r1", action="删除文件", risk="high"):
    return CogEvent(
        "approval_request", "e1", "in1",
        {"request_id": request_id, "action": action, "params": {}, "risk": risk},
        action,
    )


@pytest.mark.asyncio
async def test_ctrl_c_while_turn_running_sends_interrupt_not_exit():
    # A turn is in flight → Ctrl+C sends a control interrupt frame and stays
    # running, rather than arming the exit guard.
    interrupts: list[str] = []

    async def fake_interrupt(target_event_id: str = ""):
        interrupts.append(target_event_id)

    from echo_agent.cli.tui.status_bar import StatusBar

    app = EchoTUI(interrupt_coro=fake_interrupt)
    async with app.run_test() as pilot:
        await pilot.pause()
        app.query_one(StatusBar).start_turn_timer()   # turn 进行中
        app.on_turn_accepted("evt-123")               # 记录在飞 turn 的 event_id
        await pilot.pause()
        await app.action_interrupt()
        await pilot.pause()
        assert interrupts == ["evt-123"]     # 发了中断帧且带上目标 event_id
        assert app._exit is False            # 不退出
        assert app._last_ctrl_c == 0.0       # 未武装退出窗口


@pytest.mark.asyncio
async def test_first_ctrl_c_when_empty_arms_guard_not_exit():
    app = EchoTUI()
    async with app.run_test() as pilot:
        await pilot.pause()
        await app.action_interrupt()
        await pilot.pause()
        assert app._exit is False          # 首按不退出
        assert app._last_ctrl_c > 0.0       # 已武装退出窗口


@pytest.mark.asyncio
async def test_second_ctrl_c_within_window_exits():
    app = EchoTUI()
    async with app.run_test() as pilot:
        await pilot.pause()
        await app.action_interrupt()        # 首按：武装
        await app.action_interrupt()        # 2 秒内二次：退出
        await pilot.pause()
        assert app._exit is True


@pytest.mark.asyncio
async def test_ctrl_c_with_text_clears_prompt_not_exit():
    app = EchoTUI()
    async with app.run_test() as pilot:
        await pilot.pause()
        pi = app.query_one(PromptInput)
        pi.text = "半句话"
        await pilot.pause()
        await app.action_interrupt()
        await pilot.pause()
        assert pi.text == ""                # 清空输入
        assert app._exit is False           # 不退出
        assert app._last_ctrl_c == 0.0      # 未武装退出


@pytest.mark.asyncio
async def test_ctrl_c_denies_pending_approval_not_exit():
    sent: list[str] = []

    async def fake_send(t):
        sent.append(t)

    app = EchoTUI(send_coro=fake_send)
    async with app.run_test() as pilot:
        await pilot.pause()
        app.on_cognitive(_approval_ev("r1"))
        await pilot.pause()
        await app.action_interrupt()
        await pilot.pause()
        assert sent == ["/deny r1"]          # 拒绝，解阻服务端
        assert app._pending_approval is None
        assert app._exit is False            # 不退出


@pytest.mark.asyncio
async def test_stale_guard_does_not_exit_after_window():
    # 首按武装后超过窗口再按，视为新的首按（重新武装），不退出。
    app = EchoTUI()
    async with app.run_test() as pilot:
        await pilot.pause()
        await app.action_interrupt()
        # 手动把上次时间推到窗口之前
        app._last_ctrl_c -= app.CTRL_C_EXIT_WINDOW + 1.0
        await app.action_interrupt()
        await pilot.pause()
        assert app._exit is False


@pytest.mark.asyncio
async def test_ctrl_c_keypress_routes_to_interrupt():
    # 真键路径：ctrl+c 应走 action_interrupt（首按不退出），而非旧的直接退出。
    app = EchoTUI()
    async with app.run_test() as pilot:
        await pilot.pause()
        await pilot.press("ctrl+c")
        await pilot.pause()
        assert app._exit is False
        assert app._last_ctrl_c > 0.0
