"""Scrollable transcript container. Mounts turn/reply/cognitive/approval blocks
and maintains an index of heartbeat lines keyed by ``inbound_event_id`` so
progress notes update in place. Also tracks the most recent memory/thinking
cognitive blocks for the ctrl+r / ctrl+o expand shortcuts."""

from __future__ import annotations

from textual.containers import VerticalScroll

from echo_agent.cli.tui.blocks import (
    AgentReply,
    ApprovalBlock,
    CognitiveBlock,
    UserTurn,
)
from echo_agent.cli.tui.protocol import CogEvent


class _Heartbeat(AgentReply):
    """Lightweight progress line; reuses AgentReply's Static base."""

    def __init__(self, note: str) -> None:
        self.renderable_note = note
        super().__init__()
        self.set_final(f"⏳ {note}")

    def update_note(self, note: str) -> None:
        self.renderable_note = note
        self.set_final(f"⏳ {note}")


class TranscriptView(VerticalScroll):
    def __init__(self) -> None:
        super().__init__()
        self._heartbeats: dict[str, _Heartbeat] = {}
        self._last_memory: CognitiveBlock | None = None
        self._last_thinking: CognitiveBlock | None = None

    @property
    def heartbeat_count(self) -> int:
        return len(self._heartbeats)

    def add_user(self, text: str) -> UserTurn:
        w = UserTurn(text)
        self.mount(w)
        return w

    def start_reply(self) -> AgentReply:
        w = AgentReply()
        self.mount(w)
        return w

    def add_cognitive(self, ev: CogEvent) -> CognitiveBlock:
        b = CognitiveBlock(ev)
        self.mount(b)
        if ev.cog_type == "memory_recalled":
            self._last_memory = b
        elif ev.cog_type == "thinking":
            self._last_thinking = b
        return b

    def add_approval(
        self, request_id: str, action: str, params: dict, risk: str
    ) -> ApprovalBlock:
        b = ApprovalBlock(request_id, action, params, risk)
        self.mount(b)
        return b

    def add_error(self, msg: str) -> AgentReply:
        """Show a server-side error frame (e.g. rate limited) in the transcript.

        The socket is still open when the gateway sends an ``error`` frame, so
        this surfaces the reason instead of silently swallowing it or faking a
        disconnect."""
        w = AgentReply()
        self.mount(w)
        w.set_final(f"⚠️ 服务端错误: {msg}")
        return w

    def heartbeat_line(self, inbound_event_id: str, note: str) -> _Heartbeat:
        hb = self._heartbeats.get(inbound_event_id)
        if hb is None:
            hb = _Heartbeat(note)
            self._heartbeats[inbound_event_id] = hb
            self.mount(hb)
        else:
            hb.update_note(note)
        return hb

    def last_memory_block(self) -> CognitiveBlock | None:
        return self._last_memory

    def last_thinking_block(self) -> CognitiveBlock | None:
        return self._last_thinking
