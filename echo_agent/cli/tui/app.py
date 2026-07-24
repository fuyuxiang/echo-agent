"""EchoTUI — the Textual root app. Serves as the WSBridge sink and owns
keybindings; upstream sends go through the injected send_coro."""

from __future__ import annotations

import time

from textual.app import App, ComposeResult
from textual.binding import Binding
from textual.containers import Horizontal
from textual.widgets import OptionList, Static

from echo_agent.cli.tui.transcript import TranscriptView
from echo_agent.cli.tui.prompt_input import PromptInput
from echo_agent.cli.tui.status_bar import StatusBar
from echo_agent.cli.tui.blocks import ApprovalBlock, ChoiceBlock
from echo_agent.cli.tui.completion import completion_insert, filter_commands, help_text
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
        Binding("ctrl+d", "quit", "退出", show=False),
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
            self._tv.mount(Banner(
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
            # Only while a clarify is pending AND the prompt is not focused
            # (blurred on mount of the block). When the user has stepped into
            # free-text input the prompt is focused again, so these bindings
            # yield to normal typing/submit.
            active = self._pending_clarify is not None and self.focused is None
            return True if active else None
        return True

    @property
    def _tv(self) -> TranscriptView:
        return self.query_one(TranscriptView)

    # --- WSBridge sink ---
    def on_turn_accepted(self, event_id: str) -> None:
        """Classify an `accepted` frame against the oldest un-acked send. A
        primary (conversation) turn becomes the interrupt target; a control
        reply (approve/deny/clarify) is tracked separately so it never becomes
        the Ctrl+C target nor stops the running turn's timer."""
        self._turns.on_accepted(event_id)

    def on_user_reply_token(self, inbound_id: str, text: str) -> None:
        r = self._replies.get(inbound_id)
        if r is None:
            r = self._tv.start_reply()
            self._replies[inbound_id] = r
        r.append_token(text)

    def on_user_reply_final(self, inbound_id: str, text: str) -> None:
        r = self._replies.pop(inbound_id, None)
        if r is None:
            r = self._tv.start_reply()
        # Finished reply: render markdown now that the text is complete.
        # Streaming (append_token) stays plain text since partial markdown
        # is broken and re-parsing every token would flicker.
        r.set_markdown(text)
        # Retire this turn from the registry. Only stop the timer once NO primary
        # (conversation) turn remains outstanding — a control reply's final
        # (approve/deny/clarify ack) must not stop the original turn's timer, and
        # a queued second turn must keep it running.
        self._turns.on_final(inbound_id)
        if not self._turns.has_active_primary:
            self.query_one(StatusBar).stop_turn_timer()
            # Defensive: a primary turn ending with a clarify/approval still
            # marked pending means the server resolved/ended it without the
            # normal answer path firing (e.g. timeout, error). Clear the stale
            # pending state and re-enable the prompt so it can't stay locked.
            if self._pending_clarify is not None or self._pending_approval is not None:
                self._pending_clarify = None
                self._pending_approval = None
                self._clarify_free_input = False
                self._unlock_prompt(focus=False)
            # Return keyboard focus to the prompt now the turn is done. During
            # the turn, focus can drift off PromptInput onto a transcript block
            # (tool/cognitive blocks are focusable so a mouse click or Tab lands
            # on them) — and nothing on the normal reply path brought it back,
            # so the next keystroke went to that block (which ignores printable
            # keys) and the box looked frozen. Skip only while a pending
            # clarify/approval intentionally holds the keyboard (prompt disabled).
            if self._pending_clarify is None and self._pending_approval is None:
                self._refocus_prompt()

    def on_cognitive(self, ev: CogEvent) -> None:
        if ev.cog_type == "heartbeat":
            self._tv.heartbeat_line(
                ev.inbound_event_id, ev.data.get("note", ev.summary)
            )
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
            self._tv.add_tool_call(ev)
            return
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
        it."""
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

    def notify_reconnected(self) -> None:
        """Restore the connected state after a successful reconnect. Called by
        the reconnect path in run_client."""
        self._connected = True
        try:
            self.query_one(StatusBar).set_connection(True)
        except Exception:
            pass
        try:
            self.query_one(PromptInput).focus()
        except Exception:
            pass

    def replay_missed_reply(self, text: str) -> None:
        """Show a final reply recovered from history after a reconnect.

        The gateway drops live pushes to a dead socket without replay, so a reply
        produced during an outage would otherwise be lost to the CLI. Dedup
        against the last on-screen reply so a reconnect that missed nothing does
        not echo the previous answer again."""
        if not text or not text.strip():
            return
        try:
            last = self._tv.last_turn_reply_text()
        except Exception:
            last = ""
        if last and last.strip() == text.strip():
            return
        self._tv.add_notice("[$text-muted]（补显示断连期间的回复）[/]")
        r = self._tv.start_reply()
        r.set_markdown(text)
        try:
            self._tv.add_notice("[$success]● 已重新连接[/]")
        except Exception:
            pass

    def on_key(self, event) -> None:
        # Enter free-text clarify input: only while a clarify is pending and
        # the prompt is blurred. A single printable character (not consumed by
        # the number/arrow/enter bindings) focuses the prompt, seeds that char,
        # and marks the next submit as a clarify answer.
        if self._pending_clarify is None or self.focused is not None:
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
        self._unlock_prompt()
        if char:
            self.query_one(PromptInput).insert(char)

    # --- input ---
    async def on_prompt_input_submitted(
        self, message: PromptInput.Submitted
    ) -> None:
        text = message.text
        # A clarify free-text answer is routed to the pending clarify, not sent
        # as a new conversation turn.
        if self._clarify_free_input and self._pending_clarify is not None:
            await self._answer_clarify(text)
            return
        # Local commands execute inside the TUI and are never sent upstream;
        # server commands (/approve, /deny, /approvals) fall through to send.
        if text == "/help":
            self._tv.add_notice(help_text())
            return
        if text == "/clear":
            self._tv.clear()
            return
        if text == "/quit":
            self.exit()
            return
        if text == "/copy" or text == "/copy all":
            self._do_copy(whole=text == "/copy all")
            return
        if text == "/save" or text.startswith("/save "):
            self._do_save(text[len("/save"):].strip())
            return
        if text == "/theme" or text.startswith("/theme "):
            self._do_theme(text[len("/theme"):].strip())
            return
        if text == "/reconnect":
            await self._do_reconnect()
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
                self.query_one(PromptInput).text = text
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
        self.query_one(StatusBar).start_turn_timer()
        # Tag this as a primary (conversation) turn so its accepted frame becomes
        # the Ctrl+C interrupt target — not a later control reply's frame.
        self._turns.note_send("primary")
        if self._send is not None:
            await self._send(text)

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

        # export_text is empty exactly when there is no real conversation
        # (no user turns / non-status agent replies) — a cleaner emptiness
        # check than string-matching the Markdown header, which always carries
        # the session/timestamp metadata block.
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
        elif arg:
            self._tv.add_notice("[$warning]用法: /theme [light|dark][/]")
            return
        current = "浅色" if self.theme == "echo-light" else "深色"
        self._tv.add_notice(f"当前主题: [b]{current}[/b]")

    def on_prompt_input_content_changed(
        self, message: PromptInput.ContentChanged
    ) -> None:
        self.query_one("#placeholder").display = message.is_empty
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
        if turn_active and self._interrupt is not None:
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
            if self._pending_clarify is not None:
                self._pending_clarify = None
                self._clarify_free_input = False
                self._unlock_prompt()
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
        blk.mark("approve" if decision == "approve" else "deny")
        if self._send is not None:
            cmd = (approve_command(blk.request_id, level) if decision == "approve"
                   else deny_command(blk.request_id))
            # Control send: its accepted frame and ack reply must NOT become the
            # interrupt target nor stop the original (still-parked) turn's timer.
            self._turns.note_send("control")
            await self._send(cmd)
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
        blk.mark(answer)
        if self._send is not None:
            # Control send (clarify answer): tracked separately so it doesn't
            # clobber the primary turn that is parked waiting for this answer.
            self._turns.note_send("control")
            await self._send(clarify_command(blk.clarify_id, answer))
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
