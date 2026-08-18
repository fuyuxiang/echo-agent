"""Scrollable transcript container. Mounts turn/reply/cognitive/approval blocks
in arrival order and owns the blank-line rhythm between them (see layout.py).
Also tracks the most recent memory/thinking cognitive blocks for the ctrl+r /
ctrl+o expand shortcuts.

Progress ("still working") is deliberately NOT a transcript block: it lives in
the footer-docked ActivityLine. As a block its position was fixed when the first
beat arrived while tool lines kept appending below it, so "还在处理" rendered
*above* work that had already finished."""

from __future__ import annotations

from rich.markup import escape
from textual.containers import VerticalScroll

from echo_agent.cli.tui.blocks import (
    AgentReply,
    ApprovalBlock,
    ChoiceBlock,
    CognitiveBlock,
    ToolCallBlock,
    UserTurn,
)
from echo_agent.cli.tui.details import DetailPrefs, parse_env
from echo_agent.cli.tui.glyphs import GLYPHS, cog_glyph
from echo_agent.cli.tui.layout import lead_gap
from echo_agent.cli.tui.protocol import CogEvent


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
        self._tool_blocks: dict[str, ToolCallBlock] = {}
        # Thinking blocks by thinking_id, so a round's streamed snapshots update
        # one line instead of mounting a new one per flush.
        self._thinking_blocks: dict[str, CognitiveBlock] = {}
        self._last_memory: CognitiveBlock | None = None
        self._last_thinking: CognitiveBlock | None = None
        # Group of the previously mounted block, the only input to the spacing
        # rule. Blank lines used to come from per-block CSS margins, which gave
        # every block the same gap regardless of neighbour: a run of ten trace
        # lines was as airy as the boundary between a trace run and the answer,
        # and blocks that render nothing still painted their margin.
        self._prev_group: str | None = None
        # Monotonic turn counter. Every block mounted carries the turn it belongs
        # to, which is what lets /copy, the ctrl+r/ctrl+o shortcuts and the
        # indent rails reason about "this turn" without nesting the widget tree —
        # see turn_layout for why a container widget was rejected.
        self._turn_seq = 0
        # Event ID of the last completed reply, used for reconnection dedup.
        self._last_reply_event_id: str = ""
        # Set by /clear to signal that the next replay should not dedup
        # (the on-screen text is gone, so text comparison would be wrong).
        self._cleared_since_last_reply = False
        # Which trace sections are visible/expanded. Read from the environment at
        # construction so a user's shell default applies to the very first turn,
        # then mutated in place by /details.
        self.details: DetailPrefs = parse_env()

    def on_mount(self) -> None:
        # Anchor to the bottom so newly mounted blocks (replies, streaming
        # tokens, tool lines) auto-follow into view. Without this the
        # view kept its scroll position while content grew below the fold, so a
        # long reply looked "stuck". Textual's anchor is persistent and self-
        # releasing: scrolling up pauses the follow, scrolling back to the
        # bottom restores it — no manual scroll_end on every mount.
        self.anchor()

    def _place(self, widget) -> None:
        """Mount a block, inserting a leading blank line only at a group
        boundary (layout.lead_gap).

        The gap is applied as the widget's own top margin rather than a spacer
        widget, so ``self.children`` stays a clean list of content blocks for the
        /copy, /save and ctrl+r/ctrl+o walks. It is computed from the *previous*
        block's group alone, which is what makes it streaming-safe: a reply's
        spacing is decided when it mounts and never shifts as tokens arrive.
        """
        group = getattr(widget, "block_group", "note")
        if lead_gap(self._prev_group, group):
            widget.styles.margin = (1, 0, 0, 0)
        self._prev_group = group
        # Stamp the turn AFTER the counter has been advanced by add_user, so a
        # turn's title and everything the agent does in response share one id.
        try:
            widget.turn_seq = self._turn_seq
        except AttributeError:
            # Widgets whose class defines turn_seq as a read-only property.
            pass
        self.mount(widget)

    def mount_block(self, widget) -> None:
        """Public entry point for mounting a caller-built block (the banner), so
        outside code goes through the spacing rule instead of bypassing it with a
        bare ``mount`` and desynchronising the group bookkeeping."""
        self._place(widget)

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
        self._tool_blocks.clear()
        self._thinking_blocks.clear()
        self._last_memory = None
        self._last_thinking = None
        self._cleared_since_last_reply = True
        # Survivors keep whatever margin they were mounted with; the next block
        # is spaced as if it opens a fresh screen.
        self._prev_group = None
        # The counter deliberately keeps running rather than resetting to 0: a
        # kept survivor (a pending clarify the user still has to answer) carries
        # its original turn_seq, and reusing that number for the next turn would
        # merge two unrelated turns into one group.

    def add_user(self, text: str) -> UserTurn:
        # A user message opens a new turn: everything mounted until the next one
        # is that turn's working area.
        self._turn_seq += 1
        # Drop the previous turn's expand targets. ctrl+r / ctrl+o used to reach
        # the last memory/thinking block of ANY turn, so pressing them early in a
        # new turn silently expanded a trace line from further up the scrollback —
        # off-screen, so the keypress looked like it did nothing.
        self._last_memory = None
        self._last_thinking = None
        # Thinking ids are unique per round, so this is about not holding every
        # past turn's blocks alive in the index, not about collisions. Blocks
        # whose stream is still OPEN are kept: the user can queue a new turn while
        # the previous one is running (see PromptInput's queue confirmation), and
        # dropping the index made that round's remaining frames mount a SECOND
        # widget for the same thinking_id, filed under the new turn. The still-open
        # ones are retired by end_turn/mark_stream_ended when their round settles.
        for tid, block in list(self._thinking_blocks.items()):
            if not block.is_streaming:
                del self._thinking_blocks[tid]
        w = UserTurn(text)
        self._place(w)
        return w

    def start_reply(self, turn_seq: int = 0) -> AgentReply:
        w = AgentReply()
        self._place(w)
        if turn_seq > 0:
            w.turn_seq = turn_seq
        return w

    def add_cognitive(self, ev: CogEvent) -> CognitiveBlock | None:
        """Mount a cognitive trace line, or return None when /details hides its
        section. None (rather than an unmounted block) so callers cannot keep a
        reference to a widget that will never appear and then act on it.

        Thinking frames that carry a ``thinking_id`` are snapshots of one LLM
        round: the first mounts a block, later ones update it in place, and a
        ``retracted`` one removes it. Without the pairing a streamed round left
        one line per flush stacked down the transcript, each a longer prefix of
        the same thought.
        """
        # An update to a line already on screen is settled BEFORE the visibility
        # gate. Hiding a section mid-round leaves its existing lines mounted (see
        # set_details), and dropping their closing frame would freeze one on
        # "思考中" for the rest of the session — or, on a retraction, keep text
        # that the reply body is about to repeat.
        thinking_id = str(ev.data.get("thinking_id", ""))
        if thinking_id:
            existing = self._thinking_blocks.get(thinking_id)
            if ev.data.get("retracted"):
                # The reasoning turned out to be the answer itself; the reply
                # body is about to carry the same text.
                self._drop_thinking(thinking_id)
                return None
            if existing is not None:
                existing.update_event(ev)
                return existing
        if not self.details.shows(ev.cog_type):
            return None
        # Expanded state is decided before mount so the block's first paint is
        # already the detail view — toggling after mount would flash the summary
        # for a frame and, with the bottom anchor engaged, jog the scroll.
        b = CognitiveBlock(ev, expanded=self.details.starts_expanded(ev.cog_type))
        self._place(b)
        if ev.cog_type == "memory_recalled":
            self._last_memory = b
        elif ev.cog_type == "thinking":
            self._last_thinking = b
            if thinking_id:
                self._thinking_blocks[thinking_id] = b
        return b

    def _drop_thinking(self, thinking_id: str) -> None:
        """Remove a retracted thinking line and forget every reference to it, so
        ctrl+o does not reach a widget that is no longer on screen."""
        block = self._thinking_blocks.pop(thinking_id, None)
        if block is None:
            return
        if self._last_thinking is block:
            self._last_thinking = None
        try:
            block.remove()
        except Exception:
            pass

    @property
    def tool_block_count(self) -> int:
        return len(self._tool_blocks)

    def set_details(self, prefs: DetailPrefs) -> None:
        """Adopt new /details settings and re-render what is already on screen.

        Only the expand state is re-applied; blocks that a newly-``hidden``
        section would have suppressed stay mounted. Retroactively removing lines
        the user has already read (and may have scrolled to) makes the transcript
        disagree with what they saw, and the state to rebuild them on the way back
        is gone — so ``hidden`` governs what arrives next, not history.
        """
        self.details = prefs
        for w in list(self.children):
            section = None
            if isinstance(w, ToolCallBlock):
                section = "tools"
            elif isinstance(w, CognitiveBlock):
                section = prefs.section_of(w.ev.cog_type)
            if section is None:
                continue
            w.set_detail_default(expanded=prefs.state(section) == "expanded")

    def add_tool_call(self, ev: CogEvent) -> ToolCallBlock | None:
        """Mount a tool line on the first (running) frame; flip it in place on
        the paired done frame, keyed by tool_call_id.

        With ``tools=hidden`` nothing is mounted for a call that succeeds, and
        None is returned. A call that FAILS still mounts on its done frame — the
        block is simply built there instead of being flipped in place, which is
        why the hidden running frame needs no bookkeeping of its own. See
        details.shows for why a failure ignores the setting.
        """
        d = ev.data
        tcid = str(d.get("tool_call_id", ev.cog_event_id))
        status = d.get("status", "running")
        failed = status not in ("running", "ok")
        existing = self._tool_blocks.get(tcid)
        if existing is not None:
            existing.mark_done(
                status, d.get("result_meta"),
                str(d.get("result_text", "")), d.get("duration_ms"),
            )
            return existing
        block = ToolCallBlock(
            tcid, d.get("name", "tool"), d.get("params") or {},
            status=status,
            result_meta=d.get("result_meta"),
            result_text=str(d.get("result_text", "")),
            duration_ms=d.get("duration_ms"),
            expanded=self.details.starts_expanded("tool_call"),
        )
        if not self.details.shows("tool_call", failed=failed, tool_name=d.get("name", "")):
            return None
        self._tool_blocks[tcid] = block
        self._place(block)
        return block

    def add_approval(
        self, request_id: str, action: str, params: dict, risk: str
    ) -> ApprovalBlock:
        b = ApprovalBlock(request_id, action, params, risk)
        self._place(b)
        return b

    def add_clarify(
        self, clarify_id: str, question: str, options: list[str]
    ) -> ChoiceBlock:
        b = ChoiceBlock(clarify_id, question, options)
        self._place(b)
        return b

    def add_notice(self, markup: str) -> AgentReply:
        """Show a client-local informational line (e.g. /help output, /theme
        confirmation). Reuses AgentReply's Static base but flags is_status so
        /copy skips it — it is UI chatter, not a real agent reply. The markup is
        author-trusted (not user text), so it is rendered verbatim."""
        w = AgentReply()
        w.is_status = True
        self._place(w)
        w.set_markup(markup)
        return w

    def add_error(self, msg: str) -> AgentReply:
        """Show a server-side error frame (e.g. rate limited) in the transcript.

        The socket is still open when the gateway sends an ``error`` frame, so
        this surfaces the reason instead of silently swallowing it or faking a
        disconnect."""
        w = AgentReply()
        w.is_status = True
        self._place(w)
        w.set_markup(
            f"[$error]{cog_glyph('approval_request')} 服务端错误:[/] "
            f"[$text-muted]{escape(str(msg))}[/]"
        )
        return w

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

    def end_turn_cleanup(self) -> None:
        """Retire progress scaffolding after a turn died without finishing
        (gateway ``error`` frame, socket drop).

        Settles tool lines whose paired "done" frame will now never arrive: they
        stayed rendered as a running "…" line, indistinguishable from a command
        still executing. Streamed thinking lines are settled for the same reason:
        a partial snapshot's closing frame comes from the same dead turn, so
        without this the line keeps claiming "思考中" indefinitely.

        Deliberately NOT called on the normal reply path. The gateway splits one
        answer across several ``is_final`` frames, so a final can arrive while
        tools are still executing — marking those "未完成" would be wrong, and
        dropping their correlation would make the real done frame mount a second,
        duplicate line for the same call.
        """
        for tcid, block in list(self._tool_blocks.items()):
            if block.status == "running":
                block.mark_interrupted()
            del self._tool_blocks[tcid]
        for tid, block in list(self._thinking_blocks.items()):
            if block.is_streaming:
                block.mark_stream_ended()
            del self._thinking_blocks[tid]

    def last_thinking_block(self) -> CognitiveBlock | None:
        return self._last_thinking

    def last_turn_reply_text(self) -> str | None:
        """Plain text of the most recent *turn's* full reply, or None if there
        is no reply yet. A single logical answer is frequently split across
        several AgentReply blocks (the gateway emits text → tool call → more
        text as separate final frames), so returning only the last block copies
        just the tail the user can see at the bottom of the screen, dropping the
        earlier parts of the same answer. This collects every real AgentReply
        belonging to the newest turn, so /copy captures the whole latest answer.

        Scoped by ``turn_seq`` rather than by walking back to a UserTurn widget:
        /clear can remove the title while keeping a pending clarify block, and the
        widget walk then ran past the top of the transcript and swept replies from
        older turns into the copy. Status lines (notices, server errors) reuse
        AgentReply but are excluded — they are UI chatter, not the answer."""
        replies = [
            w for w in self.children
            if isinstance(w, AgentReply) and not getattr(w, "is_status", False)
            and w.text
        ]
        if not replies:
            return None
        newest = max(getattr(w, "turn_seq", 0) for w in replies)
        parts = [w.text for w in replies if getattr(w, "turn_seq", 0) == newest]
        return "\n\n".join(parts) if parts else None

    def export_text(self) -> str:
        """The whole conversation as a plain-text transcript (user turns and
        agent replies in order), for /copy all. Cognitive/tool lines are skipped
        — they are UI scaffolding, not content worth copying.

        Status lines (is_status) are skipped for the same reason, matching
        export_markdown: /help output, /theme confirmations and disconnect
        notices are client-local chatter, and because they carry hand-built Rich
        markup they landed in the clipboard as raw "[$text-muted]…[/]" tags.
        """
        parts: list[str] = []
        for w in self.children:
            if isinstance(w, UserTurn):
                parts.append(f"{GLYPHS.user} {w.raw_text}")
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
