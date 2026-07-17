"""Transcript block widgets. Rendering logic is kept in plain methods
(render_summary/render_detail) so it's unit-testable without a live screen."""

from __future__ import annotations

import os

from rich.markdown import Markdown
from rich.markup import escape
from rich.table import Table
from rich.text import Text
from textual.widgets import Static

from echo_agent.cli.tui.protocol import CogEvent

_ICON = {
    "memory_recalled": "🧠", "memory_written": "✍", "thinking": "💭",
    "tool_call": "🔧", "approval_request": "⚠️", "cost_update": "💰",
    "heartbeat": "⏳", "evolution": "🧬",
}

_TOOL_VERB = {
    "read_file": "读取", "write_file": "写入", "edit_file": "编辑",
    "patch": "打补丁", "list_dir": "列出", "search_files": "搜索",
    "session_search": "检索会话", "knowledge_search": "查知识库",
    "exec": "执行", "process": "运行进程", "web_fetch": "抓取网页",
    "web_search": "联网搜索", "memory": "记忆", "todo": "更新待办",
}

# 每个工具用哪个参数当"操作对象"。缺省走兜底：第一个字符串参数。
_OBJECT_KEY = {
    "read_file": "path", "write_file": "path", "edit_file": "path",
    "patch": "path", "list_dir": "path", "exec": "command",
    "process": "command", "web_fetch": "url", "web_search": "query",
}


def humanize_tool(name: str) -> str:
    """Tool id -> Chinese verb. Unknown tools fall back to the raw id."""
    return _TOOL_VERB.get(name, name)


def _clip(s: str, n: int) -> str:
    s = " ".join(str(s).split())
    return s if len(s) <= n else s[: n - 1] + "…"


def pick_object(name: str, params: dict) -> str:
    """The primary argument shown as the tool's operand."""
    params = params or {}
    if name == "search_files":
        pat = params.get("pattern")
        return f'"{pat}"' if pat else ""
    key = _OBJECT_KEY.get(name)
    val = params.get(key) if key else None
    if val is None:
        # Fallback: first string-valued argument.
        val = next((v for v in params.values() if isinstance(v, str)), "")
    if name in ("read_file", "write_file", "edit_file", "patch") and val:
        val = os.path.basename(str(val))
    return _clip(val, 48) if val else ""


def summarize_result(
    name: str, result_meta: dict | None, result_text: str, success: bool
) -> str:
    """Turn the producer-supplied count (result_meta) into Chinese words;
    fall back to a text preview. Never recount on the truncated result_text."""
    if not success:
        return "失败"
    meta = result_meta or {}
    if name == "read_file" and "total_lines" in meta:
        return f"{meta['total_lines']} 行"
    if name == "search_files" and "count" in meta:
        return f"找到 {meta['count']} 处"
    if name == "list_dir" and "count" in meta:
        return f"{meta['count']} 个"
    if name in ("exec", "process"):
        return "完成"
    preview = _clip(result_text or "", 40)
    return preview or "完成"


class Banner(Static):
    """Modern-minimal brand banner shown on the transcript's first screen.

    Kept as a pure render (build_text) so it is unit-testable without a live
    screen, mirroring the other blocks."""

    def __init__(self, session_key: str = "") -> None:
        self.session_key = session_key
        super().__init__(self.build_text())

    def build_text(self) -> str:
        sess = f"  ·  会话 {self.session_key}" if self.session_key else ""
        return (
            f"[bold $primary]echo[/] [$text-muted]· agent[/]{sess}\n"
            f"[$text-muted]输入消息开始对话  ·  /help 查看命令  ·  Ctrl+C 停止任务/退出[/]"
        )


class UserTurn(Static):
    def __init__(self, text: str) -> None:
        # Keep the sigil-free original so /copy can export a clean transcript
        # without the "❯ " decoration.
        self.raw_text = text
        self.text_content = f"❯ {text}"
        # Markup keeps the sigil in the accent colour and the task text bright,
        # so the title reads as the strongest element in each turn.
        super().__init__(f"[bold $primary]❯[/] [b]{escape(text)}[/b]")


