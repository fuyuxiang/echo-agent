"""交互回归：逐条锁定一次 TUI 走查中确认的缺陷。

每个用例对应一个真实可复现的故障场景，命名以 test_<症状> 表述用户看到的现象，
而非实现细节，这样后续重构时失败信息能直接说明"用户又遇到什么了"。
"""

import pytest

from echo_agent.cli.tui.app import EchoTUI
from echo_agent.cli.tui.blocks import ApprovalBlock, ChoiceBlock, ToolCallBlock
from echo_agent.cli.tui.prompt_input import PromptInput
from echo_agent.cli.tui.protocol import CogEvent


def _clarify_ev(options, clarify_id="c1"):
    return CogEvent(
        "clarify_request", "e1", "in1",
        {"clarify_id": clarify_id, "question": "选哪个?", "options": options},
        "选哪个?",
    )


def _approval_ev(request_id="r1", params=None):
    return CogEvent(
        "approval_request", "a1", "in1",
        {"request_id": request_id, "action": "exec",
         "params": params or {}, "risk": "高风险"},
        "需要确认",
    )


def _tool_ev(status="running", tool_call_id="tc1", **extra):
    data = {"tool_call_id": tool_call_id, "name": "exec",
            "params": {"command": "sleep 5"}, "status": status}
    data.update(extra)
    return CogEvent("tool_call", f"t_{status}", "in1", data, "执行")


# ── Ctrl+D 退出 ────────────────────────────────────────────────────────────────

@pytest.mark.asyncio
async def test_ctrl_d_exits_even_though_textarea_binds_it_to_delete():
    # TextArea 自带 "delete,ctrl+d -> delete_right"，而输入框默认持有焦点，
    # 因此 App 级 ctrl+d 必须 priority=True，否则退出键只会删掉一个字符。
    app = EchoTUI()
    async with app.run_test() as pilot:
        prompt = app.query_one(PromptInput)
        prompt.text = "abcd"
        prompt.move_cursor((0, 0))
        await pilot.press("ctrl+d")
        await pilot.pause()
        assert app.is_running is False
        assert prompt.text == "abcd"  # 没有被当成 delete_right 吃掉


@pytest.mark.asyncio
async def test_ctrl_d_exits_from_empty_prompt():
    app = EchoTUI()
    async with app.run_test() as pilot:
        await pilot.press("ctrl+d")
        await pilot.pause()
        assert app.is_running is False


# ── clarify 待答时的键盘可用性 ─────────────────────────────────────────────────

@pytest.mark.asyncio
async def test_clarify_number_key_still_works_after_tab_moves_focus():
    # transcript 里的工具/认知块可聚焦，按一次 Tab 焦点就会落到它们身上。
    # 旧门禁要求 focused is None，于是数字键被过滤、输入框又是 disabled，
    # 用户彻底无法回答澄清。
    sent: list[str] = []

    async def fake_send(t):
        sent.append(t)

    app = EchoTUI(send_coro=fake_send)
    async with app.run_test() as pilot:
        app.on_cognitive(_tool_ev())
        app.on_cognitive(_clarify_ev(["方案A", "方案B"]))
        await pilot.pause()
        await pilot.press("tab")
        await pilot.pause()
        assert app.focused is not None  # 焦点确实离开了输入框
        await pilot.press("2")
        await pilot.pause()
        assert sent == ["/clarify c1 方案B"]


@pytest.mark.asyncio
async def test_clarify_free_text_still_works_after_tab_moves_focus():
    sent: list[str] = []

    async def fake_send(t):
        sent.append(t)

    app = EchoTUI(send_coro=fake_send)
    async with app.run_test() as pilot:
        app.on_cognitive(_tool_ev())
        app.on_cognitive(_clarify_ev(["A", "B"]))
        await pilot.pause()
        await pilot.press("tab")
        await pilot.press("x")
        await pilot.pause()
        assert app._clarify_free_input is True
        assert app.query_one(PromptInput).text == "x"
        await pilot.press("enter")
        await pilot.pause()
        assert sent == ["/clarify c1 x"]


# ── turn 结束后的进度残留 ──────────────────────────────────────────────────────

