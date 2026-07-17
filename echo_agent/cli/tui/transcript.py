"""Scrollable transcript container. Mounts turn/reply/cognitive/approval blocks
and maintains an index of heartbeat lines keyed by ``inbound_event_id`` so
progress notes update in place. Also tracks the most recent memory/thinking
cognitive blocks for the ctrl+r / ctrl+o expand shortcuts."""

from __future__ import annotations

from textual.containers import VerticalScroll

from echo_agent.cli.tui.blocks import (
    AgentReply,
    ApprovalBlock,
    ChoiceBlock,
    CognitiveBlock,
    ToolCallBlock,
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
        self._tool_blocks: dict[str, ToolCallBlock] = {}
        self._last_memory: CognitiveBlock | None = None
        self._last_thinking: CognitiveBlock | None = None

    def on_mount(self) -> None:
        # Anchor to the bottom so newly mounted blocks (replies, streaming
        # tokens, tool/heartbeat lines) auto-follow into view. Without this the
        # view kept its scroll position while content grew below the fold, so a
        # long reply looked "stuck". Textual's anchor is persistent and self-
        # releasing: scrolling up pauses the follow, scrolling back to the
        # bottom restores it — no manual scroll_end on every mount.
        self.anchor()

    @property
    def heartbeat_count(self) -> int:
        return len(self._heartbeats)

    def clear(self) -> None:
        """Remove all mounted blocks and reset the per-turn indexes.
        Used by the client-local /clear command — screen only, session intact."""
        self.remove_children()
        self._heartbeats.clear()
        self._tool_blocks.clear()
        self._last_memory = None
        self._last_thinking = None

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

    @property
    def tool_block_count(self) -> int:
        return len(self._tool_blocks)

    def add_tool_call(self, ev: CogEvent) -> ToolCallBlock:
        """Mount a tool line on the first (running) frame; flip it in place on
        the paired done frame. Keyed by tool_call_id, mirroring heartbeat_line."""
        d = ev.data
        tcid = str(d.get("tool_call_id", ev.cog_event_id))
        existing = self._tool_blocks.get(tcid)
        if existing is not None:
            existing.mark_done(
                d.get("status", "ok"), d.get("result_meta"),
                str(d.get("result_text", "")), d.get("duration_ms"),
            )
            return existing
        b = ToolCallBlock(
            tcid, d.get("name", "tool"), d.get("params") or {},
            status=d.get("status", "running"),
            result_meta=d.get("result_meta"),
            result_text=str(d.get("result_text", "")),
            duration_ms=d.get("duration_ms"),
        )
        self._tool_blocks[tcid] = b
        self.mount(b)
        return b

    def add_approval(
        self, request_id: str, action: str, params: dict, risk: str
    ) -> ApprovalBlock:
        b = ApprovalBlock(request_id, action, params, risk)
        self.mount(b)
        return b

    def add_clarify(
        self, clarify_id: str, question: str, options: list[str]
    ) -> ChoiceBlock:
        b = ChoiceBlock(clarify_id, question, options)
        self.mount(b)
        return b

    def add_error(self, msg: str) -> AgentReply:
        """Show a server-side error frame (e.g. rate limited) in the transcript.

        The socket is still open when the gateway sends an ``error`` frame, so
        this surfaces the reason instead of silently swallowing it or faking a
        disconnect."""
        w = AgentReply()
        w.is_status = True
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

    def last_reply_text(self) -> str | None:
        """Plain text of the most recent agent reply, or None if there is no
        reply yet. Heartbeats subclass AgentReply, so they are excluded to keep
        /copy pointed at the actual answer, not a progress line. Status lines
        (server errors) reuse AgentReply but set is_status — also excluded, so a
        rate-limit/error right before /copy doesn't clobber the real last reply."""
        for w in reversed(list(self.children)):
            if isinstance(w, _Heartbeat):
                continue
            if isinstance(w, AgentReply):
                if getattr(w, "is_status", False):
                    continue
                return w.text
        return None

    def export_text(self) -> str:
        """The whole conversation as a plain-text transcript (user turns and
        agent replies in order), for /copy all. Cognitive/tool/heartbeat lines
        are skipped — they are UI scaffolding, not content worth copying."""
        parts: list[str] = []
        for w in self.children:
            if isinstance(w, _Heartbeat):
                continue
            if isinstance(w, UserTurn):
                parts.append(f"❯ {w.raw_text}")
            elif isinstance(w, AgentReply):
                parts.append(w.text)
        return "\n\n".join(p for p in parts if p)