class AgentReply(Static):
    """Agent reply body. Streaming tokens render as plain (escaped) text —
    partial markdown is inevitably broken, and re-parsing every token would
    flicker. The finished reply is rendered as markdown via ``set_markdown``;
    ``set_final`` stays a plain-text path for status lines (heartbeat/error)
    that reuse this widget but carry hand-built Rich markup, not markdown."""

    def __init__(self) -> None:
        self._buf = ""
        # Status lines (server errors, etc.) reuse this widget but are NOT real
        # agent replies. Flagged so /copy skips them and stays pointed at the
        # last genuine answer.
        self.is_status = False
        super().__init__("[$primary]●[/] ")

    @property
    def text(self) -> str:
        """The reply body as plain text (markdown source / status line),
        without the ``●`` sigil — used by /copy."""
        return self._buf

    def append_token(self, t: str) -> None:
        self._buf += t
        self.update(f"[$primary]●[/] {escape(self._buf)}")

    def set_final(self, text: str) -> None:
        self._buf = text
        self.update(f"[$primary]●[/] {escape(self._buf)}")

    def _bullet_color(self) -> str:
        """Resolve the theme's ``primary`` colour so the ``●`` matches the
        streaming sigil. Rich renderables bypass Textual's ``$var`` markup
        substitution, so we look the colour up here. Falls back to a fixed hue
        when no app/theme is attached (e.g. pure unit tests)."""
        try:
            theme = self.app.current_theme
            if theme is not None and theme.primary:
                return theme.primary
        except Exception:
            pass
        return "#8899ff"

    def set_markdown(self, text: str) -> None:
        """Render the finished reply as markdown, keeping the ``●`` sigil inline
        with the body's first line. A two-column grid places the accent bullet
        beside the markdown so the turn still reads as "the agent is speaking",
        and wrapped/subsequent lines stay aligned under the body."""
        self._buf = text
        grid = Table.grid(padding=(0, 1, 0, 0))
        grid.add_column()
        grid.add_column()
        grid.add_row(Text("●", style=self._bullet_color()), Markdown(text))
        self.update(grid)


class CognitiveBlock(Static):
    def __init__(self, ev: CogEvent) -> None:
        self.ev = ev
        self.expanded = False
        super().__init__(self.render_summary())

    def render_summary(self) -> str:
        icon = _ICON.get(self.ev.cog_type, "•")
        hint = " (ctrl+r)" if self.ev.cog_type == "memory_recalled" else (
            " (ctrl+o)" if self.ev.cog_type == "thinking" else "")
        # Cognitive traces are secondary information — render the whole line in
        # the muted indigo tone so it recedes behind replies and tool actions.
        return f"[$secondary]{icon}[/] [$text-muted]{escape(self.ev.summary)}{escape(hint)}[/]"

    def render_detail(self) -> str:
        d = self.ev.data
        lines = [self.render_summary()]
        for it in d.get("items", []):
            src = it.get("source", "")
            badge = f"\\[{escape(src)}]" if src else ""
            content = escape(str(it.get("content", "")))
            lines.append(f"    [$text-muted]·[/] {content} [$text-muted]{badge}[/]".rstrip())
        if self.ev.cog_type == "thinking" and d.get("text"):
            lines.append(f"    [$text-muted]{escape(str(d['text']))}[/]")
        return "\n".join(lines)

    def toggle(self) -> None:
        self.expanded = not self.expanded
        self.update(self.render_detail() if self.expanded else self.render_summary())


class ToolCallBlock(Static):
    """One tool invocation. Flips in place from running (🔧 … ) to done
    (🔧 … · summary ✓/✗). Paired across the two frames by tool_call_id."""

    def __init__(
        self,
        tool_call_id: str,
        name: str,
        params: dict,
        status: str = "running",
        result_meta: dict | None = None,
        result_text: str = "",
        duration_ms: int | None = None,
    ) -> None:
        self.tool_call_id = tool_call_id
        # Stored as tool_name to avoid clashing with Textual Widget's read-only
        # `name` property (which has no setter).
        self.tool_name = name
        self.params = params or {}
        self.status = status
        self.result_meta = result_meta
        self.result_text = result_text
        self.duration_ms = duration_ms
        self.expanded = False
        super().__init__(self.render_summary())

    def render_summary(self) -> str:
        verb = escape(humanize_tool(self.tool_name))
        obj = escape(pick_object(self.tool_name, self.params))
        # verb in accent, operand muted so the eye separates "what" from "on what".
        head = f"🔧 [b]{verb}[/b]"
        if obj:
            head += f" [$text-muted]{obj}[/]"
        if self.status == "running":
            return f"{head} [$text-muted]…[/]"
        ok = self.status == "ok"
        mark = "[$success]✓[/]" if ok else "[$error]✗[/]"
        summary = escape(summarize_result(
            self.tool_name, self.result_meta, self.result_text, ok
        ))
        tone = "$text-muted" if ok else "$error"
        return f"{head} [$text-muted]·[/] [{tone}]{summary}[/] {mark}"

    def render_detail(self) -> str:
        lines = [self.render_summary()]
        if self.params:
            joined = ", ".join(f"{k}={_clip(v, 60)}" for k, v in self.params.items())
            lines.append(f"    [$text-muted]↳ 参数 {escape(joined)}[/]")
        if self.result_text:
            lines.append(f"    [$text-muted]↳ 结果 {escape(_clip(self.result_text, 200))}[/]")
        return "\n".join(lines)

    def mark_done(
        self, status: str, result_meta: dict | None, result_text: str, duration_ms: int | None
    ) -> None:
        self.status = status
        self.result_meta = result_meta
        self.result_text = result_text
        self.duration_ms = duration_ms
        self.update(self.render_detail() if self.expanded else self.render_summary())

    def toggle(self) -> None:
        self.expanded = not self.expanded
        self.update(self.render_detail() if self.expanded else self.render_summary())


