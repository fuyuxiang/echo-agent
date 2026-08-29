"""Webhook channel — HTTP API for external event ingestion."""

from __future__ import annotations

import asyncio
import hashlib
import hmac
import inspect
import json

from aiohttp import web
from loguru import logger

from echo_agent.bus.idempotency import (
    BoundedIdempotencyStore,
    CachedResponse,
    IDEMPOTENCY_FINGERPRINT_METADATA,
    IDEMPOTENCY_NAMESPACE_METADATA,
    canonical_operation_fingerprint,
    deterministic_event_id,
    durable_fingerprint_conflicts,
    normalize_idempotency_key,
)
from echo_agent.bus.events import (
    FAULTED_TURN_OUTCOMES,
    OutboundEvent,
    TERMINAL_TURN_OUTCOMES,
    final_frame_http_status,
    turn_outcome_http_status,
)
from echo_agent.bus.queue import MessageBus
from echo_agent.channels.base import BaseChannel, SendResult
from echo_agent.config.schema import WebhookChannelConfig


class WebhookChannel(BaseChannel):
    name = "webhook"
    is_realtime = False

    def __init__(self, config: WebhookChannelConfig, bus: MessageBus):
        super().__init__(config, bus)
        self._app: web.Application | None = None
        self._runner: web.AppRunner | None = None
        self._pending_responses: dict[str, asyncio.Future[CachedResponse]] = {}
        self._idempotency = BoundedIdempotencyStore(
            max_entries=2048, ttl_seconds=3600.0,
        )

    async def start(self) -> None:
        self._app = web.Application()
        self._app.router.add_post(self.config.path, self._handle_webhook)
        self._app.router.add_get("/health", self._handle_health)
        self._runner = web.AppRunner(self._app)
        await self._runner.setup()
        site = web.TCPSite(self._runner, self.config.host, self.config.port)
        await site.start()
        self._running = True
        self.bus.subscribe_outbound(self.name, self.send)
        logger.info("Webhook channel listening on {}:{}{}", self.config.host, self.config.port, self.config.path)

    async def stop(self) -> None:
        self._running = False
        if self._runner:
            await self._runner.cleanup()

    async def send(self, event: OutboundEvent) -> SendResult | None:
        if not self.should_deliver(event):
            return SendResult(success=True, skipped=True)
        is_terminal = event.is_final or event.message_kind == "final"
        if not is_terminal:
            # Heartbeats and approval prompts are delivered, but they do not
            # end the synchronous HTTP request. Only the turn's final frame may
            # consume its waiter and finalize the cached idempotency response.
            return SendResult(success=True)
        correlation_id = str(event.metadata.get("_inbound_event_id") or event.reply_to_id or "")
        final_response = self._final_response(
            correlation_id, event.text, event.metadata,
        )
        future = self._pending_responses.pop(correlation_id, None)
        if future and not future.done():
            future.set_result(final_response)
        if correlation_id:
            await self._idempotency.complete_event(
                correlation_id,
                status=final_response.status,
                payload=final_response.payload,
            )
        return SendResult(success=True)

    @staticmethod
    def _final_response(
        event_id: str, text: str, metadata: dict,
    ) -> CachedResponse:
        turn_status = str(metadata.get("_turn_status") or "")
        if turn_status not in TERMINAL_TURN_OUTCOMES:
            # Pre-contract or third-party frame: fall back to the legacy reading.
            turn_status = "failed" if metadata.get("_error") else "completed"
        if turn_status == "completed":
            return CachedResponse(
                status=200,
                payload={"response": text, "event_id": event_id},
            )
        default_reason = (
            "agent processing failed"
            if turn_status in FAULTED_TURN_OUTCOMES
            else f"turn {turn_status}"
        )
        return CachedResponse(
            status=final_frame_http_status(metadata, turn_status),
            payload={
                "status": turn_status,
                "error": str(metadata.get("_error_reason") or default_reason),
                "response": text,
                "event_id": event_id,
            },
        )

    def _verify_signature(self, body: bytes, signature: str) -> bool:
        if not self.config.secret:
            return True
        expected = hmac.new(
            self.config.secret.encode(), body, hashlib.sha256
        ).hexdigest()
        return hmac.compare_digest(expected, signature)

    async def _get_turn_run(self, event_id: str) -> dict | None:
        """Read the optional durable ledger without requiring it from embedders."""
        lookup = getattr(self.bus, "get_turn_run", None)
        if not callable(lookup):
            return None
        result = lookup(event_id)
        if inspect.isawaitable(result):
            result = await result
        return result if isinstance(result, dict) else None

    async def _release_durable_claim(self, event_id: str) -> None:
        """Best-effort release of a tombstone this request can no longer own.

        Never raises: every caller is already returning a definite failure to
        the client, and a release error must not replace that with an opaque
        500. An unreleased tombstone still expires with its TTL.
        """
        release = getattr(self.bus, "release_durable_idempotency", None)
        if not callable(release):
            return
        try:
            result = release(event_id)
            if inspect.isawaitable(result):
                await result
        except Exception as e:
            logger.error(
                "Releasing durable webhook idempotency claim {} failed: {}",
                event_id, e,
            )

    @staticmethod
    def _turn_run_response(
        row: dict, event_id: str,
    ) -> tuple[int, dict, bool]:
        status = str(row.get("status") or "accepted")
        payload = {"status": status, "event_id": event_id}
        if status not in TERMINAL_TURN_OUTCOMES:
            return 200, payload, False
        response_text = str(row.get("response_text") or "")
        if status == "completed":
            payload["response"] = response_text
            return 200, payload, True
        payload["error"] = str(row.get("error") or f"turn {status}")
        if response_text:
            payload["response"] = response_text
        # Same mapping as the live final frame, so a replay cannot answer with a
        # different status code than the original request received.
        return turn_outcome_http_status(status), payload, True

    async def _handle_webhook(self, request: web.Request) -> web.Response:
        body = await request.read()
        signature = request.headers.get("X-Signature", "")
        if not self._verify_signature(body, signature):
            return web.json_response({"error": "invalid signature"}, status=403)

        try:
            data = json.loads(body)
        except json.JSONDecodeError:
            return web.json_response({"error": "invalid json"}, status=400)
        if not isinstance(data, dict):
            return web.json_response({"error": "json body must be an object"}, status=400)

        sender_id = str(data.get("sender_id", "webhook"))
        chat_id = str(data.get("chat_id", "webhook"))
        text = data.get("text", data.get("content", ""))
        if not text:
            return web.json_response({"error": "missing text"}, status=400)

        wait = data.get("wait", False)

        try:
            header_key = normalize_idempotency_key(
                request.headers.get("Idempotency-Key")
                or request.headers.get("X-Idempotency-Key")
            )
            body_key = normalize_idempotency_key(data.get("idempotency_key"))
        except ValueError as e:
            return web.json_response({"error": str(e)}, status=400)
        if header_key and body_key and header_key != body_key:
            return web.json_response(
                {"error": "conflicting idempotency keys"}, status=400,
            )
        idempotency_key = header_key or body_key

        try:
            event = self._build_event(
                sender_id=sender_id,
                chat_id=chat_id,
                text=text,
                metadata=data.get("metadata", {}),
            )
        except PermissionError:
            return web.json_response({"error": "forbidden"}, status=403)

        claim = None
        operation_fingerprint = ""
        durable_claimed = False
        if idempotency_key:
            scope = f"{sender_id}\0{chat_id}"
            event.event_id = deterministic_event_id(
                "webhook", scope, idempotency_key,
            )
            operation_fingerprint = canonical_operation_fingerprint({
                "sender_id": event.sender_id,
                "chat_id": event.chat_id,
                "text": event.text,
                "metadata": event.metadata,
            })
            event.metadata[IDEMPOTENCY_NAMESPACE_METADATA] = "webhook"
            event.metadata[IDEMPOTENCY_FINGERPRINT_METADATA] = operation_fingerprint
            claim = await self._idempotency.claim(
                namespace="webhook",
                scope=scope,
                key=idempotency_key,
                fingerprint=operation_fingerprint,
                event_id=event.event_id,
            )
            if claim.outcome == "conflict":
                return web.json_response(
                    {"error": "idempotency key was already used for a different request"},
                    status=409,
                )
            if claim.outcome == "full":
                return web.json_response(
                    {"error": "idempotency store is full"}, status=503,
                )
            if claim.outcome == "duplicate" and claim.entry is not None:
                cached = (
                    claim.entry.response if wait else claim.entry.admission
                )
                if cached is not None:
                    return web.json_response(cached.payload, status=cached.status)
                # Do not acknowledge a racing duplicate speculatively: the first
                # handler may still fail its capacity check or inbound publish.
                try:
                    waiter = (
                        self._idempotency.wait(claim.entry, timeout=120)
                        if wait
                        else self._idempotency.wait_admitted(
                            claim.entry, timeout=120,
                        )
                    )
                    cached = await waiter
                    return web.json_response(cached.payload, status=cached.status)
                except asyncio.TimeoutError:
                    return web.json_response(
                        {"error": "timeout", "event_id": event.event_id}, status=504,
                    )

            durable_claim_fn = getattr(self.bus, "claim_durable_idempotency", None)
            try:
                if not callable(durable_claim_fn):
                    raise RuntimeError("durable idempotency storage is unavailable")
                durable_claim = durable_claim_fn(
                    event.event_id,
                    namespace="webhook",
                    fingerprint=operation_fingerprint,
                    session_key=event.session_key,
                )
                if inspect.isawaitable(durable_claim):
                    durable_claim = await durable_claim
                if not isinstance(durable_claim, dict):
                    raise RuntimeError("invalid durable idempotency claim result")
            except Exception as e:
                logger.error("Durable webhook idempotency claim failed: {}", e)
                if claim.entry is not None:
                    await self._idempotency.abort(claim.entry)
                return web.json_response(
                    {"error": "durable idempotency storage unavailable"},
                    status=503,
                )
            durable_outcome = durable_claim.get("outcome")
            durable_record = durable_claim.get("row")
            if durable_outcome == "full":
                await self._idempotency.abort(claim.entry)
                return web.json_response(
                    {"error": "durable idempotency store is full"}, status=503,
                )
            if durable_outcome == "conflict":
                await self._idempotency.abort(claim.entry)
                return web.json_response(
                    {"error": "idempotency key was already used for a different request"},
                    status=409,
                )
            if durable_outcome == "duplicate" and isinstance(durable_record, dict):
                response_status, response_payload, terminal = (
                    self._turn_run_response(durable_record, event.event_id)
                )
                if terminal:
                    await self._idempotency.complete(
                        claim.entry,
                        status=response_status,
                        payload=response_payload,
                    )
                else:
                    await self._idempotency.abort(
                        claim.entry,
                        status=response_status,
                        payload=response_payload,
                    )
                return web.json_response(response_payload, status=response_status)
            if durable_outcome != "new":
                await self._idempotency.abort(claim.entry)
                return web.json_response(
                    {"error": "durable idempotency storage unavailable"},
                    status=503,
                )
            durable_claimed = True

            try:
                durable_row = await self._get_turn_run(event.event_id)
            except Exception:
                await self._release_durable_claim(event.event_id)
                durable_claimed = False
                if claim.entry is not None:
                    await self._idempotency.abort(claim.entry)
                raise
            if durable_row is not None and claim.entry is not None:
                if durable_fingerprint_conflicts(
                    durable_row,
                    namespace="webhook",
                    fingerprint=operation_fingerprint,
                ):
                    await self._release_durable_claim(event.event_id)
                    durable_claimed = False
                    payload = {
                        "error": "idempotency key was already used for a different request",
                    }
                    await self._idempotency.abort(
                        claim.entry, status=409, payload=payload,
                    )
                    return web.json_response(payload, status=409)
                try:
                    await self.bus.sync_durable_idempotency(durable_row)
                except Exception as e:
                    logger.error("Durable webhook idempotency sync failed: {}", e)
                    # Release the tombstone we just claimed. Leaving it behind
                    # pinned this key at `pending` for the whole TTL, so every
                    # retry replayed "pending" even though the turn had already
                    # completed. The DELETE is guarded to pending/accepted rows,
                    # so it cannot erase a racing owner's terminal result.
                    await self._release_durable_claim(event.event_id)
                    durable_claimed = False
                    await self._idempotency.abort(claim.entry)
                    return web.json_response(
                        {"error": "durable idempotency storage unavailable"},
                        status=503,
                    )
                response_status, response_payload, terminal = (
                    self._turn_run_response(durable_row, event.event_id)
                )
                if terminal:
                    await self._idempotency.complete(
                        claim.entry,
                        status=response_status,
                        payload=response_payload,
                    )
                else:
                    await self._idempotency.abort(
                        claim.entry,
                        status=response_status,
                        payload=response_payload,
                    )
                return web.json_response(
                    response_payload, status=response_status,
                )

        published = False
        publish_outcome_unknown = False
        pending_event_id = ""
        try:
            future: asyncio.Future[CachedResponse] | None = None
            if wait:
                if len(self._pending_responses) >= self.config.max_pending:
                    if durable_claimed:
                        await self._release_durable_claim(event.event_id)
                        durable_claimed = False
                    if claim is not None and claim.entry is not None:
                        await self._idempotency.abort(
                            claim.entry,
                            payload={"error": "too many pending requests"},
                        )
                    return web.json_response(
                        {"error": "too many pending requests"}, status=503,
                    )
                future = asyncio.get_running_loop().create_future()
                self._pending_responses[event.event_id] = future
                pending_event_id = event.event_id

            try:
                accepted = await self.bus.publish_inbound(event)
            except asyncio.CancelledError:
                # Queue admission may already have happened when cancellation
                # is observed. Preserve the claim so a retry cannot publish a
                # second copy; a final reply or the stale-outcome TTL settles it.
                publish_outcome_unknown = True
                raise
            if not accepted:
                if durable_claimed:
                    await self._release_durable_claim(event.event_id)
                    durable_claimed = False
                if claim is not None and claim.entry is not None:
                    await self._idempotency.abort(
                        claim.entry,
                        payload={"error": "busy, try again later"},
                    )
                return web.json_response(
                    {"error": "busy, try again later"}, status=503,
                )

            published = True
            if durable_claimed:
                await asyncio.shield(
                    self.bus.mark_durable_idempotency_admitted(event.event_id)
                )
                durable_claimed = False
            admission_payload = {
                "status": "accepted", "event_id": event.event_id,
            }
            if claim is not None and claim.entry is not None:
                operation = (
                    self._idempotency.mark_admitted(
                        claim.entry, status=200, payload=admission_payload,
                    )
                    if future
                    else self._idempotency.complete(
                        claim.entry, status=200, payload=admission_payload,
                    )
                )
                await asyncio.shield(operation)

            if future:
                try:
                    result = await asyncio.wait_for(future, timeout=120)
                except asyncio.TimeoutError:
                    return web.json_response(
                        {"error": "timeout", "event_id": event.event_id},
                        status=504,
                    )
                if claim is not None and claim.entry is not None:
                    await self._idempotency.complete(
                        claim.entry, status=result.status, payload=result.payload,
                    )
                return web.json_response(result.payload, status=result.status)

            return web.json_response(admission_payload)
        except asyncio.CancelledError:
            if (
                not published
                and not publish_outcome_unknown
                and claim is not None
                and claim.entry is not None
            ):
                await asyncio.shield(self._idempotency.abort(claim.entry))
            if not published and not publish_outcome_unknown and durable_claimed:
                await asyncio.shield(
                    self._release_durable_claim(event.event_id)
                )
            raise
        except Exception:
            if not published and claim is not None and claim.entry is not None:
                await self._idempotency.abort(claim.entry)
            if not published and durable_claimed:
                await self._release_durable_claim(event.event_id)
            raise
        finally:
            if pending_event_id:
                self._pending_responses.pop(pending_event_id, None)

    async def _handle_health(self, request: web.Request) -> web.Response:
        return web.json_response({"status": "ok", "channel": self.name})
