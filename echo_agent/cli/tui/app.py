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

    def __init__(self, send_coro=None, session_key: str = "", interrupt_coro=None) -> None:
        super().__init__()
        self._send = send_coro
        # Sends a control-only interrupt frame ({"type":"interrupt"}) upstream so
        # the gateway can cooperatively stop the running turn. Distinct from
        # _send (ordinary messages) so an interrupt never becomes a chat turn.
        self._interrupt = interrupt_coro
        self._session_key = session_key
        # Brand strings (name/prompt/welcome/goodbye) are configurable via
        # ECHO_BRAND_* so a white-label deployment can rebrand without code edits.
        self._brand = load_brand()
        self._replies: dict[str, object] = {}
        # event_id of the turn currently in flight, captured from its `accepted`
        # frame. Ctrl+C scopes its interrupt to this ID so a stop frame delayed
        # past the turn's end can't clip the next turn. Cleared when the turn
        # ends (final reply or terminal error).
        self._active_event_id: str = ""
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

    # Seconds within which a second Ctrl+C confirms exit (matches the common
    # 2s window used by shells and other agent CLIs).
    CTRL_C_EXIT_WINDOW = 2.0

    def compose(self) -> ComposeResult:
        yield TranscriptView()
        panel = OptionList(id="slash_panel")
        panel.display = False
        yield panel
        with Horizontal(id="input_row"):
            yield Static(self._brand.prompt, id="prompt_sigil")
            yield PromptInput()
        yield Static("输入消息…", id="placeholder")
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
        """Record the in-flight turn's event_id (from its `accepted` frame) so a
        Ctrl+C interrupt can target exactly this turn."""
        self._active_event_id = event_id

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
        self._active_event_id = ""
        self.query_one(StatusBar).stop_turn_timer()

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
            # Blur the prompt so App-level y/n/a bindings are not filtered out
            # by the focused TextArea (textual 8.2.8 check_consume_key).
            self.set_focus(None)
            return
        if ev.cog_type == "clarify_request":
            d = ev.data
            self._pending_clarify = self._tv.add_clarify(
                d.get("clarify_id", ""), d.get("question", ""),
                d.get("options", []) or [],
            )
            self._clarify_free_input = False
            # Blur the prompt so App-level number/arrow bindings win over the
            # focused TextArea (textual 8.2.8 check_consume_key).
            self.set_focus(None)
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
        self._active_event_id = ""
        # A gateway error frame is terminal for the turn: the request was
        # rejected, so no reply will land to clear the active flag. End the turn
        # now, otherwise the Ctrl+C guard would keep trying to interrupt a turn
        # that already died server-side.
        try:
            self.query_one(StatusBar).stop_turn_timer()
        except Exception:
            pass

    def notify_disconnected(self) -> None:
        """Flip the status bar to the disconnected state after a silent ws
        close (gateway drops the socket with no error frame). Called by pump()
        when its async-for over the socket ends. Defensive: the app may not be
        fully mounted yet if the socket dies during startup."""
        try:
            self.query_one(StatusBar).set_connection(False)
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

    def _enter_clarify_free_input(self, char: str) -> None:
        # Seed a free-text clarify answer: focus the prompt, mark the next
        # submit as a clarify answer, and insert the first character. Shared by
        # on_key (printable non-digit) and the out-of-range digit fallback in
        # action_clarify_pick.
        pi = self.query_one(PromptInput)
        self._clarify_free_input = True
        pi.focus()
        pi.insert(char)

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
        if text == "/theme" or text.startswith("/theme "):
            self._do_theme(text[len("/theme"):].strip())
            return
        self._tv.add_user(text)
        self.query_one(StatusBar).start_turn_timer()
        if self._send is not None:
            await self._send(text)

    def _do_copy(self, whole: bool) -> None:
        """Copy the last reply (default) or the whole transcript (/copy all) to
        the system clipboard via OSC 52. Terminal support varies (works in
        iTerm2/WezTerm/kitty; macOS Terminal.app does not), so we notify with
        the copied length rather than silently succeeding."""
        text = self._tv.export_text() if whole else self._tv.last_reply_text()
        if not text:
            self.notify("暂无可复制的内容", severity="warning", timeout=3)
            return
        self.copy_to_clipboard(text)
        scope = "整段对话" if whole else "最近回复"
        self.notify(f"已复制{scope}（{len(text)} 字）到剪贴板", timeout=3)

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
        try:
            turn_active = self.query_one(StatusBar).is_turn_active
        except Exception:
            turn_active = False
        if turn_active and self._interrupt is not None:
            self._last_ctrl_c = 0.0
            # Scope the stop to the in-flight turn so a delayed frame can't clip
            # the next one. Empty (no accepted frame seen yet) → server stops
            # whatever is running, preserving the old behavior.
            await self._interrupt(self._active_event_id)
            # A stop while parked on a clarify also cancels it server-side
            # (loop._handle_interrupt calls clarify.cancel_session). Drop the
            # TUI's pending clarify too, or the next thing the user types would
            # be sent as an answer to a clarify that no longer exists.
            if self._pending_clarify is not None:
                self._pending_clarify = None
                self._clarify_free_input = False
                try:
                    self.query_one(PromptInput).focus()
                except Exception:
                    pass
            self.notify("已请求停止当前任务…", severity="warning", timeout=3)
            return

        # 4. Two-press exit guard.
        now = time.monotonic()
        if now - self._last_ctrl_c < self.CTRL_C_EXIT_WINDOW:
            self.exit()
            return
        self._last_ctrl_c = now
        try:
            active = self.query_one(StatusBar).is_turn_active
        except Exception:
            active = False
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
            await self._send(cmd)
        self._pending_approval = None
        # Return focus to the prompt for the next turn.
        try:
            self.query_one(PromptInput).focus()
        except Exception:
            pass

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
            await self._send(clarify_command(blk.clarify_id, answer))
        self._pending_clarify = None
        self._clarify_free_input = False
        try:
            self.query_one(PromptInput).focus()
        except Exception:
            pass

    async def action_clarify_pick(self, number: int) -> None:
        blk = self._pending_clarify
        if blk is None:
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
        opt = blk.highlighted_option()
        if opt is not None:
            await self._answer_clarify(opt)