@pytest.mark.asyncio
async def test_progress_line_settles_once_reply_lands():
    # 心跳曾按 inbound_event_id 挂进 transcript：位置在第一次心跳时就定死，
    # 之后的工具行追加在它下面，"还在处理"于是显示在已完成内容的上方，而且
    # 每一轮都在答案旁留一句残留。现在进度是输入框上方那一行停靠状态，
    # 心跳只更新它的阶段标签。
    #
    # 答案落地后这一行不再隐藏，而是留下一句终态。此前 stop() 直接把整行
    # display 置 False，屏幕上唯一在动的东西连同用时一起消失、行高塌成 0，
    # "处理完了"和"卡住了"于是长得一模一样——用户实测反馈正是"刚才哗哗地动，
    # 现在突然不动了，不知道是结束了还是卡住了"。
    app = EchoTUI()
    async with app.run_test() as pilot:
        before = len(app._tv.children)
        app.on_cognitive(CogEvent(
            "heartbeat", "h1", "in1", {"stage": "thinking"}, "x"))
        app.on_cognitive(CogEvent(
            "heartbeat", "h2", "in1", {"stage": "generating"}, "x"))
        await pilot.pause()
        al = app._activity
        assert al.is_active is True
        assert "正在组织答案" in al.render_text()
        # 心跳不再往 transcript 里挂任何东西
        assert len(app._tv.children) == before
        app.on_user_reply_final("in1", "答案")
        await pilot.pause()
        assert al.is_active is False
        # 行仍在屏幕上，且明确说明这一轮已经结束
        assert al.is_settled is True
        assert al.display is True
        assert "完成" in al.render_text()
        # 不再显示过时的阶段标签，也不再提示可以中断
        assert "正在组织答案" not in al.render_text()
        assert "Ctrl+C" not in al.render_text()


@pytest.mark.asyncio
async def test_running_tool_line_settles_when_turn_dies_with_error():
    # 网关 error 帧终结了这一轮，但工具行仍渲染成 "…"，读起来像命令还在跑。
    app = EchoTUI()
    async with app.run_test() as pilot:
        app.on_cognitive(_tool_ev())
        await pilot.pause()
        app.on_error("rate limited")
        await pilot.pause()
        block = next(w for w in app._tv.children if isinstance(w, ToolCallBlock))
        assert block.status == "interrupted"
        assert "未完成" in block.render_summary()


@pytest.mark.asyncio
async def test_running_tool_line_kept_alive_across_intermediate_final():
    # 一轮回答会被拆成多个 is_final 帧（文本→工具→再文本），因此正常回复路径
    # 不能把仍在执行的工具标成"未完成"，否则真正的 done 帧会另起一行重复显示。
    app = EchoTUI()
    async with app.run_test() as pilot:
        app.on_cognitive(_tool_ev())
        app.on_user_reply_final("in1", "先说一句")
        await pilot.pause()
        block = next(w for w in app._tv.children if isinstance(w, ToolCallBlock))
        assert block.status == "running"
        app.on_cognitive(_tool_ev(status="ok", result_meta={"total_lines": 1}))
        await pilot.pause()
        blocks = [w for w in app._tv.children if isinstance(w, ToolCallBlock)]
        assert len(blocks) == 1  # 原地翻转，没有重复行
        assert blocks[0].status == "ok"


@pytest.mark.asyncio
async def test_completed_tool_result_never_overwritten_by_cleanup():
    app = EchoTUI()
    async with app.run_test() as pilot:
        app.on_cognitive(_tool_ev())
        app.on_cognitive(_tool_ev(status="ok", result_meta={"total_lines": 42}))
        app.on_error("boom")
        await pilot.pause()
        block = next(w for w in app._tv.children if isinstance(w, ToolCallBlock))
        assert block.status == "ok"


# ── 断线提示 ───────────────────────────────────────────────────────────────────

@pytest.mark.asyncio
async def test_disconnect_notice_shown_once_per_drop():
    # attach_client 有三个调用点（send/interrupt/pump 退出），一次真实掉线
    # 会重复触发，旧实现每次都追加一条相同提示。
    app = EchoTUI()
    async with app.run_test() as pilot:
        app.notify_disconnected()
        app.notify_disconnected()
        app.notify_disconnected()
        await pilot.pause()
        notices = [
            w for w in app._tv.children
            if getattr(w, "is_status", False) and "连接已断开" in w.text
        ]
        assert len(notices) == 1


