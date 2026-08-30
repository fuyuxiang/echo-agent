"""Scrollback-first interactive client.

The full-screen Textual application remains available with ``--tui``.  This
front-end is the default because it behaves like a normal shell program: old
output remains native terminal scrollback, selections can be copied normally,
and only one transient spinner row is ever rewritten.

Protocol callbacks are deliberately synchronous.  ``WSBridge`` invokes them in
the same asyncio loop as :meth:`run_async`, so state updates remain ordered while
input and gateway frames can interleave safely.
"""

from __future__ import annotations

import asyncio
import base64
import inspect
import json
import os
import shlex
import shutil
import subprocess
import sys
import time
from collections import deque
from dataclasses import dataclass
from datetime import datetime
from pathlib import Path
from typing import Any, Awaitable, Callable

from prompt_toolkit.completion import Completer

from echo_agent.agent.proc_lifecycle import run_owned
from echo_agent.cli.inline.printer import InlinePrinter
from echo_agent.cli.render import ansi as A
from echo_agent.cli.render.redact import (
    format_params,
    mask_sensitive_strings,
    redact_for_export,
)
from echo_agent.cli.render.text import clip
from echo_agent.cli.render.tool import humanize_tool, pick_object
from echo_agent.cli.tui.brand import load_brand
from echo_agent.cli.tui.completion import COMMANDS
from echo_agent.cli.tui.details import parse_command as parse_details, parse_env
from echo_agent.cli.tui.protocol import (
    CogEvent,
    approve_command,
    clarify_command,
    deny_command,
)
from echo_agent.cli.tui.turns import TurnRegistry

Send = Callable[[str], Awaitable[None]]
Interrupt = Callable[[str], Awaitable[None]]
Reconnect = Callable[[], Awaitable[bool]]
TurnStatus = Callable[[str], Awaitable[dict]]
InputReader = Callable[[], str | Awaitable[str]]


@dataclass
class _Approval:
    request_id: str
    action: str
    params: dict
    risk: str


@dataclass
class _Clarify:
    clarify_id: str
    question: str
    options: list[tuple[str, str]]  # (display label, answer value)


def _normalise_options(raw: Any) -> list[tuple[str, str]]:
    """Tolerate old/richer clarify payloads without importing Textual widgets."""
    if raw is None:
        values: list[Any] = []
    elif isinstance(raw, str):
        text = raw.strip()
        values = [raw] if text else []
        if text[:1] in "[({":
            import ast

            try:
                parsed = ast.literal_eval(text)
            except (ValueError, SyntaxError, MemoryError, RecursionError):
                parsed = None
            if isinstance(parsed, (list, tuple, set)):
                values = list(parsed)
            elif isinstance(parsed, dict):
                values = [parsed]
    elif isinstance(raw, (list, tuple)):
        values = list(raw)
    else:
        values = [raw]

    result: list[tuple[str, str]] = []
    for value in values:
        if isinstance(value, dict):
            answer = value.get("value")
            desc = value.get("description")
            if answer is not None:
                label = f"{answer} — {desc}" if desc else str(answer)
                result.append((label, str(answer)))
                continue
            if desc:
                result.append((str(desc), str(desc)))
                continue
        shown = str(value)
        result.append((shown, shown))
    return result


class _SlashCompleter(Completer):
    """Small prompt-toolkit adapter around the renderer-neutral command list."""

    def get_completions(self, document, complete_event):  # pragma: no cover - PTK calls
        from prompt_toolkit.completion import Completion

        word = document.text_before_cursor
        if not word.startswith("/") or any(ch.isspace() for ch in word):
            return
        prefix = word.lower()
        for command in COMMANDS:
            if command.name.startswith(prefix):
                suffix = " " if command.takes_args else ""
                yield Completion(
                    command.name + suffix,
                    start_position=-len(word),
                    display_meta=command.desc,
                )


