"""Transcript block widgets. Rendering logic is kept in plain methods
(render_summary/render_detail) so it's unit-testable without a live screen."""

from __future__ import annotations

from textual.widgets import Static

from echo_agent.cli.tui.protocol import CogEvent

_ICON = {
    "memory_recalled": "🧠", "memory_written": "✍", "thinking": "💭",
    "tool_call": "🔧", "approval_request": "⚠️", "cost_update": "💰",
    "heartbeat": "⏳", "evolution": "🧬",
}


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
        if self.ev.cog_type == "tool_call":
            lines.append(f"    params={d.get('params')} → {d.get('result_summary','')}")
        return "\n".join(lines)

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