@pytest.mark.asyncio
async def test_reconnect_then_drop_notifies_again():
    # 去重是"每次掉线一条"，不是"整个会话一条"。
    app = EchoTUI()
    async with app.run_test() as pilot:
        app.notify_disconnected()
        app.notify_reconnected()
        app.notify_disconnected()
        await pilot.pause()
        notices = [
            w for w in app._tv.children
            if getattr(w, "is_status", False) and "连接已断开" in w.text
        ]
        assert len(notices) == 2


@pytest.mark.asyncio
async def test_idle_disconnect_keeps_the_previous_turn_settled_as_done():
    # settle() 有意允许后来的终态覆盖先前的（网关报错后通常紧跟一次断线，
    # 更具体的原因应当胜出），但断线处此前是无条件调用：用户空闲时掉线，
    # 会把上一轮已经"完成"的那行改写成"连接已断开"，而它上方那条答案早已
    # 送达并仍在屏幕上——这一行是在跟记录本身矛盾。
    app = EchoTUI()
    async with app.run_test() as pilot:
        app._activity.start()
        app._activity.settle("done")
        await pilot.pause()
        assert app._activity.is_settled
        assert not app._activity.is_active

        app.notify_disconnected()
        await pilot.pause()
        # 终态仍是"完成"，只有状态栏与提示反映连接已断开。
        assert app._activity._outcome == "done"


@pytest.mark.asyncio
async def test_disconnect_while_a_turn_is_running_still_settles_as_disconnected():
    # 正例：回合确实在进行时，断线必须收尾，否则进度行会一直"转"下去，
    # 而这条 socket 上再也不会有帧到来。
    app = EchoTUI()
    async with app.run_test() as pilot:
        app._activity.start()
        await pilot.pause()
        assert app._activity.is_active

        app.notify_disconnected()
        await pilot.pause()
        assert not app._activity.is_active
        assert app._activity._outcome == "disconnected"


@pytest.mark.asyncio
async def test_idle_disconnect_on_a_fresh_screen_invents_no_summary():
    # 没有跑过任何回合时掉线，不应凭空出现一行终态摘要。
    app = EchoTUI()
    async with app.run_test() as pilot:
        app.notify_disconnected()
        await pilot.pause()
        assert not app._activity.is_settled
        assert app._activity._outcome == ""


# ── 主题切换 ───────────────────────────────────────────────────────────────────

@pytest.mark.asyncio
async def test_theme_switch_repaints_existing_markdown_replies():
    # set_markdown 会把主题色固化进 Rich renderable，所以切到浅色后，屏幕上
    # 已有回复仍是深色调色板 —— 白底上的低对比灰正是浅色主题要修的问题。
    app = EchoTUI()
    async with app.run_test() as pilot:
        app.on_user_reply_final("in1", "# 标题\n正文")
        await pilot.pause()
        reply = [w for w in app._tv.children if type(w).__name__ == "AgentReply"][-1]

        def baked_palette():
            grid = getattr(reply, "_Static__content")
            return grid.columns[1]._cells[0]._palette

        assert baked_palette()["muted"] == "#8b949e"   # 深色
        app._do_theme("light")
        await pilot.pause()
        assert baked_palette()["muted"] == "#5a6472"   # 浅色


@pytest.mark.asyncio
async def test_theme_switch_leaves_streaming_text_intact():
    # 流式文本走 markup 路径，本身会重解析 $var，repaint 不该把它变成 markdown。
    app = EchoTUI()
    async with app.run_test() as pilot:
        app.on_user_reply_token("in1", "半截文本")
        await pilot.pause()
        streaming = app._replies["in1"]
        app._do_theme("light")
        await pilot.pause()
        assert streaming.text == "半截文本"
        assert streaming._is_markdown is False


# ── 本地命令分发 ───────────────────────────────────────────────────────────────

