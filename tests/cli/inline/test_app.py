import asyncio
import io
import json
import os

import pytest

from echo_agent.cli.inline.app import InlineApp
from echo_agent.cli.render import ansi as A
from echo_agent.cli.renderer_base import RenderSink
from echo_agent.cli.tui.details import DetailPrefs
from echo_agent.cli.tui.protocol import CogEvent


@pytest.fixture(autouse=True)
def _plain_output(monkeypatch):
    A.set_color_override(False)
    monkeypatch.delenv("ECHO_TUI_DETAILS", raising=False)
    yield
    A.set_color_override(None)


def _app(*, send=None, interrupt=None, reconnect=None, save_dir=None):
    buf = io.StringIO()
    app = InlineApp(
        send_coro=send,
        interrupt_coro=interrupt,
        reconnect_coro=reconnect,
        session_key="cli:test",
        save_dir=save_dir,
        stream=buf,
    )
    return app, buf


def _tool(status, *, tcid="t1", name="edit_file", params=None, **extra):
    data = {
        "tool_call_id": tcid,
        "name": name,
        "params": {"path": "/tmp/example.py"} if params is None else params,
        "status": status,
        **extra,
    }
    return CogEvent("tool_call", f"cog-{tcid}-{status}", "turn-1", data, "")


def test_inline_app_satisfies_renderer_contract():
    app, _ = _app()
    assert isinstance(app, RenderSink)


@pytest.mark.asyncio
async def test_streaming_draft_is_never_printed_and_final_appears_once():
    sent = []

    async def send(text):
        sent.append(text)

    app, buf = _app(send=send)
    await app.submit("你好")
    app.on_turn_accepted("turn-1")
    app.on_user_reply_token("turn-1", "草稿不应出现")
    app.on_user_reply_reset("turn-1")
    app.on_user_reply_token("turn-1", "另一份草稿")
    app.on_user_reply_token("turn-1", "x" * 300)
    assert len(app._drafts["turn-1"]) == 256
    app.on_user_reply_final("turn-1", "**最终答案**")
    app.on_user_reply_final("turn-1", "**最终答案**")

    out = buf.getvalue()
    assert sent == ["你好"]
    assert "草稿不应出现" not in out
    assert "另一份草稿" not in out
    assert out.count("最终答案") == 1


@pytest.mark.asyncio
async def test_empty_terminal_reply_still_settles_activity_and_turn():
    app, _ = _app()
    await app.submit("开始")
    app.on_turn_accepted("turn-1")
    app.on_user_reply_final("turn-1", "")
    assert app._turns.has_active_primary is False
    assert app._turn_started == 0.0


@pytest.mark.asyncio
async def test_tool_running_and_done_render_one_stable_tool_block():
    app, buf = _app()
    await app.submit("修改文件")
    app.on_turn_accepted("turn-1")
    app.on_cognitive(_tool("running"))
    app.on_cognitive(_tool("ok", params={}, result_meta={"changed": 1}))

    out = buf.getvalue()
    assert out.count("编辑 example.py") == 1
    assert "✓" in out


@pytest.mark.asyncio
async def test_tool_done_inherits_params_from_running_frame():
    app, buf = _app()
    await app.submit("修改")
    app.on_turn_accepted("turn-1")
    app.on_cognitive(_tool("running", params={"path": "/tmp/kept.py"}))
    app.on_cognitive(_tool("ok", params={}))
    assert "编辑 kept.py" in buf.getvalue()


@pytest.mark.asyncio
async def test_default_process_view_shows_successful_read_and_failure():
    app, buf = _app()
    await app.submit("读取")
    app.on_turn_accepted("turn-1")
    app.on_cognitive(_tool("running", tcid="r1", name="read_file"))
    app.on_cognitive(_tool("ok", tcid="r1", name="read_file"))
    app.on_cognitive(_tool("fail", tcid="r2", name="read_file", result_text="boom"))
    out = buf.getvalue()
    assert out.count("读取 example.py") == 2
    assert "✓" in out
    assert "✗" in out


@pytest.mark.asyncio
async def test_explicit_lean_mode_still_hides_successful_read_but_shows_failure():
    app, buf = _app()
    app._details = DetailPrefs(tools="lean")
    await app.submit("读取")
    app.on_turn_accepted("turn-1")
    app.on_cognitive(_tool("running", tcid="r1", name="read_file"))
    app.on_cognitive(_tool("ok", tcid="r1", name="read_file"))
    app.on_cognitive(_tool(
        "fail", tcid="r2", name="read_file", result_text="boom",
    ))
    out = buf.getvalue()
    assert out.count("读取 example.py") == 1
    assert "✗" in out


