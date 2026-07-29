import pytest

from echo_agent.cli.tui.app import EchoTUI
from echo_agent.cli.tui.protocol import CogEvent


def _clarify_ev(clarify_id, question, options):
    return CogEvent(
        "clarify_request", "evt_1", "in_1",
        {"clarify_id": clarify_id, "question": question, "options": options},
        question,
    )


def _closed_ev(clarify_id):
    return CogEvent(
        "clarify_closed", "evt_2", "in_1", {"clarify_id": clarify_id}, "",
    )


@pytest.mark.asyncio
async def test_clarify_request_mounts_block_and_blurs_prompt():
    sent: list[str] = []

    async def fake_send(text):
        sent.append(text)

    app = EchoTUI(send_coro=fake_send, session_key="s1")
    async with app.run_test():
        from echo_agent.cli.tui.prompt_input import PromptInput
        app.on_cognitive(_clarify_ev("c1", "选哪个?", ["A", "B"]))
        assert app._pending_clarify is not None
        # prompt 被禁用(而非仅 blur),因此 focused 落到 None
        assert app.query_one(PromptInput).disabled is True
        assert app.focused is None


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


@pytest.mark.asyncio
async def test_mouse_click_cannot_break_option_selection():
    """回归:选项待定时鼠标点输入框,过去会让输入框重新聚焦、选项键失灵。

    输入框被禁用后,点击无法聚焦它(textual 不聚焦 disabled widget),数字键
    仍走 App 级 binding 正常选中。"""
    sent: list[str] = []

    async def fake_send(text):
        sent.append(text)

    app = EchoTUI(send_coro=fake_send, session_key="s1")
    async with app.run_test() as pilot:
        from echo_agent.cli.tui.prompt_input import PromptInput
        app.on_cognitive(_clarify_ev("c1", "选哪个?", ["方案A", "方案B"]))
        pi = app.query_one(PromptInput)
        # 模拟鼠标点击输入框:禁用态下点击不应聚焦它
        await pilot.click(PromptInput)
        await pilot.pause()
        assert pi.disabled is True
        assert app.focused is None
        # 点击后数字键仍能选中选项(过去这里会失灵)
        await pilot.press("1")
        await pilot.pause()
        assert sent == ["/clarify c1 方案A"]
        assert app._pending_clarify is None
        # 回答后输入框重新启用
        assert pi.disabled is False


@pytest.mark.asyncio
async def test_answer_reenables_prompt():
    sent: list[str] = []

    async def fake_send(text):
        sent.append(text)

    app = EchoTUI(send_coro=fake_send, session_key="s1")
    async with app.run_test() as pilot:
        from echo_agent.cli.tui.prompt_input import PromptInput
        app.on_cognitive(_clarify_ev("c1", "选哪个?", ["A", "B"]))
        assert app.query_one(PromptInput).disabled is True
        await pilot.press("1")
        await pilot.pause()
        assert sent == ["/clarify c1 A"]
        assert app.query_one(PromptInput).disabled is False


@pytest.mark.asyncio
async def test_other_option_number_enters_free_input():
    """选末尾"其他(自行输入)"哨兵项应切到自由输入,而非发送答案。

    2 个真实选项 -> 哨兵编号为 3。"""
    sent: list[str] = []

    async def fake_send(text):
        sent.append(text)

    app = EchoTUI(send_coro=fake_send, session_key="s1")
    async with app.run_test() as pilot:
        from echo_agent.cli.tui.prompt_input import PromptInput
        app.on_cognitive(_clarify_ev("c1", "选哪个?", ["方案A", "方案B"]))
        await pilot.press("3")   # 哨兵"其他"
        await pilot.pause()
        assert sent == []
        assert app._clarify_free_input is True
        pi = app.query_one(PromptInput)
        assert pi.disabled is False
        assert app.focused is pi
        # 现在自由打字并提交,作为 clarify 答案送回
        await pilot.press("z")
        await pilot.press("enter")
        await pilot.pause()
        assert sent == ["/clarify c1 z"]