@pytest.mark.asyncio
async def test_uppercase_local_command_is_not_sent_upstream():
    # 补全面板用 lower() 匹配，会把 /HELP 提示成 /help；分发却按精确比较，
    # 于是这条命令被当成一轮对话发给了服务端。
    sent: list[str] = []

    async def fake_send(t):
        sent.append(t)

    app = EchoTUI(send_coro=fake_send)
    async with app.run_test() as pilot:
        app.post_message(PromptInput.Submitted("/HELP"))
        await pilot.pause()
        assert sent == []
        app.post_message(PromptInput.Submitted("/Theme light"))
        await pilot.pause()
        assert app.theme == "echo-light"
        assert sent == []


@pytest.mark.asyncio
async def test_reconnect_reachable_while_clarify_pending():
    # 澄清待答期间掉线，旧实现把 /reconnect 当成澄清答案原样上行，
    # 用户被锁在一个连不上的澄清里。
    sent: list[str] = []

    async def fake_send(t):
        sent.append(t)

    reconnected: list[bool] = []

    async def fake_reconnect():
        reconnected.append(True)
        return True

    app = EchoTUI(send_coro=fake_send, reconnect_coro=fake_reconnect)
    async with app.run_test() as pilot:
        app.on_cognitive(_clarify_ev(["A", "B"]))
        app.notify_disconnected()
        await pilot.pause()
        app.post_message(PromptInput.Submitted("/reconnect"))
        await pilot.pause()
        assert reconnected == [True]
        assert not any(s.startswith("/clarify") for s in sent)


@pytest.mark.asyncio
async def test_ordinary_clarify_answer_still_reaches_the_clarify():
    # 上一条放行的是逃生命令，普通答案（含以 / 开头的服务端命令之外的文本）
    # 必须仍然作为澄清答案送出。
    sent: list[str] = []

    async def fake_send(t):
        sent.append(t)

    app = EchoTUI(send_coro=fake_send)
    async with app.run_test() as pilot:
        app.on_cognitive(_clarify_ev(["A", "B"]))
        await pilot.pause()
        await pilot.press("z")
        await pilot.pause()
        app.query_one(PromptInput).text = "全删"
        app.post_message(PromptInput.Submitted("全删"))
        await pilot.pause()
        assert sent == ["/clarify c1 全删"]


# ── /clear 与待决状态 ──────────────────────────────────────────────────────────

@pytest.mark.asyncio
async def test_clear_keeps_pending_approval_so_prompt_lock_is_explained():
    # 旧实现清掉了 ApprovalBlock 却保留 _pending_approval，用户面对一个空屏幕
    # 和一个禁用的输入框，没有任何线索说明该按什么键，服务端还在等决定。
    sent: list[str] = []

    async def fake_send(t):
        sent.append(t)

    app = EchoTUI(send_coro=fake_send)
    async with app.run_test() as pilot:
        app._tv.add_user("旧对话")
        app.on_user_reply_final("in0", "旧回复")
        app.on_cognitive(_approval_ev())
        await pilot.pause()
        app._do_clear()
        await pilot.pause()
        kept = [w for w in app._tv.children if isinstance(w, ApprovalBlock)]
        assert len(kept) == 1
        assert not any(type(w).__name__ == "UserTurn" for w in app._tv.children)
        await pilot.press("y")
        await pilot.pause()
        assert sent == ["/approve r1"]


@pytest.mark.asyncio
async def test_clear_keeps_pending_clarify_with_its_answer_values():
    # 待决块是原地保留而非重建，所以 dict 选项的真实答案值不会丢。
    sent: list[str] = []

    async def fake_send(t):
        sent.append(t)

    app = EchoTUI(send_coro=fake_send)
    async with app.run_test() as pilot:
        app.on_cognitive(_clarify_ev([{"value": "A", "description": "详细说明"}]))
        await pilot.pause()
        app._do_clear()
        await pilot.pause()
        assert len([w for w in app._tv.children if isinstance(w, ChoiceBlock)]) == 1
        await pilot.press("1")
        await pilot.pause()
        assert sent == ["/clarify c1 A"]  # 送出裸 value，不带描述