@pytest.mark.asyncio
async def test_parallel_tool_results_repeat_identity_for_correlation():
    app, buf = _app()
    await app.submit("并行读取")
    app.on_turn_accepted("turn-1")
    app.on_cognitive(_tool(
        "running", tcid="r1", name="read_file", params={"path": "/tmp/a.py"},
    ))
    app.on_cognitive(_tool(
        "running", tcid="r2", name="read_file", params={"path": "/tmp/b.py"},
    ))
    app.on_cognitive(_tool(
        "ok", tcid="r2", name="read_file", params={},
        result_meta={"total_lines": 20},
    ))
    app.on_cognitive(_tool(
        "ok", tcid="r1", name="read_file", params={},
        result_meta={"total_lines": 10},
    ))
    out = buf.getvalue()
    assert "读取 b.py · 20 行" in out
    assert "读取 a.py · 10 行" in out


@pytest.mark.asyncio
async def test_approval_ack_is_hidden_and_does_not_settle_primary_turn():
    sent = []

    async def send(text):
        sent.append(text)

    app, buf = _app(send=send)
    await app.submit("执行")
    app.on_turn_accepted("turn-1")
    app.on_cognitive(CogEvent(
        "approval_request", "a1", "turn-1",
        {"request_id": "req-1", "action": "shell", "params": {}, "risk": "EXEC"},
        "",
    ))
    await app.submit("y")
    app.on_turn_accepted("control-1")
    app.on_user_reply_final("control-1", "approval accepted")

    assert sent == ["执行", "/approve req-1"]
    assert "approval accepted" not in buf.getvalue()
    assert app._turns.active_turn_id == "turn-1"


@pytest.mark.asyncio
async def test_clarify_number_sends_underlying_dict_value():
    sent = []

    async def send(text):
        sent.append(text)

    app, buf = _app(send=send)
    app.on_cognitive(CogEvent(
        "clarify_request", "q1", "turn-1",
        {"clarify_id": "clar-1", "question": "选哪个？", "options": [
            {"value": "safe", "description": "仅读"}, "fast",
        ]},
        "",
    ))
    await app.submit("1")
    assert sent == ["/clarify clar-1 safe"]
    assert "safe — 仅读" in buf.getvalue()
    assert "已回答：safe" in buf.getvalue()


@pytest.mark.asyncio
async def test_queue_guard_requires_a_second_submit():
    sent = []

    async def send(text):
        sent.append(text)

    app, buf = _app(send=send)
    await app.submit("第一轮")
    app.on_turn_accepted("turn-1")
    await app.submit("第二轮")
    assert sent == ["第一轮"]
    assert "排队" in buf.getvalue()
    assert app._prompt_default == "第二轮"
    await app.submit("第二轮")
    assert sent == ["第一轮", "第二轮"]


@pytest.mark.asyncio
async def test_ctrl_c_targets_primary_not_later_control_event():
    interrupted = []

    async def send(_text):
        pass

    async def interrupt(event_id):
        interrupted.append(event_id)

    app, _ = _app(send=send, interrupt=interrupt)
    await app.submit("开始")
    app.on_turn_accepted("primary")
    await app.submit("/approvals")
    app.on_turn_accepted("control")
    await app.handle_ctrl_c()
    assert interrupted == ["primary"]


@pytest.mark.asyncio
async def test_interrupted_running_tool_settles_once_when_final_arrives():
    interrupted = []

    async def interrupt(event_id):
        interrupted.append(event_id)

    app, buf = _app(interrupt=interrupt)
    await app.submit("开始")
    app.on_turn_accepted("turn-1")
    app.on_cognitive(_tool("running"))
    await app.handle_ctrl_c()
    app.on_user_reply_final("turn-1", "已停止")
    out = buf.getvalue()
    assert interrupted == ["turn-1"]
    assert out.count("编辑 example.py") == 1
    assert "未完成" in out


