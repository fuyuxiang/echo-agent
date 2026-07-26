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
from echo_agent.cli.tui.status_phrases import choose_status_phrase


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
    # A VerticalScroll defaults to can_focus=True and binds up/down to
    # scroll_up/scroll_down. That put the container itself in the Tab focus
    # ring (…block → block → PromptInput → TranscriptView → block…), so after
    # the user visited the prompt and Tabbed back, focus landed on the
    # *container* rather than a block. From there the arrow keys hit the
    # container's own scroll bindings — the scrollbar "stole" up/down and block
    # selection was dead until the user Tabbed past it. The container is only a
    # scroll viewport for its blocks, never a selection target, so it should not
    # be focusable itself; its children stay focusable (can_focus_children
    # defaults to True) and Textual still scrolls a focused block into view.
    can_focus = False

    def __init__(self) -> None:
        super().__init__()
        self._heartbeats: dict[str, _Heartbeat] = {}
        self._tool_blocks: dict[str, ToolCallBlock] = {}
        self._last_memory: CognitiveBlock | None = None
        self._last_thinking: CognitiveBlock | None = None
        # Rolling window of recently shown status phrases so the heartbeat line
        # rotates without immediate repeats.
        self._status_recent: list[str] = []

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

    def clear(self, *, keep: list | None = None) -> None:
        """Remove all mounted blocks and reset the per-turn indexes.
        Used by the client-local /clear command — screen only, session intact.

        ``keep`` holds widgets that must survive (a pending approval/clarify the
        user still has to answer): everything else is removed around them, so the
        interactive prompt that the disabled input box refers to stays on screen
        with its internal state intact.
        """
        survivors = [w for w in (keep or []) if w in self.children]
        for w in list(self.children):
            if w in survivors:
                continue
            try:
                w.remove()
            except Exception:
                pass
        self._heartbeats.clear()
        self._tool_blocks.clear()
        self._last_memory = None
        self._last_thinking = None
        self._status_recent.clear()

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

    def add_notice(self, markup: str) -> AgentReply:
        """Show a client-local informational line (e.g. /help output, /theme
        confirmation). Reuses AgentReply's Static base but flags is_status so
        /copy skips it — it is UI chatter, not a real agent reply. The markup is
        author-trusted (not user text), so it is rendered verbatim."""
        w = AgentReply()
        w.is_status = True
        self.mount(w)
        w.set_markup(markup)
        return w

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
        # Show a friendly rotating phrase rather than the raw server note: the
        # note can be monotonous or leak model scratch text, and the concrete
        # progress already lives in the tool/thinking blocks above. Each tick
        # re-picks (avoiding recent repeats) so the line feels alive.
        phrase = choose_status_phrase(self._status_recent)
        hb = self._heartbeats.get(inbound_event_id)
        if hb is None:
            hb = _Heartbeat(phrase)
            self._heartbeats[inbound_event_id] = hb
            self.mount(hb)
        else:
            hb.update_note(phrase)
        return hb

    def repaint_replies(self) -> None:
        """Re-render every markdown reply against the active theme.

        Called after a ``/theme`` switch: markdown replies bake their colours in
        at render time (Rich renderables bypass Textual's ``$var`` resolution),
        so without this the switch only affected content rendered afterwards and
        the existing conversation kept the old palette's hues.
        """
        for w in self.children:
            if isinstance(w, AgentReply):
                try:
                    w.repaint()
                except Exception:
                    pass

    def last_memory_block(self) -> CognitiveBlock | None:
        return self._last_memory

    def clear_heartbeats(self) -> None:
        """Remove the rotating "⏳ …" progress lines.

        They were mounted per inbound_event_id and never removed, so every past
        turn left a phantom "还在处理" line in the transcript and ``_heartbeats``
        grew without bound across a session.

        Safe to call whenever a reply lands, including on the intermediate
        ``is_final`` frames the gateway emits mid-turn (text → tool → more text):
        the index is cleared too, so a later heartbeat simply mounts a fresh
        line rather than updating a removed widget.
        """
        for hb in list(self._heartbeats.values()):
            try:
                hb.remove()
            except Exception:
                pass
        self._heartbeats.clear()

    def end_turn_cleanup(self) -> None:
        """Retire progress scaffolding after a turn died without finishing
        (gateway ``error`` frame, socket drop).

        Beyond the heartbeat lines, this settles tool lines whose paired "done"
        frame will now never arrive: they stayed rendered as ``🔧 执行 …``,
        indistinguishable from a command still running.

        Deliberately NOT called on the normal reply path. The gateway splits one
        answer across several ``is_final`` frames, so a final can arrive while
        tools are still executing — marking those "未完成" would be wrong, and
        dropping their correlation would make the real done frame mount a second,
        duplicate line for the same call.
        """
        self.clear_heartbeats()
        for tcid, block in list(self._tool_blocks.items()):
            if block.status == "running":
                block.mark_interrupted()
            del self._tool_blocks[tcid]

    def last_thinking_block(self) -> CognitiveBlock | None:
        return self._last_thinking

    def last_turn_reply_text(self) -> str | None:
        """Plain text of the most recent *turn's* full reply, or None if there
        is no reply yet. A single logical answer is frequently split across
        several AgentReply blocks (the gateway emits text → tool call → more
        text as separate final frames), so returning only the last block copies
        just the tail the user can see at the bottom of the screen, dropping the
        earlier parts of the same answer. This walks back to the last UserTurn and
        joins every real AgentReply after it, so /copy captures the whole latest
        answer. Heartbeats (progress lines) and status lines (server errors) reuse
        AgentReply but are excluded — they are UI chatter, not the answer."""
        parts: list[str] = []
        for w in reversed(list(self.children)):
            if isinstance(w, UserTurn):
                break
            if isinstance(w, _Heartbeat):
                continue
            if isinstance(w, AgentReply):
                if getattr(w, "is_status", False):
                    continue
                if w.text:
                    parts.append(w.text)
        if not parts:
            return None
        parts.reverse()
        return "\n\n".join(parts)

    def export_text(self) -> str:
        """The whole conversation as a plain-text transcript (user turns and
        agent replies in order), for /copy all. Cognitive/tool/heartbeat lines
        are skipped — they are UI scaffolding, not content worth copying.

        Status lines (is_status) are skipped for the same reason, matching
        export_markdown: /help output, /theme confirmations and disconnect
        notices are client-local chatter, and because they carry hand-built Rich
        markup they landed in the clipboard as raw "[$text-muted]…[/]" tags.
        """
        parts: list[str] = []
        for w in self.children:
            if isinstance(w, _Heartbeat):
                continue
            if isinstance(w, UserTurn):
                parts.append(f"❯ {w.raw_text}")
            elif isinstance(w, AgentReply):
                if getattr(w, "is_status", False):
                    continue
                parts.append(w.text)
        return "\n\n".join(p for p in parts if p)

    def export_markdown(self, *, session_key: str = "", when: str = "") -> str:
        """The conversation as a readable Markdown document, for /save. Same
        content selection as export_text (user turns + real agent replies; UI
        scaffolding skipped), but rendered with a heading, an optional metadata
        block, and one ``## 用户`` / ``## 助手`` section per message so the
        file reads well in any Markdown viewer. Status lines (is_status) reuse
        AgentReply and are excluded — they are UI chatter, not conversation."""
        lines: list[str] = ["# Echo 对话记录", ""]
        meta: list[str] = []
        if session_key:
            meta.append(f"- 会话: `{session_key}`")
        if when:
            meta.append(f"- 导出时间: {when}")
        if meta:
            lines.extend(meta)
            lines.append("")
        for w in self.children:
            if isinstance(w, _Heartbeat):
                continue
            if isinstance(w, UserTurn):
                lines.append("## 用户")
                lines.append("")
                lines.append(w.raw_text)
                lines.append("")
            elif isinstance(w, AgentReply):
                if getattr(w, "is_status", False):
                    continue
                body = w.text
                if not body:
                    continue
                lines.append("## 助手")
                lines.append("")
                lines.append(body)
                lines.append("")
        return "\n".join(lines).rstrip() + "\n"