@pytest.mark.asyncio
async def test_other_option_via_arrow_enter_enters_free_input():
    sent: list[str] = []

    async def fake_send(text):
        sent.append(text)

    app = EchoTUI(send_coro=fake_send, session_key="s1")
    async with app.run_test() as pilot:
        from echo_agent.cli.tui.prompt_input import PromptInput
        app.on_cognitive(_clarify_ev("c1", "选哪个?", ["A", "B"]))
        # 高亮从 A 往下移到哨兵(A->B->其他),回车进入自由输入
        await pilot.press("down")
        await pilot.press("down")
        await pilot.press("enter")
        await pilot.pause()
        assert sent == []
        assert app._clarify_free_input is True
        assert app.query_one(PromptInput).disabled is False


# --- 缺陷一回归:自由输入曾是单向门,进去就再也回不到选项 ---


@pytest.mark.asyncio
async def test_escape_returns_from_free_input_to_options():
    """回归:误按一个字母进入自由输入后,Esc 应把键盘交还给选项。

    过去 _clarify_free_input 一旦置真就无法复位,check_action 永久过滤掉
    数字/↑↓/回车,选项块还挂在屏上却完全点不动。"""
    sent: list[str] = []

    async def fake_send(text):
        sent.append(text)

    app = EchoTUI(send_coro=fake_send, session_key="s1")
    async with app.run_test() as pilot:
        from echo_agent.cli.tui.prompt_input import PromptInput
        app.on_cognitive(_clarify_ev("c1", "选哪个?", ["A", "B", "C"]))
        pi = app.query_one(PromptInput)
        await pilot.press("x")            # 误触进入自由输入
        await pilot.pause()
        assert app._clarify_free_input is True
        pi.text = ""                      # 清空,准备退回
        await pilot.pause()
        await pilot.press("escape")
        await pilot.pause()
        assert app._clarify_free_input is False
        assert pi.disabled is True
        # 选项键恢复可用
        await pilot.press("down")
        await pilot.press("enter")
        await pilot.pause()
        assert sent == ["/clarify c1 B"]


@pytest.mark.asyncio
async def test_clearing_prompt_returns_to_options():
    """清空输入框即视为撤销模式切换 —— 擦干净是最自然的 undo。"""
    sent: list[str] = []

    async def fake_send(text):
        sent.append(text)

    app = EchoTUI(send_coro=fake_send, session_key="s1")
    async with app.run_test() as pilot:
        from echo_agent.cli.tui.prompt_input import PromptInput
        app.on_cognitive(_clarify_ev("c1", "选哪个?", ["A", "B"]))
        pi = app.query_one(PromptInput)
        await pilot.press("x")
        await pilot.pause()
        assert app._clarify_free_input is True
        pi.text = ""
        await pilot.pause()
        assert app._clarify_free_input is False
        assert pi.disabled is True
        await pilot.press("1")
        await pilot.pause()
        assert sent == ["/clarify c1 A"]


@pytest.mark.asyncio
async def test_escape_keeps_text_when_prompt_not_empty():
    """输入框非空时 Esc 不得丢弃用户已打的答案。"""
    sent: list[str] = []

    async def fake_send(text):
        sent.append(text)

    app = EchoTUI(send_coro=fake_send, session_key="s1")
    async with app.run_test() as pilot:
        from echo_agent.cli.tui.prompt_input import PromptInput
        app.on_cognitive(_clarify_ev("c1", "选哪个?", ["A", "B"]))
        pi = app.query_one(PromptInput)
        await pilot.press("x")
        await pilot.press("y")
        await pilot.pause()
        await pilot.press("escape")
        await pilot.pause()
        # 仍在自由输入,文本完好
        assert app._clarify_free_input is True
        assert pi.text == "xy"
        await pilot.press("enter")
        await pilot.pause()
        assert sent == ["/clarify c1 xy"]