@pytest.mark.asyncio
async def test_disconnect_blocks_messages_and_manual_reconnect_restores_input():
    sent = []
    reconnects = []

    async def send(text):
        sent.append(text)

    async def reconnect():
        reconnects.append(True)
        return True

    app, buf = _app(send=send, reconnect=reconnect)
    app.notify_disconnected()
    await app.submit("不应发送")
    await app.submit("/reconnect")
    await app.submit("恢复后发送")
    assert reconnects == [True]
    assert sent == ["恢复后发送"]
    assert "连接已断开" in buf.getvalue()
    assert "已重新连接" in buf.getvalue()


@pytest.mark.asyncio
async def test_non_tty_activity_emits_only_one_static_progress_line_per_turn():
    app, buf = _app()
    await app.submit("开始")
    app.on_turn_accepted("turn-1")
    app.on_user_reply_token("turn-1", "a")
    app.on_user_reply_token("turn-1", "b")
    app.on_cognitive(CogEvent(
        "heartbeat", "h1", "turn-1", {"stage": "thinking"}, "",
    ))
    progress_lines = [line for line in buf.getvalue().splitlines() if "正在" in line]
    assert progress_lines == ["正在分析请求"]


@pytest.mark.asyncio
async def test_approval_pauses_motion_without_resetting_whole_turn_timer():
    app, _ = _app()
    await app.submit("执行")
    started = app._turn_started
    app.on_turn_accepted("turn-1")
    app.on_cognitive(CogEvent(
        "approval_request", "a1", "turn-1",
        {"request_id": "req-1", "action": "exec", "params": {}, "risk": "EXEC"},
        "",
    ))
    assert app._activity_task is None
    assert app._turn_started == started
    assert app._turn_elapsed == 0


def test_wide_toolbar_restores_session_telemetry(monkeypatch):
    app, _ = _app()
    monkeypatch.setattr(
        "echo_agent.cli.inline.app.shutil.get_terminal_size",
        lambda fallback: os.terminal_size((140, 24)),
    )
    app._status.update({
        "model": "MiniMax-M3",
        "context_used": 57_300,
        "context_max": 1_000_000,
        "total_cost": 0.0042,
        "memory_count": 769,
    })
    text = "".join(value for _style, value in app._toolbar())
    assert "●已连接 cli:test" in text
    assert "⚡ MiniMax-M3" in text
    assert "57.3K/1.0M" in text
    assert "6%" in text
    assert "$0.0042" in text
    assert "🧠 769" in text


def test_initial_status_is_visible_before_first_model_round(monkeypatch):
    monkeypatch.setattr(
        "echo_agent.cli.inline.app.shutil.get_terminal_size",
        lambda fallback: os.terminal_size((140, 24)),
    )
    app = InlineApp(
        session_key="cli:test", stream=io.StringIO(),
        initial_status={"model": "MiniMax-M3", "context_max": 1_000_000},
    )
    text = "".join(value for _style, value in app._toolbar())
    assert "MiniMax-M3" in text
    assert "0/1.0M" in text


def test_mid_toolbar_keeps_memory_visible(monkeypatch):
    app, _ = _app()
    monkeypatch.setattr(
        "echo_agent.cli.inline.app.shutil.get_terminal_size",
        lambda fallback: os.terminal_size((100, 24)),
    )
    app._status.update({
        "model": "gpt-5.5",
        "context_max": 128_000,
        "memory_count": 47,
        "total_cost": 0.0042,
    })
    text = "".join(value for _style, value in app._toolbar())
    assert "●已连接" in text
    assert "⚡ gpt-5.5" in text
    assert "上下文 0%" in text
    assert "🧠 47" in text
    assert "$0.0042" not in text


def test_narrow_toolbar_prioritizes_connection_and_activity(monkeypatch):
    app, _ = _app()
    monkeypatch.setattr(
        "echo_agent.cli.inline.app.shutil.get_terminal_size",
        lambda fallback: os.terminal_size((48, 24)),
    )
    monkeypatch.setattr("echo_agent.cli.inline.app.time.monotonic", lambda: 20.0)
    app._turn_started = 10.0
    app._activity_label = "正在读取 very-long-file-name.py"
    text = "".join(value for _style, value in app._toolbar())
    assert "●已连接" in text
    assert "⏱ 10s" in text
    # The spinner owns the live stage; repeating it in the adjacent toolbar
    # would produce two competing "正在读取" rows.
    assert "正在读取" not in text
    assert "上下文" not in text and "🧠" not in text and "$" not in text


