"""Routes raw WS payloads to TUI sink actions. Pure dispatch (no screen
access) so routing is unit-testable with a fake sink."""

from __future__ import annotations

from echo_agent.cli.renderer_base import RenderSink
from echo_agent.cli.tui.protocol import parse_cog_frame, CogDedup


class WSBridge:
    def __init__(self, sink: RenderSink) -> None:
        self._sink = sink
        self._dedup = CogDedup()
        self._delivery_dedup = CogDedup()

    def dispatch(self, payload: dict) -> None:
        mtype = payload.get("type")
        if mtype == "error":
            self._sink.on_error(payload.get("error") or "")
            return
        if mtype == "accepted":
            # Capture the accepted turn's event_id so a later Ctrl+C can scope
            # its interrupt to THIS turn (the gateway matches it), preventing a
            # delayed stop frame from clipping the next turn. event_id is absent
            # on the interrupt-ACK itself and on older gateways — the sink treats
            # empty as "no specific target".
            eid = payload.get("event_id")
            if eid:
                self._sink.on_turn_accepted(str(eid))
            return
        if mtype in ("auth_ok", "pong"):
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
        # The approval prompt is published as a redundant is_final text reply
        # (metadata._approval_request) purely for IM channels — the interactive
        # ApprovalBlock is already rendered from the cognitive approval_request
        # frame above. If we let this text through, on_user_reply_final would (a)
        # render a duplicate reply and (b) prematurely end the ORIGINAL turn,
        # which is still parked server-side in wait_for_decision. Skip it: the TUI
        # shows the ApprovalBlock, not this text.
        if meta.get("_approval_request"):
            return
        inbound_id = str(meta.get("_inbound_event_id", ""))
        artifact_part = meta.get("_artifact_part")
        artifact_delivery_id = str(meta.get("_artifact_delivery_id") or "")
        if meta.get("_tool_delivery"):
            delivery_id = ""
            if artifact_delivery_id and isinstance(artifact_part, int) and artifact_part > 0:
                delivery_id = (
                    f"{inbound_id}:artifact:{artifact_delivery_id}:{artifact_part}"
                )
            else:
                delivery_event_id = str(payload.get("event_id") or "")
                if delivery_event_id:
                    delivery_id = f"{inbound_id}:tool-delivery:{delivery_event_id}"
            if self._delivery_dedup.seen(delivery_id):
                return
            self._sink.on_tool_delivery(
                inbound_id,
                delivery_id,
                str(payload.get("text") or ""),
            )
            return
        if artifact_delivery_id and isinstance(artifact_part, int) and artifact_part > 0:
            # One artifact is intentionally delivered as several authoritative
            # text frames on clients without attachment support.  A renderer's
            # ordinary final-frame dedup uses inbound_id and would otherwise
            # discard/replace every part after the first.  Keep turn correlation
            # in metadata on the wire, but give each displayed part a stable,
            # retry-idempotent identity locally.
            inbound_id = f"{inbound_id}:artifact:{artifact_delivery_id}:{artifact_part}"
        text = payload.get("text") or ""
        streaming = bool(meta.get("_token_stream"))
        is_final = payload.get("is_final", True) or payload.get("message_kind") == "final"
        if streaming and not is_final:
            # An optimistically-streamed draft turned out to be a pre-tool
            # preamble and was retracted server-side. Tokens accumulate in one
            # reply widget, so without acting on this the next iteration's text
            # would be appended to the abandoned draft and the user would watch a
            # spliced answer until the final frame replaced it.
            if meta.get("_stream_reset"):
                self._sink.on_user_reply_reset(inbound_id)
                return
            self._sink.on_user_reply_token(inbound_id, text)
        else:
            self._sink.on_user_reply_final(inbound_id, text)
