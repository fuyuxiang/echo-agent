"""EchoTUI — the Textual root app. Serves as the WSBridge sink and owns
keybindings; upstream sends go through the injected send_coro."""

from __future__ import annotations

from textual.app import App, ComposeResult
from textual.binding import Binding

from echo_agent.cli.tui.transcript import TranscriptView
from echo_agent.cli.tui.prompt_input import PromptInput
from echo_agent.cli.tui.status_bar import StatusBar
from echo_agent.cli.tui.blocks import ApprovalBlock
from echo_agent.cli.tui.protocol import CogEvent, approve_command, deny_command


class EchoTUI(App):
    BINDINGS = [
        Binding("ctrl+r", "toggle_memory", "记忆", show=False),
        Binding("ctrl+o", "toggle_thinking", "思考", show=False),
        Binding("ctrl+c", "quit", "退出", show=False),
        Binding("ctrl+d", "quit", "退出", show=False),
        # y/n/a are declared as bindings (not on_key) because a focused
        # PromptInput (TextArea) consumes printable keys before on_key runs in
        # textual 8.2.8. check_action gates them so they only fire while an
        # approval is pending; when pending we also blur focus so the App-level
        # binding is not filtered out by TextArea.check_consume_key.
        Binding("y", "approve", "批准", show=False),
        Binding("n", "deny", "拒绝", show=False),
        Binding("a", "approve_always", "始终允许", show=False),
    ]

    def __init__(self, send_coro=None) -> None:
        super().__init__()
        self._send = send_coro
        self._replies: dict[str, object] = {}
        # A single pending-approval slot is sufficient (no queue needed):
        # approval requests are serialized server-side by inference_stage Phase A
        # — that check is a serial for-loop where cli blocks in wait_for_decision
        # until this decision resolves, so at most one approval is ever
        # outstanding. Phase B runs concurrently but only for read-only,
        # non-conflicting tools that never raise an approval_request.
        self._pending_approval: ApprovalBlock | None = None

    def compose(self) -> ComposeResult:
        yield TranscriptView()
        yield PromptInput()
        yield StatusBar()

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
        return True

    @property
    def _tv(self) -> TranscriptView:
        return self.query_one(TranscriptView)

    # --- WSBridge sink ---
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
        r.set_final(text)

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
        if ev.cog_type == "cost_update":
            # Cost only refreshes the status bar; it must not enter the
            # transcript stream (symmetric with heartbeat/approval_request
            # above), otherwise tool-heavy turns spam 💰 blocks.
            self.query_one(StatusBar).set_cost(ev.data.get("total_cost", 0.0))
            return
        self._tv.add_cognitive(ev)

    def on_error(self, msg: str) -> None:
        self.query_one(StatusBar).set_connection(False)

    def notify_disconnected(self) -> None:
        """Flip the status bar to the disconnected state after a silent ws
        close (gateway drops the socket with no error frame). Called by pump()
        when its async-for over the socket ends. Defensive: the app may not be
        fully mounted yet if the socket dies during startup."""
        try:
            self.query_one(StatusBar).set_connection(False)
        except Exception:
            pass

    # --- input ---
    async def on_prompt_input_submitted(
        self, message: PromptInput.Submitted
    ) -> None:
        self._tv.add_user(message.text)
        if self._send is not None:
            await self._send(message.text)

    # --- keybindings ---
    def action_toggle_memory(self) -> None:
        b = self._tv.last_memory_block()
        if b is not None:
            b.toggle()

    def action_toggle_thinking(self) -> None:
        b = self._tv.last_thinking_block()
        if b is not None:
            b.toggle()

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