class ApprovalBlock(Static):
    def __init__(self, request_id: str, action: str, params: dict, risk: str) -> None:
        self.request_id = request_id
        self.action = action
        self.params = params
        self.risk = risk
        self.decision: str | None = None
        super().__init__(self._body())

    def _body(self) -> str:
        action = escape(str(self.action))
        if self.decision == "approve":
            return f"[$warning]⚠️[/] {action} — [$success]✅ 已批准[/]"
        if self.decision == "deny":
            return f"[$warning]⚠️[/] {action} — [$error]❌ 已拒绝[/]"
        return (
            f"[$warning]⚠️ 需要确认:[/] [b]{action}[/b]\n"
            f"    [$text-muted]{escape(str(self.risk))}[/]\n"
            f"    [$text-muted]params={escape(str(self.params))}[/]\n"
            f"    [$success]\\[y] 批准[/]  [$error]\\[n] 拒绝[/]  [$warning]\\[a] 本会话始终允许[/]"
        )

    def mark(self, decision: str) -> None:
        self.decision = decision
        self.update(self._body())


def _option_to_pair(opt) -> tuple[str, str]:
    """Normalize a single clarify option to a (display, answer) pair.

    Plain strings pass through as both. Dict-shaped options (which the model may
    emit despite the string-only schema) show "value — description" but answer
    with the bare value: the description is a hint for the human, not part of
    the choice, so sending the whole rendered label back would feed the model
    prose it never offered as an option. Anything else falls back to str() for
    both."""
    if isinstance(opt, str):
        return opt, opt
    if isinstance(opt, dict):
        value = opt.get("value")
        desc = opt.get("description")
        if value is not None and desc:
            return f"{value} — {desc}", str(value)
        if value is not None:
            return str(value), str(value)
        s = _option_to_str(opt)
        return s, s
    s = _option_to_str(opt)
    return s, s


def _option_to_str(opt) -> str:
    """Display-only string for an option (used where the answer value is not
    needed, e.g. legacy call sites and the dict fallback in _option_to_pair)."""
    if isinstance(opt, str):
        return opt
    if isinstance(opt, dict):
        value = opt.get("value")
        desc = opt.get("description")
        if value is not None and desc:
            return f"{value} — {desc}"
        if value is not None:
            return str(value)
        if desc:
            return str(desc)
    return str(opt)


class ChoiceBlock(Static):
    """A clarify prompt: a question plus optional numbered choices. The user
    picks by number, arrows+enter, or free text. Rendering is a pure method so
    it is unit-testable without a live screen (like ApprovalBlock)."""

    def __init__(self, clarify_id: str, question: str, options: list) -> None:
        self.clarify_id = clarify_id
        self.question = question
        # The clarify schema declares options as strings, but the model
        # sometimes returns richer objects like {"value": ..., "description":
        # ...}. Coerce every option to a display string at this boundary so the
        # rest of the flow (rendering, selection, the answer sent back to the
        # server) only ever deals with strings and never chokes on a dict.
        pairs = [_option_to_pair(o) for o in (options or [])]
        # Display labels (may be "value — description"); shown in the list.
        self.options = [d for d, _ in pairs]
        # Answer values (bare value for dicts); sent back to the server on pick.
        self._answers = [a for _, a in pairs]
        self.highlighted = 0
        self.answer: str | None = None
        super().__init__(self.render_body())

    def render_body(self) -> str:
        q = escape(str(self.question))
        if self.answer is not None:
            return f"[$secondary]❓[/] {q} [$text-muted]—[/] [$success]已选:{escape(str(self.answer))}[/]"
        if not self.options:
            return f"[$secondary]❓[/] {q}\n    [$text-muted](请输入回答)[/]"
        lines = [f"[$secondary]❓[/] [b]{q}[/b]"]
        for i, opt in enumerate(self.options):
            # Keep "{n}. {opt}" contiguous (no tag between number and text) so
            # the label reads as one unit; only the marker/tone differs by state.
            label = f"{i + 1}. {escape(str(opt))}"
            if i == self.highlighted:
                lines.append(f"  [$primary]› {label}[/]")
            else:
                lines.append(f"    [$text-muted]{label}[/]")
        lines.append("    [$text-muted](按数字选择 · ↑↓ 移动后回车 · 或直接输入其他答案)[/]")
        return "\n".join(lines)

    def move(self, delta: int) -> None:
        if not self.options:
            return
        self.highlighted = max(0, min(len(self.options) - 1, self.highlighted + delta))
        self.update(self.render_body())

    def option_for_number(self, n: int) -> str | None:
        # Return the ANSWER value, not the display label: for a dict option the
        # user sees "value — description" but the server must receive only value.
        if not self.options or n < 1 or n > len(self.options):
            return None
        return self._answers[n - 1]

    def highlighted_option(self) -> str | None:
        if not self.options:
            return None
        return self._answers[self.highlighted]

    def mark(self, answer: str) -> None:
        self.answer = answer
        self.update(self.render_body())
