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


@pytest.mark.asyncio
async def test_ctrl_c_acknowledges_the_stop_on_the_progress_row():
    """Ctrl+C 必须在常驻进度行上留痕,而不只是弹一个 3 秒后消失的 toast。

    中断是协作式的:网关要跑到下一个检查点才真正停,这中间进度行原样继续转,
    看起来像 Ctrl+C 完全没生效;等回合真的收尾时,它还会给一个被用户主动取消
    的回合报"完成"。
    """
    async def fake_interrupt(target_event_id: str = ""):
        pass

    app = EchoTUI(interrupt_coro=fake_interrupt)
    async with app.run_test() as pilot:
        await pilot.pause()
        app._turns.note_send("primary")
        app.on_turn_accepted("evt-1")
        app.on_cognitive(CogEvent(
            "tool_call", "t1", "evt-1",
            {"tool_call_id": "tc1", "name": "exec",
             "params": {"command": "sleep 5"}, "status": "running"},
            "执行",
        ))
        await pilot.pause()
        al = app._activity
        assert "Ctrl+C" in al.render_text()      # 中断前:提示可以中断

        await app.action_interrupt()
        await pilot.pause()
        # 行仍在转(还没真停),但已经承认收到了停止请求,且不再提示可以中断
        assert al.is_active is True
        assert "正在停止" in al.render_text()
        assert "Ctrl+C" not in al.render_text()

        # 回合收尾走的是正常回复路径,但被取消过的回合不能报"完成"。
        app.on_user_reply_final("evt-1", "只做了一半")
        await pilot.pause()
        assert al.is_settled is True
        assert "已中断" in al.render_text()
        assert "完成" not in al.render_text()


@pytest.mark.asyncio
async def test_cancelled_turn_also_settles_its_running_tool_line():
    """回合被取消后,上方仍渲染成 "…" 的工具行必须一起收尾。

    协作式中断走的是普通 final 帧,而 final 路径刻意不清扫工具行(一次回答会拆成
    多个 final,工具可能真的还在跑)。于是取消后底部行已经写着"已中断",上方却还有
    一条命令看起来在执行——和用户反馈的"不知道是结束了还是卡住了"是同一类歧义,
    只是错位了一行。
    """
    from echo_agent.cli.tui.blocks import ToolCallBlock

    async def fake_interrupt(target_event_id: str = ""):
        pass

    app = EchoTUI(interrupt_coro=fake_interrupt)
    async with app.run_test() as pilot:
        await pilot.pause()
        app._turns.note_send("primary")
        app.on_turn_accepted("evt-1")
        app.on_cognitive(CogEvent(
            "tool_call", "t1", "evt-1",
            {"tool_call_id": "tc1", "name": "exec",
             "params": {"command": "sleep 5"}, "status": "running"},
            "执行",
        ))
        await pilot.pause()
        await app.action_interrupt()
        app.on_user_reply_final("evt-1", "只做了一半")
        await pilot.pause()

        block = next(w for w in app._tv.children if isinstance(w, ToolCallBlock))
        assert block.status == "interrupted"
        assert "未完成" in block.render_summary()


@pytest.mark.asyncio
async def test_uncancelled_turn_keeps_its_running_tool_line_alive():
    """反向约束:没按 Ctrl+C 时,中间 final 帧不能把仍在跑的工具标成"未完成"。

    与 test_running_tool_line_kept_alive_across_intermediate_final 同一条不变量,
    这里锁的是取消清扫只在取消时发生,不会顺手破坏正常路径。
    """
    from echo_agent.cli.tui.blocks import ToolCallBlock

    app = EchoTUI()
    async with app.run_test() as pilot:
        await pilot.pause()
        app._turns.note_send("primary")
        app.on_turn_accepted("evt-1")
        app.on_cognitive(CogEvent(
            "tool_call", "t1", "evt-1",
            {"tool_call_id": "tc1", "name": "exec",
             "params": {"command": "sleep 5"}, "status": "running"},
            "执行",
        ))
        app.on_user_reply_final("evt-1", "先说一句")
        await pilot.pause()

        block = next(w for w in app._tv.children if isinstance(w, ToolCallBlock))
        assert block.status == "running"


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
