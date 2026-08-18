"""EchoTUI — the Textual root app. Serves as the WSBridge sink and owns
keybindings; upstream sends go through the injected send_coro."""

from __future__ import annotations

import time
from collections import deque

from textual.app import App, ComposeResult
from textual.binding import Binding
from textual.containers import Horizontal
from textual.widgets import OptionList, Static

from echo_agent.cli.tui.transcript import TranscriptView
from echo_agent.cli.tui.activity_line import ActivityLine
from echo_agent.cli.tui.prompt_input import PromptInput
from echo_agent.cli.tui.status_bar import StatusBar
from echo_agent.cli.tui.blocks import ApprovalBlock, ChoiceBlock
from echo_agent.cli.tui.completion import completion_insert, filter_commands, help_text
from echo_agent.cli.tui.details import parse_command as parse_details_arg
from echo_agent.cli.tui.theme import ECHO_THEME, ECHO_THEME_LIGHT, resolve_theme_name
from echo_agent.cli.tui.brand import load_brand
from echo_agent.cli.tui.turns import TurnRegistry
from echo_agent.cli.tui.protocol import (
    CogEvent, approve_command, deny_command, clarify_command,
)

class EchoTUI(App):
    CSS_PATH = "app.tcss"
    # Textual focuses the first can_focus widget in DOM order on mount, which is
    # TranscriptView (a VerticalScroll) — it would silently swallow printable
    # keys, so the prompt never showed typed text. Declaring AUTO_FOCUS hands
    # focus to PromptInput on mount, the framework-native way to fix this.
    AUTO_FOCUS = "PromptInput"

    BINDINGS = [
        Binding("ctrl+r", "toggle_memory", "记忆", show=False),
        Binding("ctrl+o", "toggle_thinking", "思考", show=False),
        # Ctrl+C is a guarded interrupt, not an instant quit: it denies a
        # pending approval, else clears prompt text, else arms a 2s "press
        # again to exit" window. Ctrl+D stays the immediate escape hatch.
        Binding("ctrl+c", "interrupt", "中断/退出", show=False, priority=True),
        # priority=True is load-bearing: TextArea binds "delete,ctrl+d" to
        # delete_right, and PromptInput holds focus in the normal case
        # (AUTO_FOCUS), so without priority the widget consumed Ctrl+D and the
        # app never quit — it deleted a character instead. Three separate hints
        # ("Ctrl+D 退出" in the banner, the disconnect notice, and the Ctrl+C
        # exit prompt) promise this key works, and after a drop it is the only
        # exit the user is told about.
        Binding("ctrl+d", "quit", "退出", show=False, priority=True),
        # y/n/a are declared as bindings (not on_key) because a focused
        # PromptInput (TextArea) consumes printable keys before on_key runs in
        # textual 8.2.8. check_action gates them so they only fire while an
        # approval is pending; when pending we also blur focus so the App-level
        # binding is not filtered out by TextArea.check_consume_key.
        Binding("y", "approve", "批准", show=False),
        Binding("n", "deny", "拒绝", show=False),
        Binding("a", "approve_always", "始终允许", show=False),
        # Clarify selection keys. Gated by check_action so they only fire while
        # a clarify is pending; otherwise they pass through to the focused
        # PromptInput (typing a digit into the prompt).
        Binding("1", "clarify_pick(1)", show=False),
        Binding("2", "clarify_pick(2)", show=False),
        Binding("3", "clarify_pick(3)", show=False),
        Binding("4", "clarify_pick(4)", show=False),
        Binding("5", "clarify_pick(5)", show=False),
        Binding("6", "clarify_pick(6)", show=False),
        Binding("7", "clarify_pick(7)", show=False),
        Binding("8", "clarify_pick(8)", show=False),
        Binding("9", "clarify_pick(9)", show=False),
        Binding("up", "clarify_move(-1)", show=False),
        Binding("down", "clarify_move(1)", show=False),
        Binding("enter", "clarify_accept", show=False),
        # The way BACK from free-text entry to option picking. Stepping into
        # free text is a single printable keystroke, so it happened by accident
        # (a stray letter, or clicking the box and typing) — and it used to be
        # irreversible: _clarify_free_input gated every selection key off for
        # good, leaving the options rendered but unreachable. check_action limits
        # this to "free-text mode with an empty box", so Escape keeps its normal
        # meaning everywhere else (closing the completion panel in particular).
        Binding("escape", "clarify_leave_free_input", show=False),
    ]

    def __init__(self, send_coro=None, session_key: str = "", interrupt_coro=None,
                 reconnect_coro=None, save_dir=None) -> None:
        super().__init__()
        self._send = send_coro
        # Default directory for /save without an explicit path. run_cli_attach
        # passes <workspace>/transcripts so saved conversations sit next to the
        # rest of the workspace data; None falls back to ./transcripts under the
        # cwd (unit tests / standalone runs where no workspace was resolved).
        from pathlib import Path
        self._save_dir = Path(save_dir) if save_dir is not None else Path.cwd() / "transcripts"
        # Sends a control-only interrupt frame ({"type":"interrupt"}) upstream so
        # the gateway can cooperatively stop the running turn. Distinct from
        # _send (ordinary messages) so an interrupt never becomes a chat turn.
        self._interrupt = interrupt_coro
        # Rebuilds the WS connection after a drop (re-auth + restart pump),
        # injected by run_client. None in unit tests / when unsupported. Invoked
        # by the /reconnect command and the auto-retry path.
        self._reconnect = reconnect_coro
        self._session_key = session_key
        # Connection state gates input: after a silent ws drop, submitting would
        # send into a dead socket and silently lose the message. False disables
        # the prompt until a reconnect succeeds.
        self._connected = True
        # Brand strings (name/prompt/welcome/goodbye) are configurable via
        # ECHO_BRAND_* so a white-label deployment can rebrand without code edits.
        self._brand = load_brand()
        self._replies: dict[str, object] = {}
        # Pending turn_seqs awaiting their accepted frame, in submit order.
        # Popped in on_turn_accepted to build _event_turn_seq.
        self._pending_turn_seqs: deque[int] = deque()
        # Maps event_id → turn_seq so replies are attributed to the correct turn
        # even when a second turn was submitted before the first reply arrived.
        self._event_turn_seq: dict[str, int] = {}
        # Model of in-flight turns keyed by event_id. Replaces the old single
        # _active_event_id string, which a control reply's (approve/deny/clarify)
        # accepted frame — or a second queued turn — would overwrite, making
        # Ctrl+C target the wrong turn and letting the approval prompt end the
        # original turn early. The registry keeps primary (conversation) turns
        # separate from control events; see turns.TurnRegistry.
        self._turns = TurnRegistry()
        # A single pending-approval slot is sufficient (no queue needed):
        # approval requests are serialized server-side by inference_stage Phase A
        # — that check is a serial for-loop where cli blocks in wait_for_decision
        # until this decision resolves, so at most one approval is ever
        # outstanding. Phase B runs concurrently but only for read-only,
        # non-conflicting tools that never raise an approval_request.
        self._pending_approval: ApprovalBlock | None = None
        # Single pending-clarify slot: clarify is serialized (the agent blocks
        # on wait_for_answer, so no second clarify can arrive mid-wait).
        self._pending_clarify: ChoiceBlock | None = None
        # True while a clarify is pending and the user has started typing a
        # free-text answer — the next PromptInput submit is a clarify answer,
        # not an ordinary turn.
        self._clarify_free_input = False
        # Timestamp of the last Ctrl+C that armed the exit guard. A second
        # Ctrl+C within CTRL_C_EXIT_WINDOW seconds exits; 0.0 means unarmed.
        self._last_ctrl_c = 0.0
        # Timestamp of the last submit that armed the "a turn is still running —
        # send anyway?" confirmation. A second submit within
        # QUEUE_CONFIRM_WINDOW seconds actually sends it as a queued turn; 0.0
        # means unarmed. Guards against a reply-to-question being swallowed as an
        # out-of-context new turn while the previous turn is mid-flight (the
        # gateway serializes primary turns per session, so it would just queue
        # behind the running one rather than answer the question on screen).
        self._last_queue_confirm = 0.0

    # Seconds within which a second Ctrl+C confirms exit (matches the common
    # 2s window used by shells and other agent CLIs).
    CTRL_C_EXIT_WINDOW = 2.0

    # Seconds within which a second submit confirms "send anyway while a turn is
    # running". Slightly longer than the Ctrl+C window: the user must read the
    # hint and decide, not just double-tap Enter reflexively.
    QUEUE_CONFIRM_WINDOW = 4.0

    @property
    def goodbye_message(self) -> str:
        """Public accessor for the farewell line printed after teardown. Callers
        (run_client) use this instead of reaching into the private _brand field,
        so a test double or an alternate front-end can supply its own without
        replicating brand internals."""
        return self._brand.goodbye

    def compose(self) -> ComposeResult:
        yield TranscriptView()
        # Progress lives here, docked between transcript and input, NOT as a
        # transcript block: a block's position froze at the first heartbeat while
        # tool lines kept appending below it, so "还在处理" ended up above work
        # that had already finished. One fixed line also means progress never
        # scrolls away and never lengthens the transcript.
        yield ActivityLine()
        panel = OptionList(id="slash_panel")
        panel.display = False
        yield panel
        with Horizontal(id="input_row"):
            yield Static(self._brand.prompt, id="prompt_sigil")
            yield PromptInput()
            # Placeholder is overlaid inside the input row (layer) rather than a
            # separate row below it — TextArea has no native placeholder, but a
            # dedicated line made the empty state look like a stray caption. It
            # sits over the input area and hides on first keystroke.
            yield Static(self._brand.placeholder, id="placeholder")
        yield StatusBar()

    def on_mount(self) -> None:
        # app is constructed only after a successful handshake, so mount means
        # connected. StatusBar is yielded in compose() and mounted by now, so
        # query_one is safe without a guard (unlike notify_disconnected, where
        # the socket may die before mount).
        # Register both palettes and pick one by probing the terminal (light
        # profiles get the readable light theme; everything else stays dark).
        self.register_theme(ECHO_THEME)
        self.register_theme(ECHO_THEME_LIGHT)
        self.theme = resolve_theme_name()
        self._mount_banner()
        bar = self.query_one(StatusBar)
        bar.set_session(self._session_key)
        bar.set_connection(True)

    def _mount_banner(self) -> None:
        """Brand banner on the transcript's first screen — a light touch of
        ritual on entry. Pure presentation; safe to skip if mounting fails."""
        from echo_agent.cli.tui.blocks import Banner

        try:
            self._tv.mount_block(Banner(
                self._session_key,
                name=self._brand.name,
                tagline=self._brand.tagline,
                welcome=self._brand.welcome,
            ))
        except Exception:
            pass

    def check_action(self, action: str, parameters):
        # Only surface approval keys while a decision is actually pending;
        # otherwise return None so the key passes through to the focused widget
        # (e.g. typing "y" into the prompt).
        if action in ("approve", "deny", "approve_always"):
            pending = (
                self._pending_approval is not None
                and self._pending_approval.decision is None
            )
            return True if pending else None
        if action in ("clarify_pick", "clarify_move", "clarify_accept"):
            # Only while a clarify is pending AND the user has not stepped into
            # free-text input. The test is "is the PROMPT focused?", not "is
            # anything focused?": transcript blocks (tool/cognitive) are
            # focusable, so a single Tab moved focus onto one of them and the old
            # `self.focused is None` check went False — every number/arrow/enter
            # key was then filtered out while the prompt stayed disabled, leaving
            # the user with no way to answer the clarify at all. Mirrors how the
            # approval keys gate (on pending state, not focus).
            active = (
                self._pending_clarify is not None
                and not self._clarify_free_input
            )
            return True if active else None
        if action == "clarify_leave_free_input":
            # Escape only reclaims the keyboard for option picking when there is
            # something to go back to AND the box is empty. A non-empty box means
            # the user is mid-answer, so Escape must not discard their text; an
            # open completion panel is handled by PromptInput itself (it stops
            # the key before it reaches an App binding).
            if self._pending_clarify is None or not self._clarify_free_input:
                return None
            if not self._pending_clarify.options:
                # A free-text-only clarify has no options to return to.
                return None
            try:
                return True if self.query_one(PromptInput).is_empty else None
            except Exception:
                return None
        return True

    @property
    def _tv(self) -> TranscriptView:
        return self.query_one(TranscriptView)

    @property
    def _activity(self) -> ActivityLine:
        return self.query_one(ActivityLine)

    def _activity_call(self, method: str, *args) -> None:
        """Drive the docked progress line, tolerating its absence.

        Every caller sits on a frame-handling path that must never raise: the
        widget may not be mounted yet (frames can arrive during startup) and
        progress is pure decoration, so a failure here must not take down the
        turn that owns the actual answer.
        """
        try:
            getattr(self._activity, method)(*args)
        except Exception:
            pass

    # --- WSBridge sink ---
    def on_turn_accepted(self, event_id: str) -> None:
        """Classify an `accepted` frame against the oldest un-acked send. A
        primary (conversation) turn becomes the interrupt target; a control
        reply (approve/deny/clarify) is tracked separately so it never becomes
        the Ctrl+C target nor stops the running turn's timer."""
        kind = self._turns.on_accepted(event_id)
        if kind == "primary" and event_id and self._pending_turn_seqs:
            self._event_turn_seq[event_id] = self._pending_turn_seqs.popleft()

    def on_user_reply_token(self, inbound_id: str, text: str) -> None:
        r = self._replies.get(inbound_id)
        if r is None:
            ts = self._event_turn_seq.get(inbound_id, 0)
            r = self._tv.start_reply(turn_seq=ts)
            self._replies[inbound_id] = r
        r.append_token(text)

    def on_user_reply_reset(self, inbound_id: str) -> None:
        """Drop the draft streamed so far for this turn.

        The server retracted an optimistic draft (it turned out to be a pre-tool
        preamble). Tokens accumulate into one widget per turn, so the next
        iteration's text would otherwise be appended to the abandoned draft and
        the user would see the two spliced together until the final frame landed.
        The widget is kept and cleared rather than removed, so the reply stays in
        place in the transcript and the next token just refills it."""
        r = self._replies.get(inbound_id)
        if r is not None:
            r.clear_stream()

    def on_user_reply_final(self, inbound_id: str, text: str) -> None:
        # Classify BEFORE rendering: control replies (approve/deny/clarify acks)
        # should not appear as assistant text in the transcript.
        kind = self._turns.on_final(inbound_id)
        r = self._replies.pop(inbound_id, None)
        if kind == "control":
            # If streaming had already started a widget, hide it.
            if r is not None:
                try:
                    r.remove()
                except Exception:
                    pass
            self._event_turn_seq.pop(inbound_id, None)
            return
        if r is None:
            ts = self._event_turn_seq.get(inbound_id, 0)
            r = self._tv.start_reply(turn_seq=ts)
        # Finished reply: render markdown now that the text is complete.
        # Streaming (append_token) stays plain text since partial markdown
        # is broken and re-parsing every token would flicker.
        r.set_markdown(text)
        # Track for reconnection dedup.
        if inbound_id:
            self._tv._last_reply_event_id = inbound_id
            self._tv._cleared_since_last_reply = False
        # Clean up the turn_seq mapping for this event.
        self._event_turn_seq.pop(inbound_id, None)
        # kind was resolved above (before rendering). For non-control replies,
        # stop offering an interrupt once the turn settles.
        if kind != "control":
            self._turns.note_turn_settled()
        if not self._turns.has_active_primary:
            self.query_one(StatusBar).stop_turn_timer()
            # A cancelled turn also ends through here: the gateway's interrupt is
            # cooperative, so it converges at a checkpoint and emits an ordinary
            # final frame. Read the flag BEFORE settling, which clears it.
            cancelled = False
            try:
                cancelled = self._activity.stop_requested
            except Exception:
                pass
            # Settle the docked progress line into "完成 · <用时>". It used to be
            # hidden here, which removed the only moving thing on screen and left
            # no completion signal at all — a finished turn and a hung one looked
            # the same. Only once NO primary turn remains, so a queued second
            # turn's live progress is not wiped. Tool lines are deliberately left
            # alone here: the gateway splits one answer across several final
            # frames, so tools may still be running.
            self._activity_call("settle", "done")
            # Unless the user asked to stop — then this final IS the last one, and
            # a tool line still rendered as a running "…" would keep claiming the
            # command is executing while the row below already says 已中断. That is
            # the same "stopped moving, done or stuck?" ambiguity, one line up.
            if cancelled:
                try:
                    self._tv.end_turn_cleanup()
                except Exception:
                    pass
            # A pending clarify/approval is deliberately LEFT ALONE here.
            #
            # This used to clear it and re-enable the prompt, on the theory that a
            # turn ending with one still pending meant the server had resolved it
            # some other way. That inference is wrong on the CLI: the gateway
            # splits one answer across several is_final frames (text → tool →
            # more text), and a CLI clarify has no timeout — the turn parks in
            # wait_for_answer holding the session lock until the user answers. So
            # an intermediate final would retire a prompt that was still live:
            # the picker went inert while still on screen, the next thing the user
            # typed became a NEW turn, and that turn queued forever behind the
            # very turn waiting on this clarify. That is the "正在思考 then nothing
            # happens" freeze.
            #
            # Retiring a clarify is now driven by explicit signals only: the
            # clarify_closed frame, a gateway error frame, a disconnect, an
            # interrupt, or the user answering. An approval keeps its own
            # server-side timeout and decision frames, so it needs no guess here
            # either.
            #
            # Return keyboard focus to the prompt now the turn is done. During
            # the turn, focus can drift off PromptInput onto a transcript block
            # (tool/cognitive blocks are focusable so a mouse click or Tab lands
            # on them) — and nothing on the normal reply path brought it back,
            # so the next keystroke went to that block (which ignores printable
            # keys) and the box looked frozen. Skip only while a pending
            # clarify/approval intentionally holds the keyboard (prompt disabled).
            if self._pending_clarify is None and self._pending_approval is None:
                self._refocus_prompt()

    def _retire_clarify(self, clarify_id: str = "", *, cancelled: bool = True) -> None:
        """Drop the pending clarify because it can no longer be answered.

        ``clarify_id`` scopes this to a specific prompt when the caller knows it
        (the clarify_closed frame): a stale frame for an already-retired prompt
        must not clear a NEWER one that has since been mounted. An empty id means
        "whatever is pending" — used by the paths that kill every prompt anyway
        (interrupt, reconnect, terminal error).

        ``cancelled`` greys the block out with an explanation. It is False only
        when the block already shows an accurate terminal state of its own.
        """
        blk = self._pending_clarify
        if blk is None:
            return
        if clarify_id and blk.clarify_id != clarify_id:
            return
        if cancelled:
            blk.mark_cancelled()
        else:
            blk.set_free_input(False)
        self._pending_clarify = None
        self._clarify_free_input = False
        self._unlock_prompt(focus=True)

    def on_cognitive(self, ev: CogEvent) -> None:
        if ev.cog_type == "heartbeat":
            # The beat now only refines the docked line's stage label; it mounts
            # nothing. The server note itself is dropped — it is monotonous and
            # can echo model scratch text, while the concrete progress already
            # reads off the tool/thinking lines in the transcript.
            self._activity_call("set_stage", str(ev.data.get("stage", "")))
            return
        if ev.cog_type == "approval_request":
            d = ev.data
            self._pending_approval = self._tv.add_approval(
                d.get("request_id", ""), d.get("action", ""),
                d.get("params", {}), d.get("risk", ""))
            # Disable the prompt so App-level y/n/a bindings are not filtered
            # out by a focused TextArea (textual 8.2.8 check_consume_key), and
            # so a mouse click cannot re-focus it and swallow the keys.
            self._lock_prompt()
            return
        if ev.cog_type == "clarify_request":
            d = ev.data
            self._pending_clarify = self._tv.add_clarify(
                d.get("clarify_id", ""), d.get("question", ""),
                d.get("options", []) or [],
            )
            self._clarify_free_input = False
            # Disable the prompt so App-level number/arrow bindings win over a
            # focused TextArea (textual 8.2.8 check_consume_key), and so a mouse
            # click cannot re-focus it and break option selection.
            self._lock_prompt()
            return
        if ev.cog_type == "clarify_closed":
            # The server closed this prompt (its tool call returned). This is the
            # ONLY signal that retires a pending clarify short of the user
            # answering it — see the comment in on_user_reply_final for why the
            # mid-turn final frames must not be used for that.
            self._retire_clarify(ev.data.get("clarify_id", ""))
            return
        if ev.cog_type == "approval_closed":
            # The server closed this approval (timeout or resolved externally).
            # Symmetric with clarify_closed: the TUI cannot guess from final
            # frames alone, so an explicit signal is needed.
            if self._pending_approval is not None:
                request_id = ev.data.get("request_id", "")
                if not request_id or self._pending_approval.request_id == request_id:
                    self._pending_approval = None
                    self._unlock_prompt()
            return
        if ev.cog_type == "cost_update":
            bar = self.query_one(StatusBar)
            bar.set_cost(ev.data.get("total_cost", 0.0))
            if ev.data.get("model"):
                bar.set_model(ev.data["model"])
            ctx_used = ev.data.get("context_used", 0)
            ctx_max = ev.data.get("context_max", 0)
            if ctx_max:
                bar.set_context(ctx_used, ctx_max)
            mem = ev.data.get("memory_count")
            if mem is not None:
                bar.set_memory_count(mem)
            # A cost_update means one LLM round just settled — but the TURN is
            # still running (more tool rounds, clarify waits, reflection reruns
            # may follow). Deliberately do NOT pause the elapsed-time display
            # here: the timer must span the WHOLE turn, not freeze at the first
            # round's duration. It stops only when the final reply lands
            # (stop_turn_timer). The Ctrl+C guard keys off turn-active state, not
            # this display, so it is unaffected either way.
            return
        if ev.cog_type == "tool_call":
            # Tool lines flip in place (running -> done) via a dedicated block,
            # keyed by tool_call_id. The generic cognitive block can't pair frames.
            block = self._tv.add_tool_call(ev)
            # Mirror the pairing onto the docked line so it can name what is
            # running without waiting for the next heartbeat (which may be up to
            # min_interval_sec away, or never on a short turn). Driven off the
            # frame's own status, not the block: with `/details 工具 隐藏` there is
            # no block, and the live line is then the ONLY place the user can see
            # that a tool is running at all.
            # Correlated by tool_call_id so parallel tools finishing out of
            # order remove the right entry from the live line.
            tcid = str(ev.data.get("tool_call_id", "") or ev.cog_event_id or "")
            if str(ev.data.get("status", "running")) == "running":
                name = block.tool_name if block is not None else str(
                    ev.data.get("name", "")
                )
                self._activity_call("tool_started", name, tcid)
            else:
                self._activity_call("tool_finished", tcid)
            return
        if ev.cog_type == "thinking" and ev.data.get("streaming"):
            # A partial thinking snapshot is proof the model is reasoning right
            # now, which is more timely than the heartbeat that would otherwise
            # be the only thing to set this stage. Also covers `/details 思考
            # 隐藏`, where the docked line is the only remaining signal.
            self._activity_call("set_stage", "thinking")
        self._tv.add_cognitive(ev)

    def on_error(self, msg: str) -> None:
        # A gateway `error` frame (e.g. rate limited, unauthorized) arrives on a
        # LIVE socket — it is not a disconnect. Surface the reason in the
        # transcript rather than flipping the status bar to "disconnected",
        # which would mislead the user into debugging their connection.
        self._tv.add_error(msg or "未知错误")
        # A gateway error frame is terminal for the turn: the request was
        # rejected, so no reply will land to clear the active flag. End the turn
        # now, otherwise the Ctrl+C guard would keep trying to interrupt a turn
        # that already died server-side.
        self._turns.on_terminal_error()
        try:
            self.query_one(StatusBar).stop_turn_timer()
        except Exception:
            pass
        # The turn died server-side, so the progress line and any tool line still
        # rendered as running are now lying about what is happening. Retire
        # them the same way the normal reply path does — but settled as "出错",
        # not "完成": the row is the user's only at-a-glance answer to "did that
        # work?", so a failure must not look like a success.
        self._activity_call("settle", "error")
        try:
            self._tv.end_turn_cleanup()
        except Exception:
            pass
        # Drop stream correlation for the dead turn(s): only on_user_reply_final
        # pops from _replies, so a turn ended by an error/interrupt left its entry
        # behind forever — one leaked widget reference per affected turn for the
        # life of the process.
        self._replies.clear()
        # A gateway error frame is one of the explicit signals that a pending
        # clarify is dead: the turn that would have consumed the answer died
        # server-side, so nothing will ever read it. Retire it here (this used to
        # ride on the now-removed guess in on_user_reply_final) or the prompt
        # would stay disabled with no way back.
        self._retire_clarify()
        if self._pending_approval is not None:
            self._pending_approval = None
            self._unlock_prompt(focus=False)
        # Same focus recovery as the normal reply path: an error ends the turn,
        # so pull keyboard focus back to the prompt in case it drifted onto a
        # transcript block. Skip while a pending clarify/approval owns the
        # keyboard (prompt disabled → focus() is a no-op anyway).
        if self._pending_clarify is None and self._pending_approval is None:
            self._refocus_prompt()

    def notify_disconnected(self) -> None:
        """Flip the status bar to the disconnected state after a silent ws
        close (gateway drops the socket with no error frame). Called by pump()
        when its async-for over the socket ends. Defensive: the app may not be
        fully mounted yet if the socket dies during startup.

        Also blocks conversation sends so the user can't submit into a dead
        socket (the message would be silently lost). The prompt stays editable so
        /reconnect and other local commands remain reachable; a notice points at
        it.

        Idempotent per drop: attach_client calls this from three places
        (send_coro, interrupt_coro and the pump's exit), so one real disconnect
        easily triggers several calls. Re-entering while already disconnected
        returns immediately rather than stacking another identical notice."""
        if not self._connected:
            return
        self._connected = False
        try:
            self.query_one(StatusBar).set_connection(False)
        except Exception:
            pass
        # One notice per drop (not on every re-entry), so a flapping link doesn't
        # spam the transcript.
        try:
            self._tv.add_error("连接已断开。输入 /reconnect 重连（Ctrl+D 退出）。")
        except Exception:
            pass
        # No further frames can arrive on this socket, so a live progress line or
        # a running tool line would sit there implying live progress. Settle them
        # as "连接已断开" rather than "完成": the turn may well still be running
        # server-side, but this client can no longer show it, and replay_missed_reply
        # covers the recovered answer.
        #
        # Only while a turn is actually in flight. settle() deliberately lets a
        # later outcome overwrite an earlier one (a gateway error is routinely
        # followed by a disconnect, and the more specific reason should win), so an
        # unconditional call here rewrote the *previous* turn's settled "完成" into
        # "连接已断开" whenever the link dropped while the user sat idle — and
        # /reconnect then relabelled it again. The answer above it had been
        # delivered and was still on screen, so the row was contradicting the
        # transcript. A drop with nothing running needs no activity update: the
        # status bar and the transcript notice already say the connection is gone.
        # Read the state directly rather than through _activity_call, which
        # swallows its return value (it exists to make fire-and-forget calls safe).
        try:
            turn_in_flight = bool(self._activity.is_active)
        except Exception:
            turn_in_flight = False
        if turn_in_flight:
            self._activity_call("settle", "disconnected")
        try:
            self._tv.end_turn_cleanup()
        except Exception:
            pass
        # The gateway synthesizes /__clarify_cancel__ on ws teardown, so a prompt
        # pending at the drop is already dead server-side. Retire it to match —
        # and, more importantly, to hand the keyboard back: the prompt has to stay
        # usable for /reconnect, which a locked clarify would block.
        self._retire_clarify()
        if self._pending_approval is not None:
            self._pending_approval = None
            self._unlock_prompt(focus=False)

    def notify_reconnected(self) -> None:
        """Restore the connected state after a successful reconnect. Called by
        the reconnect path in run_client."""
        self._connected = True
        # Drop in-flight turn correlation: a turn outstanding across the drop can
        # never be retired by an incoming frame (accepted is not re-sent, a final
        # produced during the outage was dropped and only its text is recovered
        # via replay_missed_reply). Without this, _primary/_pending_primary stay
        # set, has_active_primary is pinned True, and the queue-guard blocks every
        # later submit until the process restarts. Any turn still running
        # server-side delivers its final as a standalone reply, which still shows.
        self._turns.reset_on_reconnect()
        # Also clear the pending clarify/approval slots and re-enable the prompt:
        # their in-flight acks are unrecoverable across the drop too, and a stale
        # pending slot would keep the keyboard captured / prompt disabled. The
        # clarify goes through _retire_clarify so its block is greyed out with a
        # reason instead of being left on screen still advertising live keys.
        self._retire_clarify()
        self._pending_approval = None
        # Stream correlation is per-connection too: a partially-streamed reply
        # from before the drop can never receive its final, so its _replies entry
        # would linger and a same-id frame after the reconnect would append into
        # the old, abandoned widget.
        self._replies.clear()
        self._pending_turn_seqs.clear()
        self._event_turn_seq.clear()
        try:
            bar = self.query_one(StatusBar)
            bar.set_connection(True)
            # No primary turn is tracked anymore, so a still-running elapsed
            # timer would tick against nothing — stop it to match the reset state.
            bar.stop_turn_timer()
        except Exception:
            pass
        # Same reasoning as the timer: turn tracking was just reset, so a live
        # progress row would spin against a turn this client no longer follows.
        # Settled as "已中断" rather than left on the disconnect's "连接已断开":
        # the link is back (the status bar now says 已连接), so the row must stop
        # asserting a dead connection — what actually happened to that turn, from
        # this client's point of view, is that it stopped following it.
        self._activity_call("settle", "interrupted")
        self._unlock_prompt(focus=True)

    def replay_missed_reply(self, text: str, event_id: str = "") -> None:
        """Show a final reply recovered from history after a reconnect.

        The gateway drops live pushes to a dead socket without replay, so a reply
        produced during an outage would otherwise be lost to the CLI. Dedup by
        event_id first (reliable), then fall back to text comparison. Skip dedup
        entirely after /clear (on-screen text is gone)."""
        if not text or not text.strip():
            return
        # Event-id-based dedup: if the server tells us which reply this is and
        # it matches the last one we rendered, skip it.
        if event_id and event_id == self._tv._last_reply_event_id:
            return
        # Text-based fallback, unless /clear wiped the screen.
        if not self._tv._cleared_since_last_reply:
            try:
                last = self._tv.last_turn_reply_text()
            except Exception:
                last = ""
            if last and last.strip() == text.strip():
                return
        self._tv._cleared_since_last_reply = False
        self._tv.add_notice("[$text-muted]（补显示断连期间的回复）[/]")
        r = self._tv.start_reply()
        r.set_markdown(text)
        if event_id:
            self._tv._last_reply_event_id = event_id
        try:
            self._tv.add_notice("[$success]● 已重新连接[/]")
        except Exception:
            pass

    def on_key(self, event) -> None:
        # Enter free-text clarify input: only while a clarify is pending and the
        # user has not already stepped into free-text. A single printable
        # character (not consumed by the number/arrow/enter bindings) focuses the
        # prompt, seeds that char, and marks the next submit as a clarify answer.
        # Gated on _clarify_free_input rather than "nothing is focused" so a Tab
        # onto a transcript block cannot strand the user with a disabled prompt
        # and no working keys (same reasoning as check_action).
        if self._pending_clarify is None or self._clarify_free_input:
            return
        ch = event.character
        if ch is None or not ch.isprintable() or ch == " ":
            return
        # Digits 1-9 are quick-select bindings (clarify_pick); yield to them so
        # the binding fires instead of seeding a free-text answer. In textual
        # 8.2.8 this App-level on_key runs before binding resolution, so a
        # printable digit would otherwise be captured here first.
        if ch in "123456789":
            return
        self._enter_clarify_free_input(ch)
        event.prevent_default()
        event.stop()

    def _lock_prompt(self) -> None:
        # A pending clarify/approval owns the keyboard: disable the prompt so
        # the number/arrow/enter (or y/n/a) App-level bindings win. Disabling
        # (not just blurring) is what makes it robust against a mouse click —
        # textual won't focus a disabled widget, and disabling auto-blurs it if
        # it currently holds focus, so App.focused settles to None. Blurring
        # alone left a click able to re-focus the prompt and swallow the keys.
        try:
            self.query_one(PromptInput).disabled = True
        except Exception:
            pass

    def _unlock_prompt(self, *, focus: bool = True) -> None:
        # Re-enable the prompt (and optionally focus it) once the pending
        # clarify/approval is resolved or the user steps into free-text input.
        # Enable MUST precede focus(): focus() is a no-op on a disabled widget.
        try:
            pi = self.query_one(PromptInput)
            pi.disabled = False
            if focus:
                pi.focus()
        except Exception:
            pass

    def _refocus_prompt(self) -> None:
        # Put keyboard focus back on the prompt without touching its enabled
        # state (unlike _unlock_prompt, which also flips disabled). Used at turn
        # end to recover from focus drifting onto a transcript block during the
        # turn. focus() is a no-op on a disabled widget, so a still-locked prompt
        # (pending clarify/approval) is left alone. Best-effort — never crash the
        # sink if the widget isn't mounted.
        try:
            self.query_one(PromptInput).focus()
        except Exception:
            pass

    def _enter_clarify_free_input(self, char: str = "") -> None:
        # Step from option-picking into free-text: re-enable + focus the prompt,
        # mark the next submit as a clarify answer, and seed the first character
        # if one was given. Shared by on_key (printable non-digit), the
        # out-of-range digit fallback, and the "其他(自行输入)" sentinel option.
        self._clarify_free_input = True
        if self._pending_clarify is not None:
            # Surfaces the "Esc returns to the options" hint on the block itself.
            self._pending_clarify.set_free_input(True)
        self._unlock_prompt()
        if char:
            self.query_one(PromptInput).insert(char)

    def action_clarify_leave_free_input(self) -> None:
        """Return from free-text entry to option picking (Escape, empty box).

        check_action already established that a clarify with options is pending,
        that we are in free-text mode, and that the box is empty — so this only
        has to undo the mode switch: drop the flag and re-lock the prompt, which
        is what hands the number/arrow/enter keys back to the App bindings."""
        if self._pending_clarify is None or not self._clarify_free_input:
            return
        self._clarify_free_input = False
        self._pending_clarify.set_free_input(False)
        self._lock_prompt()

    # --- input ---
    async def on_prompt_input_submitted(
        self, message: PromptInput.Submitted
    ) -> None:
        text = message.text
        # Local commands execute inside the TUI and are never sent upstream;
        # server commands (/approve, /deny, /approvals) fall through to send.
        #
        # These are checked BEFORE the clarify free-text branch for the few
        # commands that must stay reachable while a clarify is pending. Typing
        # "/reconnect" into a clarify used to be sent verbatim as the ANSWER
        # ("/clarify c1 /reconnect"), so a drop that happened while the agent was
        # waiting for an answer left the user locked inside an unanswerable
        # clarify on a dead socket, with the completion panel still popping up as
        # if the command would run. Answer-shaped input (anything else) still
        # goes to the clarify.
        if await self._run_local_command(text, during_clarify=True):
            return
        # A clarify free-text answer is routed to the pending clarify, not sent
        # as a new conversation turn.
        if self._clarify_free_input and self._pending_clarify is not None:
            await self._answer_clarify(text)
            return
        if await self._run_local_command(text, during_clarify=False):
            return
        # Block conversation turns while disconnected — a send into a dead socket
        # is silently lost. Local commands above still work; point the user at
        # /reconnect instead of accepting a turn that goes nowhere.
        if not self._connected:
            self._tv.add_error("未连接。请先输入 /reconnect 重连。")
            return
        # Queue-guard: a real conversation turn (not a server control command
        # like /approve /deny) submitted while a primary turn is still running
        # would be classified as a NEW primary turn and — because the gateway
        # serializes primary turns per session — queued behind the running one
        # rather than answered in place. That is exactly how a reply to the
        # model's on-screen question ("全删还是逐条勾?") gets swallowed as an
        # out-of-context new turn. Require a second submit within the window to
        # confirm the user really means to queue a new turn, not answer the
        # question. Control commands (/approve …) are excluded: they act on the
        # running turn and must go through immediately.
        if not text.startswith("/") and self._turns.has_active_primary:
            now = time.monotonic()
            if now - self._last_queue_confirm >= self.QUEUE_CONFIRM_WINDOW:
                # First submit (or a stale one): arm the window, keep the text in
                # the box so a second Enter resends it, and tell the user why.
                self._last_queue_confirm = now
                self.query_one(PromptInput).restore_draft(text)
                self._tv.add_notice(
                    "[$text-muted]上一轮仍在进行中。当前回复不会打断它，"
                    "而是作为新一轮排在其后。再次回车确认发送，"
                    "或按 Ctrl+C 停止当前任务。[/]"
                )
                return
            # Second submit within the window: confirmed — fall through to send.
        # Clear the arm unconditionally on any real send so a stale timestamp
        # (e.g. the running turn ended before the second submit) can never make
        # a later, unrelated submit look pre-confirmed.
        self._last_queue_confirm = 0.0
        self._tv.add_user(text)
        self._pending_turn_seqs.append(self._tv._turn_seq)
        self.query_one(StatusBar).start_turn_timer()
        # Show progress from the submit, not from the first heartbeat: the
        # gateway waits out a silence threshold before its first beat, so the
        # opening seconds of every turn had no feedback at all.
        self._activity_call("start")
        # Tag this as a primary (conversation) turn so its accepted frame becomes
        # the Ctrl+C interrupt target — not a later control reply's frame.
        self._turns.note_send("primary")
        if self._send is not None:
            await self._send(text)

    # Local commands that stay reachable while a clarify is pending: escape
    # hatches and read-only helpers. Anything that would answer or mutate the
    # conversation is excluded, so ordinary answers still reach the clarify.
    _CLARIFY_SAFE_COMMANDS = frozenset({"/help", "/quit", "/reconnect"})

    async def _run_local_command(self, text: str, *, during_clarify: bool) -> bool:
        """Execute a client-local command. Returns True if ``text`` was handled.

        The command name is matched case-insensitively: the completion panel
        filters with ``text.lower()``, so typing "/HELP" showed "/help" as a
        valid match while dispatch compared exactly and sent "/HELP" upstream as
        a conversation turn. The argument is passed through with its original
        case (paths and theme names are the user's own text).

        ``during_clarify`` restricts the set to _CLARIFY_SAFE_COMMANDS so this
        can run ahead of the clarify-answer branch without swallowing answers.
        """
        head, _, raw_arg = text.partition(" ")
        name = head.lower()
        arg = raw_arg.strip()
        if during_clarify:
            if self._pending_clarify is None or name not in self._CLARIFY_SAFE_COMMANDS:
                return False
        if name == "/help":
            self._tv.add_notice(help_text())
            return True
        if name == "/clear":
            self._do_clear()
            return True
        if name == "/quit":
            self.exit()
            return True
        if name == "/copy" and arg in ("", "all"):
            self._do_copy(whole=arg == "all")
            return True
        if name == "/save":
            self._do_save(arg)
            return True
        if name == "/theme":
            self._do_theme(arg)
            return True
        if name == "/details":
            self._do_details(arg)
            return True
        if name == "/reconnect":
            await self._do_reconnect()
            return True
        return False

    def _do_clear(self) -> None:
        """/clear — wipe the transcript (screen only; the session is intact).

        A pending approval/clarify block is KEPT rather than removed. The prompt
        is disabled while one is outstanding, so clearing the block that
        explained why left a dead input box on an empty screen with no hint about
        which key to press — while the server sat parked waiting for the answer.
        The widgets are preserved in place (not rebuilt) so their internal state
        — a dict option's real answer value, the current highlight — survives.
        """
        keep = [
            w for w in (self._pending_approval, self._pending_clarify)
            if w is not None
        ]
        self._tv.clear(keep=keep)
        # The reply widgets those ids pointed at are gone, so a still-streaming
        # turn must start a fresh one instead of appending into a removed widget.
        self._replies.clear()
        # Blank the settled progress row too, but only when no turn is running:
        # its "完成 · 8.1s · 2 个工具" describes work that is no longer on screen.
        # A live turn keeps its row — that one is about the present, and the turn
        # survives /clear.
        try:
            if not self._activity.is_active:
                self._activity_call("reset")
        except Exception:
            pass

    async def _do_reconnect(self) -> None:
        """/reconnect — rebuild the WS connection after a drop. No-op (with a
        hint) when already connected or when no reconnect handler was injected."""
        if self._connected:
            self._tv.add_notice("[$text-muted]当前已连接，无需重连。[/]")
            return
        if self._reconnect is None:
            self._tv.add_error("此环境不支持重连，请重新启动 echo-agent cli。")
            return
        self._tv.add_notice("[$text-muted]正在重连…[/]")
        try:
            ok = await self._reconnect()
        except Exception:
            ok = False
        if ok:
            self.notify_reconnected()
        else:
            self._tv.add_error("重连失败。请确认网关仍在运行后重试 /reconnect。")

    def _do_copy(self, whole: bool) -> None:
        """Copy the last reply (default) or the whole transcript (/copy all) to
        the system clipboard via OSC 52. Terminal support varies (works in
        iTerm2/WezTerm/kitty; macOS Terminal.app does not), so we notify with
        the copied length rather than silently succeeding."""
        text = self._tv.export_text() if whole else self._tv.last_turn_reply_text()
        if not text:
            self.notify("暂无可复制的内容", severity="warning", timeout=3)
            return
        self.copy_to_clipboard(text)
        scope = "整段对话" if whole else "最近回复"
        self.notify(f"已复制{scope}（{len(text)} 字）到剪贴板", timeout=3)

    def _do_save(self, arg: str) -> None:
        """/save [路径] — write the whole conversation to a Markdown file.
        Unlike /copy this persists to disk (OSC 52 clipboard is flaky and caps
        out on long transcripts). With no arg it writes an auto-named file under
        self._save_dir (<workspace>/transcripts); an arg is taken as a target:
        a directory (or trailing-slash path) keeps the auto name inside it, any
        other value is used as the filename verbatim (``.md`` appended if
        missing). Relative args resolve against the default save dir so a bare
        ``/save notes`` lands with the rest, not wherever the gateway was
        launched."""
        from datetime import datetime
        from pathlib import Path

        # export_text is empty exactly when there is no real conversation (no
        # user turns / non-status agent replies), which is the same content
        # selection export_markdown uses — so this can never report "saved" for a
        # file that turns out to hold only the metadata header.
        if not self._tv.export_text().strip():
            self.notify("暂无可保存的对话", severity="warning", timeout=3)
            return
        md = self._tv.export_markdown(
            session_key=self._session_key,
            when=datetime.now().strftime("%Y-%m-%d %H:%M:%S"),
        )

        stamp = datetime.now().strftime("%Y%m%d-%H%M%S")
        auto_name = f"echo-{stamp}.md"
        base = self._save_dir
        if not arg:
            target = base / auto_name
        else:
            raw = Path(arg).expanduser()
            # A trailing slash or an existing directory means "put the
            # auto-named file in here"; otherwise treat arg as the filename.
            is_dir = arg.endswith("/") or raw.is_dir()
            if is_dir:
                target = (raw if raw.is_absolute() else base / raw) / auto_name
            else:
                if raw.suffix.lower() != ".md":
                    raw = raw.with_name(raw.name + ".md")
                target = raw if raw.is_absolute() else base / raw

        try:
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_text(md, encoding="utf-8")
        except OSError as e:
            self.notify(f"保存失败: {e}", severity="error", timeout=5)
            return
        self.notify(f"已保存对话到 {target}（{len(md)} 字）", timeout=4)

    def _do_theme(self, arg: str) -> None:
        """/theme — switch or report the active palette. `light`/`dark` set it;
        no arg reports the current one. Both palettes are registered in on_mount,
        so switching is just reassigning self.theme."""
        arg = arg.lower()
        if arg in ("light", "dark"):
            self.theme = "echo-light" if arg == "light" else "echo"
            # Markdown replies bake their colours in at render time, so the
            # switch alone left the existing conversation in the old palette
            # (dark teal + low-contrast grey on a white surface — the very
            # unreadability the light theme fixes). Repaint what's on screen.
            self._tv.repaint_replies()
        elif arg:
            self._tv.add_notice("[$warning]用法: /theme [light|dark][/]")
            return
        current = "浅色" if self.theme == "echo-light" else "深色"
        self._tv.add_notice(f"当前主题: [b]{current}[/b]")

    def _do_details(self, arg: str) -> None:
        """/details — how much of the agent's working trace the transcript shows.

        Bare ``/details`` reports the three sections and their state; an argument
        sets one of them. Reporting on both paths (not just the query) is
        deliberate: the reply doubles as the discoverable list of what can be
        changed, so a user who guessed the syntax wrong still learns it.
        """
        if arg:
            parsed = parse_details_arg(arg)
            if parsed is None:
                self._tv.add_notice(
                    "[$warning]用法: /details <思考|工具|状态> <展开|折叠|精简|隐藏>[/]"
                )
                return
            section, state = parsed
            self._tv.set_details(self._tv.details.with_section(section, state))
        rows = "\n".join(
            f"  [$primary]{label}[/]  {state}"
            for label, state in self._tv.details.describe()
        )
        self._tv.add_notice(f"[b]过程信息显示[/b]\n{rows}")

    def on_prompt_input_content_changed(
        self, message: PromptInput.ContentChanged
    ) -> None:
        self.query_one("#placeholder").display = message.is_empty
        # Emptying the box during a clarify returns the keyboard to the options.
        # Entering free text costs one keystroke, so it happens by accident
        # (a stray letter, a click-then-type-then-reconsider); erasing back to
        # nothing is the natural undo, and without this the only exits were
        # answering or Ctrl+C. Guarded on options existing — a free-text-only
        # clarify has nothing to return to and must keep the prompt live.
        if (
            message.is_empty
            and self._clarify_free_input
            and self._pending_clarify is not None
            and self._pending_clarify.options
        ):
            self._clarify_free_input = False
            self._pending_clarify.set_free_input(False)
            self._lock_prompt()
            return
        panel = self.query_one("#slash_panel", OptionList)
        matches = filter_commands(message.text)
        # Keep the ordered match list so PanelNav/PanelAccept can map the
        # OptionList highlight index back to the concrete SlashCommand.
        self._panel_matches = matches
        if matches:
            panel.clear_options()
            for c in matches:
                tag = "本地" if c.scope == "local" else "服务端"
                panel.add_option(
                    f"{c.name} [dim]{c.arg_template}[/dim]  {c.desc} [{tag}]"
                )
            # Refiltering resets the highlight; the user must re-enter the panel
            # with Up/Down, keeping Enter on submit until they actively select.
            panel.highlighted = None
            panel.display = True
        else:
            panel.display = False
        self.query_one(PromptInput).set_panel_visible(bool(matches))

    # --- completion panel keyboard wiring ---
    def on_prompt_input_panel_nav(
        self, message: PromptInput.PanelNav
    ) -> None:
        panel = self.query_one("#slash_panel", OptionList)
        if panel.display is False or panel.option_count == 0:
            return
        if message.direction > 0:
            panel.action_cursor_down()
        else:
            panel.action_cursor_up()

    def on_prompt_input_panel_accept(
        self, message: PromptInput.PanelAccept
    ) -> None:
        panel = self.query_one("#slash_panel", OptionList)
        idx = panel.highlighted
        matches = getattr(self, "_panel_matches", [])
        if idx is None or not (0 <= idx < len(matches)):
            return
        pi = self.query_one(PromptInput)
        pi.apply_completion(completion_insert(matches[idx]))
        self._close_panel()

    def on_prompt_input_panel_close(
        self, message: PromptInput.PanelClose
    ) -> None:
        self._close_panel()

    def _close_panel(self) -> None:
        panel = self.query_one("#slash_panel", OptionList)
        panel.display = False
        panel.highlighted = None
        self.query_one(PromptInput).set_panel_visible(False)

    # --- keybindings ---
    def action_toggle_memory(self) -> None:
        b = self._tv.last_memory_block()
        if b is not None:
            b.toggle()

    def action_toggle_thinking(self) -> None:
        b = self._tv.last_thinking_block()
        if b is not None:
            b.toggle()

    async def action_interrupt(self) -> None:
        """Guarded Ctrl+C. Priority:
          1. A pending approval → deny it (unblocks the server), stay running.
          2. Prompt has text → clear it (bash/readline convention), stay.
          3. A turn is running → send an interrupt frame so the gateway
             cooperatively stops it; stay running (do NOT arm exit).
          4. Idle & empty → first press arms a 2s window and warns; a second
             press within the window exits. Ctrl+D remains the instant exit."""
        # 1. Deny a pending approval instead of exiting mid-decision. This
        # cancels the active prompt on Ctrl+C and, unlike a
        # bare exit, actively unblocks the server-side approval gate.
        if (
            self._pending_approval is not None
            and self._pending_approval.decision is None
        ):
            self._last_ctrl_c = 0.0
            await self._decide("deny")
            return

        # 2. Clear a non-empty prompt rather than exit.
        pi = self.query_one(PromptInput)
        if not pi.is_empty:
            pi.text = ""
            self._last_ctrl_c = 0.0
            return

        # 3. A turn is in flight → request a cooperative stop instead of exiting.
        # The stop is best-effort and lands at the inference loop's next
        # checkpoint (it cannot abort a single long tool call mid-run), so we
        # tell the user it was requested rather than claiming an instant stop.
        # Key off the registry (not the display timer): a primary turn is active
        # from submit until its final reply, spanning tool rounds and approval
        # waits, and unaffected by control replies.
        turn_active = self._turns.has_active_primary
        # Also offer the interrupt when a reconnect left work running under an id
        # we can no longer name: the gateway stops whatever is running when the
        # target is empty. Without this, Ctrl+C during a post-reconnect turn armed
        # the exit prompt instead of stopping the agent — the user had no way to
        # halt work they could still see happening.
        if (turn_active or self._turns.may_be_running_uncorrelated) and self._interrupt is not None:
            self._last_ctrl_c = 0.0
            # Scope the stop to the oldest outstanding PRIMARY turn (the one the
            # gateway is running) — never a control reply's id, and never a
            # queued later turn. Empty (no accepted frame seen yet) → server
            # stops whatever is running, preserving the old behavior.
            await self._interrupt(self._turns.active_turn_id)
            # A stop while parked on a clarify also cancels it server-side
            # (loop._handle_interrupt calls clarify.cancel_session). Drop the
            # TUI's pending clarify too, or the next thing the user types would
            # be sent as an answer to a clarify that no longer exists.
            self._retire_clarify()
            # Acknowledge the stop on the progress row, not just in a toast that
            # fades after 3s. The interrupt is cooperative, so the turn keeps
            # running until it reaches a checkpoint — without this the row went on
            # spinning "调用工具 …" as though Ctrl+C had done nothing, and then
            # reported 完成 for a turn the user had cancelled.
            self._activity_call("note_stopping")
            self.notify("已请求停止当前任务…", severity="warning", timeout=3)
            return

        # 4. Two-press exit guard.
        now = time.monotonic()
        if now - self._last_ctrl_c < self.CTRL_C_EXIT_WINDOW:
            self.exit()
            return
        self._last_ctrl_c = now
        active = self._turns.has_active_primary
        hint = (
            "回复仍在服务端生成，无法中断；再次按 Ctrl+C 退出"
            if active else "再次按 Ctrl+C 退出（Ctrl+D 直接退出）"
        )
        self.notify(hint, severity="warning", timeout=self.CTRL_C_EXIT_WINDOW)

    async def _decide(self, decision: str, level: str = "") -> None:
        blk = self._pending_approval
        if blk is None or blk.decision is not None:
            return
        if self._send is not None:
            cmd = (approve_command(blk.request_id, level) if decision == "approve"
                   else deny_command(blk.request_id))
            # Control send: its accepted frame and ack reply must NOT become the
            # interrupt target nor stop the original (still-parked) turn's timer.
            self._turns.note_send("control")
            try:
                await self._send(cmd)
            except Exception:
                self.notify("发送失败，请重试", severity="error", timeout=3)
                return
            if not self._connected:
                self.notify("发送失败（连接已断开），请重连后重试", severity="error", timeout=3)
                return
        blk.mark("approve" if decision == "approve" else "deny")
        self._pending_approval = None
        # Re-enable and refocus the prompt for the next turn.
        self._unlock_prompt()

    async def action_approve(self) -> None:
        await self._decide("approve")

    async def action_deny(self) -> None:
        await self._decide("deny")

    async def action_approve_always(self) -> None:
        await self._decide("approve", "session")

    async def _answer_clarify(self, answer: str) -> None:
        blk = self._pending_clarify
        if blk is None or blk.answer is not None:
            return
        # An all-whitespace answer is not an answer: the model receives "" and,
        # having learned nothing, asks the very same question again — while this
        # block already reads "已选:" on screen. Keep the prompt live instead so
        # the keystroke is not silently converted into a wasted round trip.
        if not answer.strip():
            return
        if self._send is not None:
            # Control send (clarify answer): tracked separately so it doesn't
            # clobber the primary turn that is parked waiting for this answer.
            self._turns.note_send("control")
            try:
                await self._send(clarify_command(blk.clarify_id, answer))
            except Exception:
                self.notify("发送失败，请重试", severity="error", timeout=3)
                return
            if not self._connected:
                self.notify("发送失败（连接已断开），请重连后重试", severity="error", timeout=3)
                return
        blk.mark(answer)
        self._pending_clarify = None
        self._clarify_free_input = False
        self._unlock_prompt()

    async def action_clarify_pick(self, number: int) -> None:
        blk = self._pending_clarify
        if blk is None:
            return
        if blk.is_free_input_option(number):
            # The "其他(自行输入)" sentinel: step into free-text, no seed char.
            self._enter_clarify_free_input()
            return
        opt = blk.option_for_number(number)
        if opt is None:
            # Out-of-range digit: don't swallow the key. Fall back to free-text
            # input, seeding the digit as the first character so answers that
            # start with a number remain possible.
            self._enter_clarify_free_input(str(number))
            return
        await self._answer_clarify(opt)

    def action_clarify_move(self, delta: int) -> None:
        if self._pending_clarify is not None:
            self._pending_clarify.move(delta)

    async def action_clarify_accept(self) -> None:
        blk = self._pending_clarify
        if blk is None:
            return
        if blk.highlighted_is_free_input():
            # The "其他(自行输入)" sentinel is highlighted: step into free-text.
            self._enter_clarify_free_input()
            return
        opt = blk.highlighted_option()
        if opt is not None:
            await self._answer_clarify(opt)
