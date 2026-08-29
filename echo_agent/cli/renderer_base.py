"""The contract between the WS dispatch layer and whichever renderer is active.

WSBridge (cli/tui/bridge.py) has always depended on exactly these methods by
duck typing; attach_client additionally drives notify_disconnected /
notify_reconnected / replay_missed_reply on the reconnect path. Writing it down
is what lets a second renderer exist: the inline session and the Textual app
are interchangeable behind this and nothing in the protocol layer needs to know
which one it is talking to.

runtime_checkable gives isinstance() checks, which verify that the methods
exist — not that their signatures match. The signatures here are the
authority; a renderer that takes different arguments will fail at call time,
not at the isinstance check.
"""

from __future__ import annotations

from typing import Protocol, runtime_checkable

from echo_agent.cli.tui.protocol import CogEvent


@runtime_checkable
class RenderSink(Protocol):
    """What a renderer must offer the dispatch layer."""

    def on_turn_accepted(self, event_id: str) -> None:
        """The gateway accepted a submitted turn under this event_id."""

    def on_user_reply_token(self, inbound_id: str, text: str) -> None:
        """A streamed reply chunk for the turn correlated by inbound_id."""

    def on_user_reply_reset(self, inbound_id: str) -> None:
        """The server retracted an optimistically streamed draft."""

    def on_user_reply_final(self, inbound_id: str, text: str) -> None:
        """An authoritative reply frame. May arrive several times per turn."""

    def on_cognitive(self, ev: CogEvent) -> None:
        """A cognitive frame: tool call, thinking, approval, clarify, cost…"""

    def on_error(self, msg: str) -> None:
        """A gateway error frame; terminal for the running turn."""

    def notify_disconnected(self) -> None:
        """The socket closed without an error frame."""

    def notify_reconnected(self) -> None:
        """A replacement socket authenticated successfully."""

    def replay_missed_reply(self, text: str, event_id: str = "") -> None:
        """A reply produced while the socket was down, recovered after reconnect."""
