"""Routes raw WS payloads to TUI sink actions. Pure dispatch (no screen
access) so routing is unit-testable with a fake sink."""

from __future__ import annotations

from echo_agent.cli.tui.protocol import parse_cog_frame, CogDedup


class WSBridge:
    def __init__(self, sink) -> None:
        self._sink = sink
        self._dedup = CogDedup()

    def dispatch(self, payload: dict) -> None:
        mtype = payload.get("type")
        if mtype == "error":
            self._sink.on_error(payload.get("error") or "")
            return
        if mtype in ("accepted", "auth_ok", "pong"):
            return

        ev = parse_cog_frame(payload)
        if ev is not None:
            if not self._dedup.seen(ev.cog_event_id):
                self._sink.on_cognitive(ev)
            return

        # gateway:cli sessions also receive flat progress/tool/heartbeat frames
        # (is_final=False, no _token_stream). These are NOT reply text — the
        # cognitive heartbeat already went through parse_cog_frame above — so
        # ignore them here, otherwise on_user_reply_final would pop/overwrite an
        # in-flight streaming reply.
        if payload.get("message_kind") in ("progress", "tool", "heartbeat"):
            return

        # Plain outbound text (streaming or final)
        meta = payload.get("metadata") or {}
        inbound_id = str(meta.get("_inbound_event_id", ""))
        text = payload.get("text") or ""
        streaming = bool(meta.get("_token_stream"))
        is_final = payload.get("is_final", True) or payload.get("message_kind") == "final"
        if streaming and not is_final:
            self._sink.on_user_reply_token(inbound_id, text)
        else:
            self._sink.on_user_reply_final(inbound_id, text)
