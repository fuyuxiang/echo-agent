"""Bind interactive delivery to both its target and its conversation.

Correlation is recorded at ingress, rather than relying on producers to copy
session metadata. Unattributed notifications remain compatible when their
target has exactly one receiver; ambiguous or expired replies fail closed.
"""

from collections import OrderedDict
from collections.abc import Mapping

from echo_agent.bus.events import InboundEvent, OutboundEvent

ConnectionKey = str | tuple[str, str]


def connection_key(target: str, session_key: str) -> ConnectionKey:
    # Keep the legacy derived key for clients without an explicit session.
    return target if session_key == target else (target, session_key)


class GatewayDeliveryScopes:
    def __init__(self, max_turns: int = 8192):
        self._max_turns = max_turns
        self._turns: OrderedDict[str, tuple[str, str]] = OrderedDict()

    def remember(self, event: InboundEvent) -> None:
        self._turns[event.event_id] = (f"{event.channel}:{event.chat_id}", event.session_key)
        self._turns.move_to_end(event.event_id)
        while len(self._turns) > self._max_turns:
            self._turns.popitem(last=False)

    def resolve(self, event: OutboundEvent, clients: Mapping[ConnectionKey, object]) -> ConnectionKey | None:
        target = f"{event.channel}:{event.chat_id}"
        origin = event.delivery_origin
        correlation = str(event.metadata.get("_inbound_event_id") or event.reply_to_id or "")
        scope = self._turns.get(correlation)
        if origin is not None:
            scope = (f"{origin.channel}:{origin.chat_id}", origin.session_key)
        if scope is not None and scope[0] == target:
            # Never fall through to a different conversation when this one left.
            return connection_key(*scope)
        if correlation and scope is None:
            return None
        # An explicit tool/mirror destination can differ from the originating
        # turn. It is deliverable only when that destination is unambiguous.
        candidates = [key for key in clients if (key[0] if isinstance(key, tuple) else key) == target]
        return candidates[0] if len(candidates) == 1 else None

    def clear(self) -> None:
        self._turns.clear()
