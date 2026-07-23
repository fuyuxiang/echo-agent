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