@pytest.mark.asyncio
async def test_clear_without_pending_state_empties_the_screen():
    app = EchoTUI()
    async with app.run_test() as pilot:
        app._tv.add_user("问题")
        app.on_user_reply_final("in1", "答案")
        await pilot.pause()
        app._do_clear()
        await pilot.pause()
        assert list(app._tv.children) == []
        assert app._replies == {}


# ── 排队确认 ───────────────────────────────────────────────────────────────────

@pytest.mark.asyncio
async def test_queue_guard_does_not_duplicate_history_or_reset_cursor():
    # 回填草稿旧实现用 .text=，既让 _submit 记过的历史再记一次，又因为
    # load_text 把光标复位到 (0,0) —— 用户的光标跳到句首，上下键还会误触历史。
    sent: list[str] = []

    async def fake_send(t):
        sent.append(t)

    app = EchoTUI(send_coro=fake_send)
    async with app.run_test() as pilot:
        prompt = app.query_one(PromptInput)
        prompt.text = "第一轮"
        await pilot.press("enter")
        await pilot.pause()
        app.on_turn_accepted("ev1")

        prompt.text = "继续补充"
        await pilot.press("enter")
        await pilot.pause()
        assert sent == ["第一轮"]                       # 尚未发出
        assert prompt.text == "继续补充"                 # 文本留在框里
        assert prompt._history == ["第一轮"]             # 没有提前记进历史
        assert prompt.cursor_location == (0, 4)         # 光标在文末

        await pilot.press("enter")
        await pilot.pause()
        assert sent == ["第一轮", "继续补充"]
        assert prompt._history == ["第一轮", "继续补充"]  # 只记一次


# ── clarify 选项编号 ───────────────────────────────────────────────────────────

def test_only_first_nine_options_are_numbered():
    # 快捷键只到 9，且按 "1" 会立即选中，无法输入 "10"。给第 10 项标数字
    # 等于承诺一个用不了的快捷键 —— 当选项 ≥9 时连"其他"逃生入口都像点不到。
    block = ChoiceBlock("c1", "选?", [f"选项{i}" for i in range(12)])
    body = block.render_body()
    assert "9. 选项8" in body
    assert "10." not in body
    assert "13." not in body
    assert "选项9" in body            # 仍然渲染，只是不带编号
    assert "其他（自行输入）" in body
    assert "前 9 项可按数字选择" in body


def test_short_option_list_keeps_full_numbering():
    block = ChoiceBlock("c1", "选?", ["A", "B"])
    body = block.render_body()
    assert "1. A" in body and "2. B" in body
    assert "3. 其他（自行输入）" in body
    assert "按数字选择" in body


# ── 参数渲染与脱敏 ─────────────────────────────────────────────────────────────

def test_approval_panel_masks_credential_shaped_params():
    # 这是用户授权高风险操作的那一屏，旧实现直接 str(dict)：长命令撑爆行宽，
    # 凭据原样上屏，/save 还会把它们写进磁盘。
    block = ApprovalBlock(
        "r1", "exec",
        {"command": "psql -U admin", "API_KEY": "sk-abcdef123456",
         "db_password": "hunter2", "note": "ok"},
        "高风险",
    )
    body = block._body()
    assert "sk-abcdef123456" not in body
    assert "hunter2" not in body
    assert "••••3456" in body          # 保留尾 4 位以便区分不同凭据
    assert "command=psql -U admin" in body
    assert "note=ok" in body
    assert "params={" not in body      # 不再是 Python repr


def test_tool_detail_masks_credential_shaped_params():
    block = ToolCallBlock(
        "tc1", "web_fetch",
        {"url": "https://x.com", "authorization": "Bearer supersecret"},
    )
    detail = block.render_detail()
    assert "supersecret" not in detail
    assert "url=https://x.com" in detail


# ── 导出与内存 ─────────────────────────────────────────────────────────────────

@pytest.mark.asyncio
async def test_copy_all_excludes_client_local_notices():
    # 本地提示带手写 Rich 标记，旧实现让它们以 "[$text-muted]…[/]" 原样进剪贴板。
    app = EchoTUI()
    async with app.run_test() as pilot:
        app._tv.add_user("问题")
        app._tv.add_notice("[$text-muted]提示文本[/]")
        app.on_user_reply_final("in1", "答案")
        await pilot.pause()
        exported = app._tv.export_text()
        assert exported == "❯ 问题\n\n答案"
        assert "text-muted" not in exported