@pytest.mark.asyncio
async def test_free_text_only_clarify_never_locks_prompt_on_empty():
    """无选项的纯自由回答题:清空输入框不能把输入框锁死(没有选项可退回)。"""
    sent: list[str] = []

    async def fake_send(text):
        sent.append(text)

    app = EchoTUI(send_coro=fake_send, session_key="s1")
    async with app.run_test() as pilot:
        from echo_agent.cli.tui.prompt_input import PromptInput
        app.on_cognitive(_clarify_ev("c1", "随便说点什么", []))
        pi = app.query_one(PromptInput)
        await pilot.press("x")
        await pilot.pause()
        pi.text = ""
        await pilot.pause()
        assert app._clarify_free_input is True
        assert pi.disabled is False
        await pilot.press("escape")
        await pilot.pause()
        assert pi.disabled is False


@pytest.mark.asyncio
async def test_submitting_free_answer_does_not_relock_prompt():
    """提交自由答案会顺带清空输入框,ContentChanged 不能把已解锁的输入框重新锁上。

    _submit() 先投 Submitted 再置 text="",所以 _answer_clarify 已先清掉
    _pending_clarify,新增的自动回退分支应当被 guard 跳过。这条测试就是钉住
    这个投递顺序。"""
    sent: list[str] = []

    async def fake_send(text):
        sent.append(text)

    app = EchoTUI(send_coro=fake_send, session_key="s1")
    async with app.run_test() as pilot:
        from echo_agent.cli.tui.prompt_input import PromptInput
        app.on_cognitive(_clarify_ev("c1", "选哪个?", ["A", "B"]))
        pi = app.query_one(PromptInput)
        await pilot.press("z")
        await pilot.press("enter")
        await pilot.pause()
        assert sent == ["/clarify c1 z"]
        assert app._pending_clarify is None
        assert app._clarify_free_input is False
        assert pi.disabled is False
        assert pi.text == ""


# --- 缺陷二回归:中途 is_final 帧曾误清 pending,导致真死锁 ---


@pytest.mark.asyncio
async def test_mid_turn_final_frame_keeps_clarify_alive():
    """回归:一个答案被拆成多个 is_final 帧,中途那帧不得退休还活着的提问。

    过去这里会清 _pending_clarify 并解锁输入框:选项当场变哑,用户接着打的字
    成了新一轮 turn,而服务端那轮仍 parked 在 wait_for_answer 持有 session
    锁 —— 新 turn 永久排队,就是"正在思考"之后再无反应的死锁。"""
    sent: list[str] = []

    async def fake_send(text):
        sent.append(text)

    app = EchoTUI(send_coro=fake_send, session_key="s1")
    async with app.run_test() as pilot:
        from echo_agent.cli.tui.prompt_input import PromptInput
        app.on_cognitive(_clarify_ev("c1", "选哪个?", ["A", "B"]))
        app.on_user_reply_final("in_1", "中途文本片段")
        await pilot.pause()
        assert app._pending_clarify is not None
        assert app.query_one(PromptInput).disabled is True
        # 选项键仍然有效
        await pilot.press("down")
        await pilot.press("enter")
        await pilot.pause()
        assert sent == ["/clarify c1 B"]


@pytest.mark.asyncio
async def test_clarify_closed_retires_prompt_and_unlocks():
    """服务端显式关闭信号才是退休提问的依据:置灰选项块 + 解锁输入框。"""
    sent: list[str] = []

    async def fake_send(text):
        sent.append(text)

    app = EchoTUI(send_coro=fake_send, session_key="s1")
    async with app.run_test() as pilot:
        from echo_agent.cli.tui.prompt_input import PromptInput
        app.on_cognitive(_clarify_ev("c1", "选哪个?", ["A", "B"]))
        blk = app._pending_clarify
        app.on_cognitive(_closed_ev("c1"))
        await pilot.pause()
        assert app._pending_clarify is None
        assert app._clarify_free_input is False
        assert blk.cancelled is True
        assert app.query_one(PromptInput).disabled is False
        # 数字键回归普通输入,不再当选项用
        await pilot.press("1")
        await pilot.pause()
        assert sent == []
        assert app.query_one(PromptInput).text == "1"