@pytest.mark.asyncio
async def test_interactive_activity_uses_prompt_renderer_without_ansi_writes():
    class TTYBuffer(io.StringIO):
        def isatty(self):
            return True

    class PromptApp:
        def __init__(self):
            self.invalidations = 0

        def invalidate(self):
            self.invalidations += 1

    class PromptSession:
        def __init__(self):
            self.app = PromptApp()

    stream = TTYBuffer()
    app = InlineApp(session_key="cli:test", stream=stream)
    session = PromptSession()
    app._prompt_session = session

    app._start_activity("正在思考")
    first = "".join(value for _style, value in app._prompt())
    await asyncio.sleep(0.12)
    second = "".join(value for _style, value in app._prompt())
    app._pause_activity()

    assert "正在思考" in first
    assert "\n" in first
    assert first != second
    assert stream.getvalue() == ""
    assert session.app.invalidations >= 2


@pytest.mark.parametrize(
    ("outcome", "expected"),
    [("done", "✓"), ("error", "✗ 出错"),
     ("interrupted", "■ 已中断"), ("disconnected", "○ 上轮断开")],
)
def test_toolbar_distinguishes_terminal_outcomes(outcome, expected):
    app, _ = _app()
    app._turn_elapsed = 2.0
    app._turn_outcome = outcome
    text = "".join(value for _style, value in app._toolbar())
    assert expected in text


def test_prompt_style_tracks_light_theme(monkeypatch):
    monkeypatch.setenv("ECHO_TUI_THEME", "light")
    light = InlineApp._prompt_style_rules()
    monkeypatch.setenv("ECHO_TUI_THEME", "dark")
    dark = InlineApp._prompt_style_rules()
    assert "bg:#f0f2f5" in light["status.base"]
    assert "bg:#161b22" in dark["status.base"]
    assert light != dark


@pytest.mark.asyncio
async def test_clear_removes_settled_summary_from_toolbar():
    app, _ = _app()
    app._turn_elapsed = 3.0
    app._turn_outcome = "error"
    app._turn_tool_ids.add("t1")
    await app.submit("/clear")
    assert app._turn_elapsed == 0
    assert app._turn_outcome == ""
    assert app._turn_tool_ids == set()


@pytest.mark.asyncio
async def test_clear_during_tool_makes_later_result_self_contained():
    app, buf = _app()
    await app.submit("读取")
    app.on_turn_accepted("turn-1")
    app.on_cognitive(_tool("running", name="read_file"))
    await app.submit("/clear")
    after_clear = len(buf.getvalue())
    app.on_cognitive(_tool(
        "ok", name="read_file", params={}, result_meta={"total_lines": 12},
    ))
    post_clear = buf.getvalue()[after_clear:]
    assert "读取 example.py" in post_clear
    assert "12 行" in post_clear


@pytest.mark.asyncio
async def test_save_json_keeps_audit_but_redacts_secrets(tmp_path):
    app, _ = _app(save_dir=tmp_path)
    await app.submit("检查")
    app.on_cognitive(_tool(
        "ok", params={"path": "/tmp/a", "api_token": "sk-secret-123456"},
    ))
    await app.submit("/save --format json audit")
    target = tmp_path / "audit.json"
    body = target.read_text(encoding="utf-8")
    parsed = json.loads(body)
    assert parsed["session_key"] == "cli:test"
    assert "sk-secret-123456" not in body
    assert "••••3456" in body


@pytest.mark.asyncio
async def test_injected_input_loop_runs_without_prompt_toolkit_terminal():
    sent = []
    values = iter(["你好", "/quit"])

    async def send(text):
        sent.append(text)

    buf = io.StringIO()
    app = InlineApp(
        send_coro=send, session_key="cli:test", stream=buf,
        input_reader=lambda: next(values),
    )
    await app.run_async()
    assert sent == ["你好"]
    assert "echo · agent" in buf.getvalue()
    assert "会话 cli:test" in buf.getvalue()


@pytest.mark.asyncio
async def test_piped_stdin_uses_control_sequence_free_fallback(monkeypatch):
    sent = []

    async def send(text):
        sent.append(text)

    monkeypatch.setattr("sys.stdin", io.StringIO("管道消息\n/quit\n"))
    app, buf = _app(send=send)
    await app.run_async()
    out = buf.getvalue()
    assert sent == ["管道消息"]
    assert "❯ 管道消息" in out
    assert "\033[" not in out
    assert "\r" not in out
