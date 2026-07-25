"""TurnRegistry — the TUI's model of in-flight turns, keyed by event_id.

Why this exists (replaces the old single ``_active_event_id`` string):

The gateway assigns a fresh event_id to EVERY inbound frame and echoes it back
in an ``accepted`` frame — that includes the user's real conversation turns AND
control replies like ``/approve`` / ``/deny`` / ``/clarify``. A single global
"active id" was therefore overwritten by the approval reply's accepted frame and
by any second turn the user queued, so Ctrl+C interrupted the wrong turn and the
approval prompt (a redundant is_final text) prematurely ended the original turn.

This registry distinguishes two kinds of outstanding work:

* **primary** turns — real conversation turns. These are what the timer tracks
  and what Ctrl+C must interrupt. The gateway serializes them per session
  (FIFO), so the one actually running is the oldest outstanding id.
* **control** events — approve/deny/clarify acks. They get accepted frames and
  final replies too, but must never become the interrupt target and must never
  stop the primary turn's timer.

Correlation is exact because a single WS connection delivers ``accepted`` frames
strictly in send order (the server emits each one inline in its read loop before
reading the next frame). So a FIFO of the kinds we sent lines up 1:1 with the
accepted frames we receive. Interrupt frames are excluded on purpose — their
accepted carries no event_id and never reaches the sink, so they are not
recorded here.
"""

from __future__ import annotations

from collections import deque


class TurnRegistry:
    def __init__(self) -> None:
        # Kinds of sends awaiting their `accepted` frame, in send order.
        self._pending_kinds: deque[str] = deque()
        # Outstanding primary (conversation) turn ids, oldest first. The oldest
        # is the one the gateway is actually running; later ones are queued.
        self._primary: list[str] = []
        # Control event ids (approve/deny/clarify) we've seen accepted for but
        # whose ack reply may still be in flight.
        self._control: set[str] = set()
        # Count of primary sends dispatched but not yet accepted. Keeps the turn
        # considered "active" in the gap between submit and its accepted frame.
        self._pending_primary = 0
        # Set when a reconnect dropped correlation while work was outstanding.
        # "We can no longer name the running turn" and "nothing is running" are
        # different facts, and conflating them cost the user control: with no
        # target id AND no active-primary flag, Ctrl+C fell through to the exit
        # guard, so a turn still running server-side could be neither stopped nor
        # interrupted. This flag keeps the interrupt path reachable while leaving
        # the queue guard open (correlation really is gone, so blocking new
        # submits forever would be worse).
        self._server_may_be_busy = False

    def note_send(self, kind: str) -> None:
        """Record that a frame of ``kind`` ("primary" or "control") was sent, so
        the matching `accepted` frame can be classified when it returns."""
        if kind not in ("primary", "control"):
            kind = "primary"
        self._pending_kinds.append(kind)
        if kind == "primary":
            self._pending_primary += 1

    def on_accepted(self, event_id: str) -> str:
        """Classify an accepted frame against the oldest un-acked send. Returns
        the resolved kind. Defaults to "primary" if the FIFO is empty (older
        gateway / unexpected frame) so a stray accept never silently vanishes."""
        kind = self._pending_kinds.popleft() if self._pending_kinds else "primary"
        if kind == "primary":
            if self._pending_primary > 0:
                self._pending_primary -= 1
            if event_id and event_id not in self._primary:
                self._primary.append(event_id)
        else:
            if event_id:
                self._control.add(event_id)
        return kind

    def on_final(self, inbound_id: str) -> str:
        """Classify a final reply by its correlated inbound event_id. Removes the
        turn from the outstanding set. Returns "primary", "control", or
        "unknown" (a reply we can't correlate — treated as a standalone turn)."""
        if inbound_id and inbound_id in self._primary:
            self._primary.remove(inbound_id)
            return "primary"
        if inbound_id and inbound_id in self._control:
            self._control.discard(inbound_id)
            return "control"
        return "unknown"

    def on_terminal_error(self) -> None:
        """A gateway error frame carries no inbound id but is terminal for the
        running turn. Clear all outstanding primary state so the timer/interrupt
        guard don't stay armed for a turn that already died server-side."""
        self._primary.clear()
        self._pending_primary = 0
        self._pending_kinds.clear()
        self._server_may_be_busy = False

    def reset_on_reconnect(self) -> None:
        """Drop ALL in-flight correlation after a socket reconnect.

        Correlation (send → accepted → final) is only valid within a single
        connection: the gateway emits each `accepted` inline in its read loop and
        never re-sends it, and a final produced while the socket was down is
        dropped (recovered only as display text via replay_missed_reply, which
        carries no event_id). So after a reconnect none of the outstanding ids
        can ever be retired by an incoming frame — leaving them here would pin
        has_active_primary True forever and the queue-guard would block every
        subsequent submit until the process restarts. Clearing control state too
        (unlike on_terminal_error): an approve/deny/clarify ack in flight across
        the drop is equally unrecoverable. Any turn still running server-side
        will deliver its final as an uncorrelated (standalone) reply, which still
        renders correctly.

        What is NOT forgotten: that work may still be running there. Clearing the
        ids answers "can we name it?" (no), not "is it over?" (unknown). The
        distinction is recorded so Ctrl+C can still send a targetless interrupt —
        the gateway stops whatever is running — instead of falling through to the
        exit prompt while the agent works on."""
        had_work = self.has_active_primary
        self._primary.clear()
        self._control.clear()
        self._pending_primary = 0
        self._pending_kinds.clear()
        self._server_may_be_busy = self._server_may_be_busy or had_work

    def note_turn_settled(self) -> None:
        """A reply/error arrived, so the pre-reconnect turn is accounted for.

        Called on any terminal frame — including the uncorrelated standalone
        reply a pre-reconnect turn produces — so the "server may be busy" flag
        does not stay armed for the rest of the session and keep offering an
        interrupt for work that already finished."""
        self._server_may_be_busy = False

    @property
    def active_turn_id(self) -> str:
        """The event_id Ctrl+C should target: the oldest outstanding primary
        turn (the one the gateway is running). Empty when none is known yet —
        the gateway then stops whatever is running, preserving old behavior."""
        return self._primary[0] if self._primary else ""

    @property
    def has_active_primary(self) -> bool:
        """True while any primary turn is outstanding OR a primary send is still
        awaiting its accepted frame. Drives both the turn timer and the Ctrl+C
        guard so they span the whole turn, never a control event."""
        return bool(self._primary) or self._pending_primary > 0

    @property
    def may_be_running_uncorrelated(self) -> bool:
        """True when work may still be running server-side under no known id.

        Only a reconnect that dropped an outstanding turn sets this. It is
        deliberately NOT folded into has_active_primary: that property gates the
        queue guard and the turn timer, which must stay open (we can never retire
        this turn, so blocking submits on it would lock the session for good).
        This one gates the interrupt path only."""
        return self._server_may_be_busy

    @property
    def queued_count(self) -> int:
        """Number of primary turns waiting behind the running one."""
        return max(0, len(self._primary) - 1)