class InlineApp:
    """RenderSink implementation plus a prompt-toolkit input loop."""

    CTRL_C_EXIT_WINDOW = 2.0
    QUEUE_CONFIRM_WINDOW = 4.0
    _CLARIFY_SAFE = frozenset({"/help", "/quit", "/reconnect", "/status"})
    _SERVER_CONTROLS = frozenset({"/approve", "/deny", "/clarify", "/approvals"})

    def __init__(
        self,
        send_coro: Send | None = None,
        session_key: str = "",
        interrupt_coro: Interrupt | None = None,
        reconnect_coro: Reconnect | None = None,
        save_dir=None,
        *,
        stream=None,
        input_reader: InputReader | None = None,
    ) -> None:
        self._send = send_coro
        self._interrupt = interrupt_coro
        self._reconnect = reconnect_coro
        self._turn_status: TurnStatus | None = None
        self._session_key = session_key
        self._save_dir = (
            Path(save_dir) if save_dir is not None else Path.cwd() / "transcripts"
        )
        self._base_stream = stream if stream is not None else sys.stdout
        self._printer = InlinePrinter(self._base_stream)
        self._input_reader = input_reader
        self._brand = load_brand()
        self._details = parse_env()
        self._turns = TurnRegistry()
        self._connected = True
        self._exit_requested = False
        self._running = False

        self._group: str | None = None
        self._drafts: dict[str, str] = {}
        self._tools: dict[str, dict] = {}
        self._thinking: dict[str, CogEvent] = {}
        self._pending_approval: _Approval | None = None
        self._pending_clarify: _Clarify | None = None
        self._pending_display: deque[bool] = deque()
        self._visible_controls: set[str] = set()
        self._activity_task: asyncio.Task | None = None
        self._activity_label = ""
        self._turn_started = 0.0
        self._last_ctrl_c = 0.0
        self._last_queue_confirm = 0.0
        self._prompt_default = ""
        self._stop_requested = False
        self._last_reply_event_id = ""
        self._cleared_since_reply = False
        self._status: dict[str, Any] = {}

        # Conversation is the human-readable /copy and md/txt export source.
        # Audit additionally retains redacted cognitive and lifecycle evidence.
        self._conversation: list[dict[str, str]] = []
        self._audit: list[dict[str, Any]] = []
        self._audit_tool_index: dict[str, int] = {}
        self._audit_cog_index: dict[str, int] = {}

    @property
    def goodbye_message(self) -> str:
        return self._brand.goodbye

    # ------------------------------------------------------------------ output
    def _begin(self, group: str) -> None:
        if self._group is not None and self._group != group:
            self._printer.blank()
        self._group = group

    def _notice(self, text: str, *, error: bool = False) -> None:
        self._begin("note")
        if error:
            self._printer.error(text)
        else:
            self._printer.notice(text)

    def _banner(self) -> None:
        self._begin("note")
        self._printer.plain(
            A.paint(self._brand.name, A.BOLD, A.fg("primary"))
            + A.paint(f" · {self._brand.tagline}", A.fg("text-muted"))
        )
        if self._session_key:
            self._printer.child(f"会话 {self._session_key}")
        self._printer.cont(self._brand.welcome)
        self._printer.blank()
        self._group = None

    def _start_activity(self, label: str = "正在思考") -> None:
        self._activity_label = label
        was_active = bool(self._turn_started)
        if not self._turn_started:
            self._turn_started = time.monotonic()
        # A pipe cannot repaint a transient status row.  Emit one start marker
        # per turn, then keep later stage refinements in memory only.
        if was_active and not self._printer.is_tty:
            return
        if self._activity_task is not None and not self._activity_task.done():
            return
        self._printer.spinner_start(self._activity_text())
        if not self._printer.is_tty:
            return
        try:
            self._activity_task = asyncio.get_running_loop().create_task(
                self._animate_activity()
            )
        except RuntimeError:
            self._activity_task = None

    def _activity_text(self) -> str:
        elapsed = time.monotonic() - self._turn_started if self._turn_started else 0
        suffix = f" · {elapsed:.1f}s" if elapsed >= 1 else ""
        return f"{self._activity_label}{suffix}"

    async def _animate_activity(self) -> None:
        try:
            while True:
                await asyncio.sleep(0.1)
                self._printer.spinner_update(self._activity_text())
        except asyncio.CancelledError:
            # Normal spinner shutdown; it owns no resources beyond this task.
            pass

    def _stop_activity(self) -> None:
        task, self._activity_task = self._activity_task, None
        if task is not None:
            task.cancel()
        self._printer.spinner_clear()
        self._activity_label = ""
        self._turn_started = 0.0

    # ----------------------------------------------------------- bridge callbacks
    def on_turn_accepted(self, event_id: str) -> None:
        kind = self._turns.on_accepted(event_id)
        visible = self._pending_display.popleft() if self._pending_display else False
        if kind == "control" and visible and event_id:
            self._visible_controls.add(event_id)

    def on_user_reply_token(self, inbound_id: str, text: str) -> None:
        # Append-only terminals cannot safely show optimistic text.  Keep the
        # draft only as state; the authoritative final is rendered exactly once.
        # Only a bounded tail is useful for diagnostics; the final frame is the
        # authority, so retaining an entire long draft would waste memory.
        self._drafts[inbound_id] = (
            self._drafts.get(inbound_id, "") + text
        )[-256:]
        self._start_activity("正在组织答案")

    def on_user_reply_reset(self, inbound_id: str) -> None:
        self._drafts.pop(inbound_id, None)
        self._start_activity("正在继续处理")

    def on_user_reply_final(self, inbound_id: str, text: str) -> None:
        kind = self._turns.on_final(inbound_id)
        self._drafts.pop(inbound_id, None)
        if kind == "control" and inbound_id not in self._visible_controls:
            return
        self._visible_controls.discard(inbound_id)
        body = str(text or "").strip()
        if not body:
            if self._stop_requested:
                self._retire_running_tools()
                self._stop_requested = False
            if kind != "control":
                self._turns.note_turn_settled()
            if not self._turns.has_active_primary:
                self._stop_activity()
            return
        # Re-delivery on reconnect is common; exact event id is authoritative.
        if inbound_id and inbound_id == self._last_reply_event_id:
            return
        if self._stop_requested:
            self._retire_running_tools()
            self._stop_requested = False
        self._begin("model")
        self._printer.reply(body)
        self._conversation.append({"role": "assistant", "text": body})
        self._audit.append({
            "type": "assistant", "event_id": inbound_id, "text": body,
        })
        if inbound_id:
            self._last_reply_event_id = inbound_id
        self._cleared_since_reply = False
        if kind != "control":
            self._turns.note_turn_settled()
        if not self._turns.has_active_primary:
            self._stop_activity()

    def on_cognitive(self, ev: CogEvent) -> None:
        self._record_cognitive(ev)
        cog = ev.cog_type
        data = ev.data or {}

        if cog == "heartbeat":
            stage = str(data.get("stage", "")).strip()
            if stage and self._turns.has_active_primary:
                labels = {
                    "thinking": "正在思考", "tool": "正在调用工具",
                    "responding": "正在组织答案",
                }
                self._start_activity(labels.get(stage, "正在处理"))
            return
        if cog == "cost_update":
            self._status.update({
                key: data.get(key)
                for key in (
                    "total_cost", "model", "context_used", "context_max",
                    "memory_count",
                )
                if data.get(key) is not None
            })
            return
        if cog == "approval_request":
            self._stop_activity()
            self._pending_approval = _Approval(
                str(data.get("request_id", "")), str(data.get("action", "")),
                data.get("params") or {}, str(data.get("risk", "")),
            )
            self._show_approval(self._pending_approval)
            return
        if cog == "approval_closed":
            pending = self._pending_approval
            request_id = str(data.get("request_id", ""))
            if pending is not None and (not request_id or request_id == pending.request_id):
                self._pending_approval = None
                self._notice("审批请求已关闭。")
                if self._turns.has_active_primary:
                    self._start_activity("正在继续处理")
            return
        if cog == "clarify_request":
            self._stop_activity()
            self._pending_clarify = _Clarify(
                str(data.get("clarify_id", "")), str(data.get("question", "")),
                _normalise_options(data.get("options")),
            )
            self._show_clarify(self._pending_clarify)
            return
        if cog == "clarify_closed":
            pending = self._pending_clarify
            clarify_id = str(data.get("clarify_id", ""))
            if pending is not None and (not clarify_id or clarify_id == pending.clarify_id):
                self._pending_clarify = None
                if self._turns.has_active_primary:
                    self._start_activity("正在继续处理")
            return
        if cog == "tool_call":
            self._handle_tool(ev)
            return
        if cog == "thinking":
            self._handle_thinking(ev)
            return

        if not self._details.shows(cog):
            return
        self._begin("trail")
        glyph = "✶" if cog == "evolution" else "⏺"
        self._printer.head(mask_sensitive_strings(ev.summary), glyph=glyph)
        if self._details.starts_expanded(cog):
            for item in data.get("items", []) or []:
                if isinstance(item, dict):
                    self._printer.child(mask_sensitive_strings(str(item.get("content", ""))))

    def _handle_tool(self, ev: CogEvent) -> None:
        data = dict(ev.data or {})
        tcid = str(data.get("tool_call_id") or ev.cog_event_id)
        previous = self._tools.get(tcid, {})
        merged = dict(previous)
        merged.update(data)
        if not data.get("params") and previous.get("params"):
            merged["params"] = previous["params"]
        status = str(merged.get("status", "running"))
        name = str(merged.get("name", "tool"))
        if status == "running":
            self._tools[tcid] = merged
            count = len(self._tools)
            obj = pick_object(name, merged.get("params") or {})
            label = f"{humanize_tool(name)} {obj}".rstrip()
            if count > 1:
                label += f" · {count} 个工具"
            self._start_activity(label)
            return

        self._tools.pop(tcid, None)
        failed = status not in ("running", "ok")
        if self._details.shows("tool_call", failed=failed, tool_name=name):
            self._begin("trail")
            self._printer.tool_line(
                name, merged.get("params") or {}, status,
                merged.get("result_meta"), str(merged.get("result_text", "")),
                merged.get("duration_ms"),
            )
            if self._details.starts_expanded("tool_call"):
                for entry in format_params(merged.get("params") or {}, value_width=120):
                    self._printer.cont(entry)
                result = clip(mask_sensitive_strings(str(merged.get("result_text", ""))), 2000)
                for line in result.splitlines():
                    self._printer.cont(line)
        if self._tools:
            self._start_activity(f"正在调用工具 · {len(self._tools)} 个进行中")
        elif self._turns.has_active_primary:
            self._start_activity("正在继续处理")

    def _retire_running_tools(self) -> None:
        """Settle transient-only tool state when no done frame can follow."""
        for tool in list(self._tools.values()):
            self._begin("trail")
            self._printer.tool_line(
                str(tool.get("name", "tool")), tool.get("params") or {},
                "interrupted",
            )
        self._tools.clear()

    def _handle_thinking(self, ev: CogEvent) -> None:
        data = ev.data or {}
        tid = str(data.get("thinking_id") or ev.cog_event_id)
        if data.get("retracted"):
            self._thinking.pop(tid, None)
            return
        if data.get("streaming"):
            self._thinking[tid] = ev
            self._start_activity("正在思考")
            return
        self._thinking.pop(tid, None)
        if not self._details.shows("thinking"):
            return
        summary = mask_sensitive_strings(ev.summary or "已完成思考")
        self._begin("trail")
        self._printer.head(summary, glyph="✻")
        if self._details.starts_expanded("thinking") and data.get("text"):
            for line in str(data["text"]).strip().splitlines():
                self._printer.cont(mask_sensitive_strings(line))

    def _show_approval(self, pending: _Approval) -> None:
        self._begin("ui")
        action = mask_sensitive_strings(pending.action or "执行操作")
        self._printer.head(f"需要确认：{action}", glyph="✦", style=A.fg("warning"))
        if pending.risk:
            self._printer.child(f"风险等级：{pending.risk}")
        for entry in format_params(pending.params, value_width=100):
            self._printer.cont(entry)
        self._printer.cont("输入 y 批准 · n 拒绝 · a 本会话始终允许")

    def _show_clarify(self, pending: _Clarify) -> None:
        self._begin("ui")
        self._printer.head(pending.question or "需要补充信息", glyph="?")
        for index, (label, _answer) in enumerate(pending.options, 1):
            self._printer.child(f"{index}. {mask_sensitive_strings(label)}")
        hint = "输入序号或直接输入答案" if pending.options else "请直接输入答案"
        self._printer.cont(hint)

    def on_error(self, msg: str) -> None:
        self._stop_activity()
        self._retire_running_tools()
        self._notice(msg or "未知错误", error=True)
        self._turns.on_terminal_error()
        self._pending_display.clear()
        self._drafts.clear()
        self._thinking.clear()
        self._pending_approval = None
        self._pending_clarify = None
        self._stop_requested = False

    def notify_disconnected(self) -> None:
        if not self._connected:
            return
        self._connected = False
        self._stop_activity()
        self._retire_running_tools()
        self._pending_approval = None
        self._pending_clarify = None
        self._stop_requested = False
        self._notice("连接已断开。输入 /reconnect 重连，Ctrl+D 退出。", error=True)

    def notify_reconnected(self) -> None:
        self._connected = True
        self._turns.reset_on_reconnect()
        self._pending_display.clear()
        self._visible_controls.clear()
        self._drafts.clear()
        self._tools.clear()
        self._thinking.clear()
        self._pending_approval = None
        self._pending_clarify = None
        self._stop_requested = False
        self._notice("已重新连接。")

    def replay_missed_reply(self, text: str, event_id: str = "") -> None:
        body = str(text or "").strip()
        if not body:
            return
        if event_id and event_id == self._last_reply_event_id:
            return
        last = next(
            (item["text"] for item in reversed(self._conversation)
             if item["role"] == "assistant"),
            "",
        )
        if not self._cleared_since_reply and last.strip() == body:
            return
        self._notice("补显示断连期间完成的回复：")
        self.on_user_reply_final(event_id, body)

    # -------------------------------------------------------------------- input
    async def run_async(self) -> None:
        self._running = True
        self._banner()
        try:
            if self._input_reader is not None:
                await self._run_injected_input()
            elif not self._printer.is_tty or not self._stdin_is_tty():
                await self._run_plain_input()
            else:
                await self._run_prompt_toolkit()
        finally:
            self._running = False
            self._stop_activity()

    @staticmethod
    def _stdin_is_tty() -> bool:
        try:
            return bool(sys.stdin.isatty())
        except (AttributeError, ValueError):
            return False

    async def _run_plain_input(self) -> None:
        """Control-sequence-free fallback for pipes, files, and dumb runners."""
        while not self._exit_requested:
            try:
                line = await asyncio.to_thread(sys.stdin.readline)
            except (OSError, ValueError):
                break
            if line == "":
                break
            await self.submit(line, echo=True)

    async def _run_injected_input(self) -> None:
        while not self._exit_requested:
            try:
                value = self._input_reader()
                text = await value if inspect.isawaitable(value) else value
            except (EOFError, StopAsyncIteration):
                break
            if text is None:
                break
            await self.submit(str(text), echo=True)

    async def _run_prompt_toolkit(self) -> None:
        from prompt_toolkit import PromptSession
        from prompt_toolkit.formatted_text import HTML
        from prompt_toolkit.history import InMemoryHistory
        from prompt_toolkit.key_binding import KeyBindings
        from prompt_toolkit.patch_stdout import patch_stdout
        from prompt_toolkit.styles import Style

        keys = KeyBindings()

        @keys.add("escape", "enter")
        def _newline(event) -> None:
            event.current_buffer.insert_text("\n")

        @keys.add("c-c")
        def _ctrl_c(event) -> None:
            # Shell/readline convention: clear a draft first.  An approval is
            # higher priority because Ctrl+C must actively deny it to unblock
            # the server, even if stray text is present in the buffer.
            if event.current_buffer.text and self._pending_approval is None:
                event.current_buffer.reset()
                self._last_ctrl_c = 0.0
                return
            event.app.exit(exception=KeyboardInterrupt)

        session = PromptSession(
            history=InMemoryHistory(), completer=_SlashCompleter(),
            complete_while_typing=True, multiline=False, key_bindings=keys,
        )
        style = Style.from_dict({
            "prompt": "bold #4fd1c5", "toolbar": "bg:#161b22 #8b949e",
        })
        with patch_stdout(raw=True):
            # Gateway callbacks now write through prompt-toolkit's proxy, which
            # redraws the current edit buffer after each asynchronous line.
            self._printer.set_stream(sys.stdout)
            while not self._exit_requested:
                try:
                    default, self._prompt_default = self._prompt_default, ""
                    text = await session.prompt_async(
                        HTML(f"<prompt>{self._brand.prompt} </prompt>"),
                        bottom_toolbar=self._toolbar,
                        style=style,
                        default=default,
                    )
                except KeyboardInterrupt:
                    await self.handle_ctrl_c()
                    continue
                except EOFError:
                    break
                self._printer.note_external_line()
                await self.submit(text, echo=False)
        self._printer.set_stream(self._base_stream)

    def _toolbar(self) -> str:
        if not self._connected:
            return " ○ 已断开 · /reconnect"
        if self._pending_approval:
            return " ✦ 等待确认 · y/n/a"
        if self._pending_clarify:
            return " ? 等待补充信息"
        if self._turns.has_active_primary:
            return " ● 正在处理 · Ctrl+C 停止"
        model = str(self._status.get("model", ""))
        return f" ● 已连接{f' · {model}' if model else ''} · /help"

    async def submit(self, text: str, *, echo: bool = True) -> None:
        text = str(text or "").strip()
        if not text:
            return
        if echo:
            self._begin("user")
            self._printer.head(text, glyph=self._brand.prompt, style=A.fg("primary"))
        else:
            self._group = "user"

        name, _, raw_arg = text.partition(" ")
        lowered = name.lower()
        arg = raw_arg.strip()

        # Escape hatches stay available even when a question owns the input.
        if (
            (self._pending_clarify or self._pending_approval)
            and lowered in self._CLARIFY_SAFE
        ):
            if await self._run_local(lowered, arg):
                return
        if self._pending_approval:
            decision = text.lower()
            if decision in {"y", "yes", "是", "批准"}:
                await self._decide_approval("approve")
            elif decision in {"a", "always", "始终允许"}:
                await self._decide_approval("approve", "session")
            elif decision in {"n", "no", "否", "拒绝"}:
                await self._decide_approval("deny")
            else:
                self._notice("当前操作等待确认，请输入 y、n 或 a。")
            return
        if self._pending_clarify:
            await self._answer_clarify(text)
            return
        if await self._run_local(lowered, arg):
            return
        if not self._connected:
            self._notice("未连接。请先输入 /reconnect 重连。", error=True)
            return

        if lowered in self._SERVER_CONTROLS:
            await self._send_message(
                text, "control", display_control=lowered == "/approvals",
            )
            return
        if not text.startswith("/") and self._turns.has_active_primary:
            now = time.monotonic()
            if now - self._last_queue_confirm >= self.QUEUE_CONFIRM_WINDOW:
                self._last_queue_confirm = now
                self._prompt_default = text
                kept = self._input_reader is None and self._printer.is_tty
                self._notice(
                    "上一轮仍在进行；再次提交会将此消息排队。"
                    + ("已将原文保留在输入框，" if kept else "重新输入后")
                    + "再次提交以确认，或按 Ctrl+C 停止当前任务。"
                )
                return
        self._last_queue_confirm = 0.0
        self._conversation.append({"role": "user", "text": text})
        self._audit.append({"type": "user", "text": text})
        self._start_activity("正在思考")
        await self._send_message(text, "primary")

    async def _send_message(
        self, text: str, kind: str, *, display_control: bool = False,
    ) -> bool:
        if not self._connected:
            return False
        self._turns.note_send(kind)
        self._pending_display.append(display_control)
        if self._send is None:
            return True
        try:
            await self._send(text)
        except Exception:
            self.notify_disconnected()
            return False
        return self._connected

    async def _decide_approval(self, decision: str, level: str = "") -> None:
        pending = self._pending_approval
        if pending is None:
            return
        command = (
            approve_command(pending.request_id, level)
            if decision == "approve"
            else deny_command(pending.request_id)
        )
        if not await self._send_message(command, "control"):
            return
        self._pending_approval = None
        self._begin("ui")
        mark = "已批准" if decision == "approve" else "已拒绝"
        self._printer.child(mark, dim=False)
        self._start_activity("正在继续处理")

    async def _answer_clarify(self, raw: str) -> None:
        pending = self._pending_clarify
        if pending is None or not raw.strip():
            return
        answer = raw.strip()
        if answer.isdigit() and pending.options:
            index = int(answer) - 1
            if 0 <= index < len(pending.options):
                answer = pending.options[index][1]
            else:
                self._notice(f"请输入 1–{len(pending.options)} 中的序号，或直接输入文本答案。")
                return
        if not await self._send_message(
            clarify_command(pending.clarify_id, answer), "control",
        ):
            return
        self._pending_clarify = None
        self._begin("ui")
        self._printer.child(f"已回答：{mask_sensitive_strings(answer)}", dim=False)
        self._start_activity("正在继续处理")

    async def handle_ctrl_c(self) -> None:
        if self._pending_approval is not None:
            await self._decide_approval("deny")
            self._last_ctrl_c = 0.0
            return
        active = self._turns.has_active_primary or self._turns.may_be_running_uncorrelated
        if active and self._interrupt is not None:
            self._last_ctrl_c = 0.0
            try:
                await self._interrupt(self._turns.active_turn_id)
            except Exception:
                self.notify_disconnected()
                return
            self._pending_clarify = None
            self._stop_requested = True
            self._activity_label = "正在停止"
            self._notice("已请求停止当前任务…")
            return
        now = time.monotonic()
        if now - self._last_ctrl_c < self.CTRL_C_EXIT_WINDOW:
            self._exit_requested = True
            return
        self._last_ctrl_c = now
        self._notice("再次按 Ctrl+C 退出（Ctrl+D 直接退出）。")

    # -------------------------------------------------------------- local cmds
    async def _run_local(self, name: str, arg: str) -> bool:
        if name == "/help":
            self._show_help()
        elif name == "/clear":
            self._printer.clear_screen()
            self._group = None
            self._drafts.clear()
            self._cleared_since_reply = True
            if self._pending_approval:
                self._show_approval(self._pending_approval)
            elif self._pending_clarify:
                self._show_clarify(self._pending_clarify)
        elif name == "/quit":
            self._exit_requested = True
        elif name == "/copy" and arg in ("", "all"):
            self._do_copy(whole=arg == "all")
        elif name == "/save":
            self._do_save(arg)
        elif name == "/theme":
            self._do_theme(arg)
        elif name == "/details":
            self._do_details(arg)
        elif name == "/reconnect":
            await self._do_reconnect()
        elif name == "/status":
            await self._do_status(arg)
        else:
            return False
        return True

    def _show_help(self) -> None:
        self._begin("note")
        self._printer.head("可用命令", glyph="/")
        for title, scope in (("本地", "local"), ("服务端", "server")):
            self._printer.child(title, dim=False)
            for command in (c for c in COMMANDS if c.scope == scope):
                args = f" {command.arg_template}" if command.arg_template else ""
                self._printer.cont(f"{command.name}{args}  {command.desc}")

    async def _do_reconnect(self) -> None:
        if self._connected:
            self._notice("当前已连接，无需重连。")
            return
        if self._reconnect is None:
            self._notice("此环境不支持重连，请重新启动 echo-agent cli。", error=True)
            return
        self._notice("正在重连…")
        try:
            ok = await self._reconnect()
        except Exception:
            ok = False
        if ok:
            self.notify_reconnected()
            await self._do_status("", quiet=True)
        else:
            self._notice("重连失败。请确认网关仍在运行后重试。", error=True)

    async def _do_status(self, event_id: str = "", *, quiet: bool = False) -> None:
        if self._turn_status is None:
            if not quiet:
                self._notice("当前连接不支持回合状态查询。", error=True)
            return
        try:
            turn = await self._turn_status(event_id)
        except Exception:
            turn = {}
        if not turn:
            if not quiet:
                self._notice("未找到可查询的回合状态。", error=True)
            return
        labels = {
            "accepted": "已接收", "running": "正在执行",
            "waiting_approval": "等待审批",
            "waiting_clarification": "等待补充信息",
            "completed": "已完成", "incomplete": "未完成",
            "failed": "失败", "interrupted": "已中断",
        }
        status = str(turn.get("status", "unknown"))
        detail = labels.get(status, status)
        if turn.get("current_tool"):
            detail += f" · 当前工具 {turn['current_tool']}"
        self._notice(f"回合 {turn.get('event_id') or '-'}：{detail}")
        self._audit.append({
            "type": "turn_status", **redact_for_export(turn),
        })

    def _do_details(self, arg: str) -> None:
        if arg:
            parsed = parse_details(arg)
            if parsed is None:
                self._notice("用法: /details <思考|工具|状态> <展开|折叠|精简|隐藏>", error=True)
                return
            self._details = self._details.with_section(*parsed)
        self._begin("note")
        self._printer.head("过程信息显示", glyph="·")
        for label, state in self._details.describe():
            self._printer.child(f"{label}：{state}")

    def _do_theme(self, arg: str) -> None:
        value = arg.strip().lower()
        if value not in ("", "light", "dark"):
            self._notice("用法: /theme [light|dark]", error=True)
            return
        if value:
            os.environ["ECHO_TUI_THEME"] = value
            A.reset_palette_cache()
        current = os.environ.get("ECHO_TUI_THEME", "auto")
        self._notice(f"当前主题：{current}（之后的输出生效）")

    def _copy_text(self, text: str) -> bool:
        candidates: list[list[str]] = []
        if sys.platform == "darwin":
            candidates.append(["pbcopy"])
        elif os.name == "nt":
            candidates.append(["clip"])
        else:
            candidates.extend([["wl-copy"], ["xclip", "-selection", "clipboard"]])
        for command in candidates:
            if shutil.which(command[0]) is None:
                continue
            try:
                run_owned(
                    command, input=text, text=True, check=True,
                    stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                )
                return True
            except (OSError, subprocess.SubprocessError):
                continue
        # OSC 52 is a useful fallback for remote terminals.  Cap payload size so
        # a huge transcript cannot freeze the terminal parser.
        if self._printer.is_tty and len(text.encode("utf-8")) <= 100_000:
            encoded = base64.b64encode(text.encode("utf-8")).decode("ascii")
            try:
                self._base_stream.write(f"\033]52;c;{encoded}\a")
                self._base_stream.flush()
                return True
            except (OSError, ValueError, AttributeError):
                # Clipboard fallback is best-effort; /copy reports failure below.
                pass
        return False

    def _do_copy(self, *, whole: bool) -> None:
        text = self._export_text() if whole else next(
            (item["text"] for item in reversed(self._conversation)
             if item["role"] == "assistant"),
            "",
        )
        if not text:
            self._notice("暂无可复制的内容。")
        elif self._copy_text(text):
            scope = "整段对话" if whole else "最近回复"
            self._notice(f"已复制{scope}（{len(text)} 字）。")
        else:
            self._notice("未找到可用的系统剪贴板。可使用 /save 保存。", error=True)

    def _export_text(self) -> str:
        labels = {"user": "你", "assistant": self._brand.name}
        return "\n\n".join(
            f"{labels[item['role']]}：{item['text']}" for item in self._conversation
        )

    def _do_save(self, arg: str) -> None:
        fmt = "md"
        try:
            tokens = shlex.split(arg)
        except ValueError as exc:
            self._notice(f"路径解析失败：{exc}", error=True)
            return
        path_parts: list[str] = []
        index = 0
        while index < len(tokens):
            token = tokens[index]
            if token == "--format":
                if index + 1 >= len(tokens):
                    self._notice("--format 需要 md、txt 或 json。", error=True)
                    return
                fmt = tokens[index + 1].lower()
                index += 2
            elif token.startswith("--format="):
                fmt = token.partition("=")[2].lower()
                index += 1
            else:
                path_parts.append(token)
                index += 1
        if fmt not in {"md", "txt", "json"}:
            self._notice(f"不支持的导出格式：{fmt}", error=True)
            return
        if not self._conversation:
            self._notice("暂无可保存的对话。")
            return

        when = datetime.now().strftime("%Y-%m-%d %H:%M:%S")
        if fmt == "json":
            content = json.dumps({
                "session_key": self._session_key, "exported_at": when,
                "events": self._audit,
            }, ensure_ascii=False, indent=2) + "\n"
        elif fmt == "txt":
            content = self._export_text().rstrip() + "\n"
        else:
            rows = [
                f"# {self._brand.name} 对话", "",
                f"- 会话：`{self._session_key}`", f"- 导出时间：{when}", "",
            ]
            for item in self._conversation:
                who = "你" if item["role"] == "user" else self._brand.name
                rows.extend((f"## {who}", "", item["text"], ""))
            content = "\n".join(rows).rstrip() + "\n"

        suffix = {"md": ".md", "txt": ".txt", "json": ".json"}[fmt]
        raw_arg = " ".join(path_parts)
        auto = f"echo-{datetime.now().strftime('%Y%m%d-%H%M%S')}{suffix}"
        if not raw_arg:
            target = self._save_dir / auto
        else:
            raw = Path(raw_arg).expanduser()
            if raw_arg.endswith(("/", os.sep)) or raw.is_dir():
                target = (raw if raw.is_absolute() else self._save_dir / raw) / auto
            else:
                if raw.suffix.lower() != suffix:
                    raw = raw.with_name(raw.name + suffix)
                target = raw if raw.is_absolute() else self._save_dir / raw
        try:
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_text(content, encoding="utf-8")
        except OSError as exc:
            self._notice(f"保存失败：{exc}", error=True)
            return
        self._notice(f"已保存到 {target}（{len(content)} 字）。")

    # -------------------------------------------------------------------- audit
    def _record_cognitive(self, ev: CogEvent) -> None:
        record = {
            "type": "cognitive", "cog_type": ev.cog_type,
            "cog_event_id": ev.cog_event_id,
            "inbound_event_id": ev.inbound_event_id,
            "summary": redact_for_export(ev.summary),
            "data": redact_for_export(ev.data),
        }
        if ev.cog_type == "tool_call":
            tcid = str(ev.data.get("tool_call_id") or ev.cog_event_id)
            index = self._audit_tool_index.get(tcid)
            if index is None:
                self._audit_tool_index[tcid] = len(self._audit)
                self._audit.append(record)
            else:
                old = dict(self._audit[index].get("data") or {})
                new = dict(record.get("data") or {})
                if not new.get("params") and old.get("params"):
                    new["params"] = old["params"]
                old.update(new)
                record["data"] = old
                self._audit[index].update(record)
            return
        key = ev.cog_event_id
        index = self._audit_cog_index.get(key) if key else None
        if index is None:
            if key:
                self._audit_cog_index[key] = len(self._audit)
            self._audit.append(record)
        else:
            self._audit[index].update(record)