@pytest.mark.asyncio
async def test_save_treats_notice_only_screen_as_empty():
    # /save 的判空走 export_text，过滤 status 行后，只有 /help 输出的会话
    # 会被正确判定为"无可保存内容"，而不是存出一个只有元数据头的空文件。
    app = EchoTUI()
    async with app.run_test() as pilot:
        app._tv.add_notice("[$text-muted]帮助输出[/]")
        await pilot.pause()
        assert app._tv.export_text().strip() == ""


@pytest.mark.asyncio
async def test_reply_correlation_cleared_when_turn_dies():
    # 只有 on_user_reply_final 会 pop，被 error/重连终结的轮次会永久留下条目。
    app = EchoTUI()
    async with app.run_test() as pilot:
        for i in range(20):
            app.on_user_reply_token(f"in{i}", "tok")
        await pilot.pause()
        assert len(app._replies) == 20
        app.on_error("boom")
        await pilot.pause()
        assert app._replies == {}

        app.on_user_reply_token("in99", "tok")
        app.notify_reconnected()
        await pilot.pause()
        assert app._replies == {}


# ── 输入框高度 ─────────────────────────────────────────────────────────────────

def _screen_text(app, width=80, height=24):
    import io

    from rich.console import Console

    console = Console(file=io.StringIO(), width=width, height=height)
    console.print(app.screen._compositor)
    return console.file.getvalue()


@pytest.mark.asyncio
@pytest.mark.parametrize("rows", [8, 9, 10])
async def test_line_being_typed_stays_visible_as_the_box_grows(rows):
    # #input_row 的 max-height 含上下两条边框，旧值 10 只留下 8 行正文区，
    # 而 PromptInput 会长到 10 行 —— 第 9 行起用户就在盲打。
    app = EchoTUI()
    async with app.run_test(size=(80, 24)) as pilot:
        for i in range(rows):
            await pilot.press(*list(f"MARK{i}"))
            if i < rows - 1:
                await pilot.press("shift+enter")
        await pilot.pause()
        assert f"MARK{rows - 1}" in _screen_text(app)


# ── /details 过程信息显示 ──────────────────────────────────────────────────────

@pytest.mark.asyncio
async def test_details_command_reports_all_three_sections():
    # 无参也回报当前设置：这条回复同时充当"能改什么"的可发现清单，
    # 语法猜错的人照样能学到用法。
    app = EchoTUI()
    async with app.run_test() as pilot:
        app.post_message(PromptInput.Submitted("/details"))
        await pilot.pause()
        text = _screen_text(app)
        for label in ("思考与记忆", "工具调用", "运行状态"):
            assert label in text


@pytest.mark.asyncio
async def test_details_command_changes_what_later_frames_render():
    app = EchoTUI()
    async with app.run_test() as pilot:
        app.post_message(PromptInput.Submitted("/details 工具 隐藏"))
        await pilot.pause()
        before = len([w for w in app._tv.children if isinstance(w, ToolCallBlock)])
        app.on_cognitive(_tool_ev("running"))
        await pilot.pause()
        after = len([w for w in app._tv.children if isinstance(w, ToolCallBlock)])
        assert after == before


@pytest.mark.asyncio
async def test_hidden_tools_still_announce_themselves_on_the_live_line():
    # 工具行隐藏时，页脚常驻行是用户唯一能看到"正在跑工具"的地方，
    # 所以它必须按帧的 status 驱动，而不是按有没有块。
    app = EchoTUI()
    async with app.run_test() as pilot:
        app._tv.set_details(app._tv.details.with_section("tools", "hidden"))
        app.on_cognitive(_tool_ev("running"))
        await pilot.pause()
        assert app._activity.is_active is True
        assert "执行" in app._activity.render_text()


@pytest.mark.asyncio
async def test_bad_details_argument_explains_the_syntax():
    app = EchoTUI()
    async with app.run_test() as pilot:
        app.post_message(PromptInput.Submitted("/details 工具 稍微展开"))
        await pilot.pause()
        assert "用法" in _screen_text(app)
