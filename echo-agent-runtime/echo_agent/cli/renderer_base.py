"""The contract between the WS dispatch layer and whichever renderer is active.

WSBridge (cli/tui/bridge.py) and attach_client depend on exactly these methods.
The Protocol is the static-checking surface; ``render_sink_implementation`` is
the executable half of the contract and rejects signature drift when a built-in
renderer class is defined, rather than waiting for a rare frame to call it.
"""

from __future__ import annotations

import inspect
from typing import Protocol, TypeVar, runtime_checkable

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

    def on_tool_delivery(self, inbound_id: str, delivery_id: str, text: str) -> None:
        """Visible tool output correlated to a turn, but never its terminal."""

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


_RENDER_SINK_METHODS = tuple(
    name
    for name, member in RenderSink.__dict__.items()
    if not name.startswith("_") and callable(member)
)

_SinkType = TypeVar("_SinkType", bound=type)


def render_sink_implementation(cls: _SinkType) -> _SinkType:
    """Validate a concrete renderer's synchronous callback signatures.

    ``runtime_checkable`` Protocols only check attribute presence.  This class
    decorator additionally pins parameter names, arity, defaults, and the fact
    that bridge callbacks are synchronous.  A drift therefore fails at import
    time with the offending method named explicitly.
    """
    errors: list[str] = []
    for method_name in _RENDER_SINK_METHODS:
        method = getattr(cls, method_name, None)
        if not callable(method):
            errors.append(f"missing {method_name}")
            continue
        if inspect.iscoroutinefunction(method):
            errors.append(f"{method_name} must be synchronous")
            continue
        parameters = list(inspect.signature(method).parameters.values())
        if not parameters or parameters[0].name != "self":
            errors.append(f"{method_name} must be an instance method")
            continue
        actual = parameters[1:]
        protocol_method = getattr(RenderSink, method_name)
        expected = list(inspect.signature(protocol_method).parameters.values())[1:]
        if len(actual) != len(expected):
            errors.append(
                f"{method_name} expects {len(expected)} argument(s), "
                f"found {len(actual)}"
            )
            continue
        for parameter, expected_parameter in zip(actual, expected):
            if parameter.kind not in {
                inspect.Parameter.POSITIONAL_ONLY,
                inspect.Parameter.POSITIONAL_OR_KEYWORD,
            }:
                errors.append(f"{method_name}.{parameter.name} must accept positional calls")
            if parameter.name != expected_parameter.name:
                errors.append(
                    f"{method_name} argument must be {expected_parameter.name!r}, "
                    f"found {parameter.name!r}"
                )
            if expected_parameter.default is inspect.Parameter.empty:
                if parameter.default is not inspect.Parameter.empty:
                    errors.append(f"{method_name}.{parameter.name} must be required")
            elif parameter.default != expected_parameter.default:
                errors.append(
                    f"{method_name}.{parameter.name} must default to "
                    f"{expected_parameter.default!r}"
                )
    if errors:
        joined = "; ".join(errors)
        raise TypeError(f"{cls.__name__} violates RenderSink: {joined}")
    return cls
