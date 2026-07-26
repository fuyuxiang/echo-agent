import pytest

from echo_agent.cli.tui.app import EchoTUI
from echo_agent.cli.tui.prompt_input import PromptInput
from echo_agent.cli.tui.protocol import CogEvent


async def _drive(app, fn):
    async with app.run_test() as pilot:
        await pilot.pause()
        await fn(pilot)


@pytest.mark.asyncio
async def test_copy_last_reply_writes_clipboard_and_not_sent():
    sent: list[str] = []

    async def fake_send(t):
        sent.append(t)

    app = EchoTUI(send_coro=fake_send)

    async def body(pilot):
        app.on_user_reply_token("in1", "第一段回复")
        app.on_user_reply_final("in1", "**完整回复** 内容")
        await pilot.pause()
        app.post_message(PromptInput.Submitted("/copy"))
        await pilot.pause()
        assert app.clipboard == "**完整回复** 内容"
        assert sent == []  # 本地命令不发上行

    await _drive(app, body)


@pytest.mark.asyncio
async def test_copy_all_exports_full_transcript():
    app = EchoTUI()

    async def body(pilot):
        app._tv.add_user("你好")
        app.on_user_reply_final("in1", "回复一")
        app._tv.add_user("再问")
        app.on_user_reply_final("in2", "回复二")
        await pilot.pause()
        app.post_message(PromptInput.Submitted("/copy all"))
        await pilot.pause()
        assert app.clipboard == "❯ 你好\n\n回复一\n\n❯ 再问\n\n回复二"

    await _drive(app, body)


@pytest.mark.asyncio
async def test_copy_excludes_heartbeat_lines():
    app = EchoTUI()

    async def body(pilot):
        app.on_user_reply_final("in1", "真正的回复")
        # 心跳行是 AgentReply 子类，必须被排除，否则 /copy 会抓到进度行
        app.on_cognitive(CogEvent("heartbeat", "e1", "in2", {"note": "处理中"}, "处理中"))
        await pilot.pause()
        assert app._tv.last_turn_reply_text() == "真正的回复"
        assert "处理中" not in app._tv.export_text()

    await _drive(app, body)


@pytest.mark.asyncio
async def test_copy_grabs_whole_last_turn_not_just_last_block():
    # 回归：一轮回答常被拆成多个 AgentReply 块（文本→工具→再文本），
    # /copy 必须拷完整一轮，而不是只拷屏幕底部最后那一块。
    app = EchoTUI()

    async def body(pilot):
        app._tv.add_user("帮我分析并执行")
        app.on_user_reply_final("in1", "先说背景，这是第一段。")
        app.on_cognitive(CogEvent("tool_call", "c1", "in1", {"name": "read_file", "params": {}}, "读取"))
        app.on_user_reply_final("in1", "分析完成，这是结论。")
        await pilot.pause()
        app.post_message(PromptInput.Submitted("/copy"))
        await pilot.pause()
        assert app.clipboard == "先说背景，这是第一段。\n\n分析完成，这是结论。"

    await _drive(app, body)


@pytest.mark.asyncio
async def test_copy_last_turn_excludes_previous_turns():
    # /copy 只取最近一轮：上一轮的问答不应混入。
    app = EchoTUI()

    async def body(pilot):
        app._tv.add_user("第一轮问题")
        app.on_user_reply_final("in1", "第一轮回答")
        app._tv.add_user("第二轮问题")
        app.on_user_reply_final("in2", "第二轮回答")
        await pilot.pause()
        app.post_message(PromptInput.Submitted("/copy"))
        await pilot.pause()
        assert app.clipboard == "第二轮回答"

    await _drive(app, body)


@pytest.mark.asyncio
async def test_copy_with_no_reply_notifies_and_skips_clipboard():
    app = EchoTUI()

    async def body(pilot):
        app.post_message(PromptInput.Submitted("/copy"))
        await pilot.pause()
        assert app.clipboard == ""  # 未写入

    await _drive(app, body)
