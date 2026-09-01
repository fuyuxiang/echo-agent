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
        # Exact IDs admitted by the gateway but not yet registered as running.
        # A control event may overtake one on the independent bus lane. Keeping
        # admission separate from execution avoids both failure modes of a TTL
        # tombstone: expiring a legitimate long-queued stop, or retaining an
        # attacker-supplied ID which was never accepted as work.
        # A dict gives us insertion order as well as O(1) membership. Older
        # clients send an unscoped interrupt before their ``accepted`` frame can
        # arrive; if control traffic overtakes normal work, that stop must bind
        # to the oldest admitted turn for the session, not an arbitrary ID.
        self._admitted: dict[tuple[str, str], None] = {}
        self._pending_targets: set[tuple[str, str]] = set()

    def admit(self, session_key: str, inbound_event_id: str) -> None:
        if session_key and inbound_event_id:
            self._admitted[(session_key, inbound_event_id)] = None

    def discard(self, session_key: str, inbound_event_id: str) -> None:
        key = (session_key, inbound_event_id)
        self._admitted.pop(key, None)
        self._pending_targets.discard(key)

    def request(self, session_key: str, inbound_event_id: str = "") -> None:
        """Register a turn as running. Called before _process_event. A fresh
        registration starts clean unless an exact targeted interrupt overtook
        this already-admitted event. A stop for another ID can never bleed into
        the new turn."""
        pending_key = (session_key, inbound_event_id)
        interrupted = bool(inbound_event_id) and pending_key in self._pending_targets
        self._admitted.pop(pending_key, None)
        self._pending_targets.discard(pending_key)
        self._running[session_key] = _Running(
            inbound_event_id=inbound_event_id,
            interrupted=interrupted,
        )

    def interrupt(self, session_key: str, target_event_id: str = "") -> bool:
        """Flag running or already-admitted work for cooperative stop.

        Returns True when a running turn was flagged or a queued, trusted
        admission was reserved for interruption. Idempotent interruption of a
        running turn stays True; a genuinely idle session is a harmless False.

        target_event_id scopes the stop to a specific turn: if the caller knows
        which turn it meant to stop (the TUI captures it from the `accepted`
        frame), a stop frame delayed by scheduling can arrive after turn A ended
        and turn B registered — without this guard it would wrongly stop B. When
        the target does not match the running turn, the interrupt is a no-op
        (returns False). An empty target keeps the old channel-agnostic behavior
        (stop the running turn, or the oldest admitted turn when control traffic
        overtook its registration) for callers that don't track event IDs."""
        r = self._running.get(session_key)
        if r is None:
            if target_event_id:
                return self._remember_admitted_target(session_key, target_event_id)
            return self._remember_oldest_admitted(session_key)
        if target_event_id and r.inbound_event_id and target_event_id != r.inbound_event_id:
            return self._remember_admitted_target(session_key, target_event_id)
        r.interrupted = True
        return True

    def _remember_admitted_target(self, session_key: str, target_event_id: str) -> bool:
        key = (session_key, target_event_id)
        if key not in self._admitted:
            return False
        self._pending_targets.add(key)
        return True

    def _remember_oldest_admitted(self, session_key: str) -> bool:
        """Bind an old-client, unscoped stop to accepted work that is queued.

        The gateway calls :meth:`admit` before publishing normal work. Control
        has its own bus lane and can therefore arrive before AgentLoop calls
        :meth:`request`. Selecting only from this trusted admission set avoids
        leaking a stale stop into unrelated future work.
        """
        for key in self._admitted:
            if key[0] == session_key:
                self._pending_targets.add(key)
                return True
        return False

    def is_interrupted(self, session_key: str) -> bool:
        r = self._running.get(session_key)
        return bool(r and r.interrupted)

    def targets_running(self, session_key: str, target_event_id: str = "") -> bool:
        """Whether this interrupt scope names the turn running right now.

        Prompt cancellation is an immediate side effect, unlike the queued
        interrupt fence. Callers use this distinction so a delayed stop for A
        cannot deny B's approval merely because both share a session.
        """
        running = self._running.get(session_key)
        if running is None:
            return False
        return not target_event_id or running.inbound_event_id == target_event_id

    def clear(self, session_key: str) -> None:
        """Deregister a finished turn. Called in the loop's finally, so a turn
        that ends normally leaves no residue for the next one to trip over."""
        self._running.pop(session_key, None)
