"""Transcript block widgets. Rendering logic is kept in plain methods
(render_summary/render_detail) so it's unit-testable without a live screen."""

from __future__ import annotations

import os

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


class UserTurn(Static):
    def __init__(self, text: str) -> None:
        self.text_content = f"❯ {text}"
        super().__init__(self.text_content)


class AgentReply(Static):
    def __init__(self) -> None:
        self._buf = ""
        super().__init__("● ")

    def append_token(self, t: str) -> None:
        self._buf += t
        self.update(f"● {self._buf}")

    def set_final(self, text: str) -> None:
        self._buf = text
        self.update(f"● {self._buf}")


class CognitiveBlock(Static):
    def __init__(self, ev: CogEvent) -> None:
        self.ev = ev
        self.expanded = False
        super().__init__(self.render_summary())

    def render_summary(self) -> str:
        icon = _ICON.get(self.ev.cog_type, "•")
        hint = " (ctrl+r)" if self.ev.cog_type == "memory_recalled" else (
            " (ctrl+o)" if self.ev.cog_type == "thinking" else "")
        return f"{icon} {self.ev.summary}{hint}"

    def render_detail(self) -> str:
        d = self.ev.data
        lines = [self.render_summary()]
        for it in d.get("items", []):
            src = it.get("source", "")
            badge = f"[{src}]" if src else ""
            lines.append(f"    · {it.get('content','')} {badge}".rstrip())
        if self.ev.cog_type == "thinking" and d.get("text"):
            lines.append(f"    {d['text']}")
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
        head = f"🔧 {humanize_tool(self.tool_name)} {pick_object(self.tool_name, self.params)}".rstrip()
        if self.status == "running":
            return f"{head} …"
        mark = "✓" if self.status == "ok" else "✗"
        summary = summarize_result(
            self.tool_name, self.result_meta, self.result_text, self.status == "ok"
        )
        return f"{head} · {summary} {mark}"

    def render_detail(self) -> str:
        lines = [self.render_summary()]
        if self.params:
            joined = ", ".join(f"{k}={_clip(v, 60)}" for k, v in self.params.items())
            lines.append(f"    ↳ 参数 {joined}")
        if self.result_text:
            lines.append(f"    ↳ 结果 {_clip(self.result_text, 200)}")
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
        if self.decision == "approve":
            return f"⚠️ {self.action} — ✅ 已批准"
        if self.decision == "deny":
            return f"⚠️ {self.action} — ❌ 已拒绝"
        return (f"⚠️ 需要确认: {self.action}\n    {self.risk}\n"
                f"    params={self.params}\n    [y] 批准  [n] 拒绝  [a] 本会话始终允许")

    def mark(self, decision: str) -> None:
        self.decision = decision
        self.update(self._body())