@pytest.mark.asyncio
async def test_stale_clarify_closed_does_not_kill_newer_prompt():
    """迟到的关闭帧只认自己那道题,不能把后来挂上的新提问一起清掉。"""
    sent: list[str] = []

    async def fake_send(text):
        sent.append(text)

    app = EchoTUI(send_coro=fake_send, session_key="s1")
    async with app.run_test() as pilot:
        from echo_agent.cli.tui.prompt_input import PromptInput
        app.on_cognitive(_clarify_ev("c2", "第二题?", ["A", "B"]))
        app.on_cognitive(_closed_ev("c1"))      # 上一题的关闭帧
        await pilot.pause()
        assert app._pending_clarify is not None
        assert app.query_one(PromptInput).disabled is True
        await pilot.press("1")
        await pilot.pause()
        assert sent == ["/clarify c2 A"]


@pytest.mark.asyncio
async def test_error_frame_retires_pending_clarify():
    """网关错误帧意味着要消费答案的那一轮已经死了,提问必须退休,否则输入框锁死无解。"""
    async def fake_send(text):
        pass

    app = EchoTUI(send_coro=fake_send, session_key="s1")
    async with app.run_test() as pilot:
        from echo_agent.cli.tui.prompt_input import PromptInput
        app.on_cognitive(_clarify_ev("c1", "选哪个?", ["A", "B"]))
        blk = app._pending_clarify
        app.on_error("上游炸了")
        await pilot.pause()
        assert app._pending_clarify is None
        assert blk.cancelled is True
        assert app.query_one(PromptInput).disabled is False


@pytest.mark.asyncio
async def test_disconnect_retires_pending_clarify():
    """断连时网关会合成 /__clarify_cancel__,客户端同步退休提问并交还输入框。"""
    async def fake_send(text):
        pass

    app = EchoTUI(send_coro=fake_send, session_key="s1")
    async with app.run_test() as pilot:
        from echo_agent.cli.tui.prompt_input import PromptInput
        app.on_cognitive(_clarify_ev("c1", "选哪个?", ["A", "B"]))
        app.notify_disconnected()
        await pilot.pause()
        assert app._pending_clarify is None
        assert app._clarify_free_input is False
        assert app.query_one(PromptInput).disabled is False


@pytest.mark.asyncio
async def test_blank_free_text_answer_is_not_sent():
    """空白答案不发送:服务端会把它当成真实的空回答交给模型,模型什么也没
    学到,只会把同一个问题再问一遍——而屏幕上这条已经显示"已选"了。此时应
    保留提问,让用户重新作答。"""
    sent: list[str] = []

    async def fake_send(text):
        sent.append(text)

    app = EchoTUI(send_coro=fake_send, session_key="s1")
    async with app.run_test():
        app.on_cognitive(_clarify_ev("c1", "选哪个?", ["A", "B"]))
        blk = app._pending_clarify
        await app._answer_clarify("   ")
        assert sent == []
        # 提问仍然存活且未被标记为已选。
        assert app._pending_clarify is blk
        assert blk.answer is None
        # 正常答案照旧发送。
        await app._answer_clarify("A")
        assert sent == ["/clarify c1 A"]
        assert app._pending_clarify is None


@pytest.mark.asyncio
async def test_clarify_options_sent_as_string_render_as_real_choices():
    """网关若把 options 作为字符串字面量下发(旧服务端/重放帧),客户端必须
    还原成真实选项,而不是逐字符拆成一堆不可见的标点选项。"""
    async def fake_send(text):
        pass

    app = EchoTUI(send_coro=fake_send, session_key="s1")
    async with app.run_test():
        app.on_cognitive(_clarify_ev("c1", "粒度?", "['全部清3项','只清1项']"))
        blk = app._pending_clarify
        assert blk is not None
        assert blk.options == ["全部清3项", "只清1项"]
