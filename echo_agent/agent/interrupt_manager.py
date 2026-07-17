"""Interrupt primitive — cooperatively stop a running turn. Channel-agnostic
(mirrors ClarifyManager's per-session, Event-based design): interrupt() may be
called from any path — the CLI's Ctrl+C interrupt frame, or a future IM adapter
sending a "stop" command.

Unlike a hard task.cancel(), this sets a per-session flag that the inference
tool loop polls at iteration boundaries and then stops cleanly, so session
history, memory writes and tool side effects are never left half-applied. The
trade-off is latency: a single long-running tool call only stops at the next
checkpoint (after it returns), not mid-call."""

from __future__ import annotations

from dataclasses import dataclass


@dataclass
class _Running:
    """One in-flight turn, keyed by session_key. inbound_event_id lets a caller
    scope an interrupt to a specific turn if it ever needs to (unused today —
    interrupt() targets whatever turn is currently running for the session)."""

    inbound_event_id: str = ""
    interrupted: bool = False


class InterruptManager:
    """Per-session registry of running turns and their interrupt flags.

    A dict (not a single slot) keeps this correct if a session ever runs turns
    concurrently. request() on turn start, clear() on turn end (both from the
    agent loop under/around the session lock); interrupt()/is_interrupted() are
    called from the lock-free control path and the inference checkpoint."""

    def __init__(self) -> None:
        self._running: dict[str, _Running] = {}

    def request(self, session_key: str, inbound_event_id: str = "") -> None:
        """Register a turn as running. Called before _process_event. A fresh
        registration always starts un-interrupted, so a stale interrupt from a
        previous turn can never bleed into the next one."""
        self._running[session_key] = _Running(inbound_event_id=inbound_event_id)

    def interrupt(self, session_key: str, target_event_id: str = "") -> bool:
        """Flag the session's running turn for cooperative stop. Returns True if
        a turn was actually running (so the caller can distinguish a real
        interrupt from a no-op on an idle session). Idempotent: interrupting an
        already-interrupted turn stays True; interrupting an idle session is a
        harmless False.

        target_event_id scopes the stop to a specific turn: if the caller knows
        which turn it meant to stop (the TUI captures it from the `accepted`
        frame), a stop frame delayed by scheduling can arrive after turn A ended
        and turn B registered — without this guard it would wrongly stop B. When
        the target does not match the running turn, the interrupt is a no-op
        (returns False). An empty target keeps the old channel-agnostic behavior
        (stop whatever is running) for callers that don't track event IDs."""
        r = self._running.get(session_key)
        if r is None:
            return False
        if target_event_id and r.inbound_event_id and target_event_id != r.inbound_event_id:
            return False
        r.interrupted = True
        return True

    def is_interrupted(self, session_key: str) -> bool:
        r = self._running.get(session_key)
        return bool(r and r.interrupted)

    def clear(self, session_key: str) -> None:
        """Deregister a finished turn. Called in the loop's finally, so a turn
        that ends normally leaves no residue for the next one to trip over."""
        self._running.pop(session_key, None)
