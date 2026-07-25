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


# ── 重连后仍要保住对服务端在跑回合的控制权 ──────────────────────────────────


@pytest.mark.asyncio
async def test_ctrl_c_after_reconnect_still_interrupts_running_turn():
    """回归:重连后 Ctrl+C 必须发无目标中断,而不是进入退出确认。

    重连会清掉全部 turn 关联(必须清:那些 id 永远无法被后续帧回收,留着会永久拦住
    提交)。但清掉关联不等于回合结束 —— 服务端可能还在跑。原实现两者混为一谈,于是
    Ctrl+C 落到第 4 分支武装退出,用户失去了停止手段。
    """
    interrupts: list[str] = []

    async def fake_interrupt(target_event_id: str = ""):
        interrupts.append(target_event_id)

    app = EchoTUI(interrupt_coro=fake_interrupt)
    async with app.run_test() as pilot:
        await pilot.pause()
        app._turns.note_send("primary")
        app.on_turn_accepted("evt-1")
        app.notify_reconnected()              # 断线重连:关联全部失效
        await pilot.pause()

        await app.action_interrupt()
        await pilot.pause()

        # 空目标 = "停掉正在跑的那个",网关支持这个语义。
        assert interrupts == [""], f"重连后未发出中断: {interrupts}"
        assert app._exit is False, "不该进入退出流程"
        assert app._last_ctrl_c == 0.0, "不该武装退出窗口"


@pytest.mark.asyncio
async def test_ctrl_c_after_reconnect_when_idle_arms_exit():
    """空闲时重连后 Ctrl+C 仍应是正常的两段式退出 —— 没有工作可中断。"""
    interrupts: list[str] = []

    async def fake_interrupt(target_event_id: str = ""):
        interrupts.append(target_event_id)

    app = EchoTUI(interrupt_coro=fake_interrupt)
    async with app.run_test() as pilot:
        await pilot.pause()
        app.notify_reconnected()              # 空闲重连
        await pilot.pause()
        await app.action_interrupt()
        await pilot.pause()
        assert interrupts == [], "无在飞工作时不该发中断"
        assert app._last_ctrl_c > 0.0


@pytest.mark.asyncio
async def test_reply_after_reconnect_disarms_the_interrupt_path():
    """断连前的回合以无关联回复落地后,Ctrl+C 应恢复为退出确认。"""
    interrupts: list[str] = []

    async def fake_interrupt(target_event_id: str = ""):
        interrupts.append(target_event_id)

    app = EchoTUI(interrupt_coro=fake_interrupt)
    async with app.run_test() as pilot:
        await pilot.pause()
        app._turns.note_send("primary")
        app.on_turn_accepted("evt-1")
        app.notify_reconnected()
        # 服务端把回合跑完了,final 以无关联回复到达。
        app.on_user_reply_final("evt-1", "答案")
        await pilot.pause()

        await app.action_interrupt()
        await pilot.pause()
        assert interrupts == [], "回合已结束,不该再发中断"
        assert app._last_ctrl_c > 0.0
