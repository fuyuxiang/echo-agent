"""Webhook and Gateway retries are at-most-once within a bounded window."""

from __future__ import annotations

import asyncio
import json
from types import SimpleNamespace
from unittest.mock import AsyncMock, MagicMock

import aiohttp
import pytest

from echo_agent.agent.turn_run_store import TurnRunStore
from echo_agent.bus.idempotency import (
    BoundedIdempotencyStore,
    IDEMPOTENCY_FINGERPRINT_METADATA,
    IDEMPOTENCY_NAMESPACE_METADATA,
    canonical_operation_fingerprint,
    deterministic_event_id,
)
from echo_agent.bus.events import OutboundEvent
from echo_agent.channels.webhook import WebhookChannel
from echo_agent.storage.sqlite import SQLiteBackend


class _Request:
    def __init__(self, body: dict, *, headers: dict[str, str] | None = None) -> None:
        self._body = body
        self._raw = json.dumps(body).encode("utf-8")
        self.headers = headers or {}
        self.query = {}

    async def read(self) -> bytes:
        return self._raw

    async def json(self) -> dict:
        return self._body


class _DurableIngressStub:
    """Small storage seam used by unit tests that do not initialize SQLite."""

    def __init__(self) -> None:
        self.records: dict[str, dict] = {}
        self.turns: dict[str, dict] = {}

    async def claim_idempotency(
        self, event_id: str, *, namespace: str, fingerprint: str, session_key: str,
    ) -> dict:
        row = self.records.get(event_id)
        if row is None:
            row = {
                "event_id": event_id,
                "namespace": namespace,
                "fingerprint": fingerprint,
                "session_key": session_key,
                "status": "pending",
                "response_text": "",
                "error": "",
            }
            self.records[event_id] = row
            return {"outcome": "new", "row": dict(row)}
        if (
            row["namespace"] != namespace
            or row["fingerprint"] != fingerprint
            or row["session_key"] != session_key
        ):
            return {"outcome": "conflict", "row": dict(row)}
        return {"outcome": "duplicate", "row": dict(row)}

    async def get(self, event_id: str) -> dict | None:
        return self.turns.get(event_id)

    async def release_acceptance(self, event_id: str, session_key: str) -> bool:
        return True

    async def mark_idempotency_admitted(self, event_id: str) -> None:
        if event_id in self.records:
            self.records[event_id]["status"] = "accepted"

    async def release_idempotency(self, event_id: str) -> bool:
        row = self.records.get(event_id)
        if row is not None and row["status"] in {"pending", "accepted"}:
            self.records.pop(event_id)
            return True
        return False

    async def sync_idempotency_from_turn(self, row: dict) -> None:
        record = self.records.get(str(row.get("event_id") or ""))
        if record is not None:
            record.update({
                "status": row.get("status", "accepted"),
                "response_text": row.get("response_text", ""),
                "error": row.get("error", ""),
            })


def _response_json(response) -> dict:
    return json.loads(response.body)


@pytest.mark.asyncio
async def test_store_claim_is_atomic_and_capacity_is_bounded() -> None:
    store = BoundedIdempotencyStore(max_entries=2, ttl_seconds=60)

    claims = await asyncio.gather(*(
        store.claim(
            namespace="n", scope="s", key="same", fingerprint="body",
            event_id="event-1",
        )
        for _ in range(10)
    ))
    assert [claim.outcome for claim in claims].count("new") == 1
    assert [claim.outcome for claim in claims].count("duplicate") == 9

    second = await store.claim(
        namespace="n", scope="s", key="second", fingerprint="body",
        event_id="event-2",
    )
    full = await store.claim(
        namespace="n", scope="s", key="third", fingerprint="body",
        event_id="event-3",
    )
    assert second.outcome == "new" and full.outcome == "full"
    assert store.size == 2

    assert claims[0].entry is not None
    await store.complete(claims[0].entry, status=200, payload={"ok": True})
    admitted = await store.claim(
        namespace="n", scope="s", key="third", fingerprint="body",
        event_id="event-3",
    )
    assert admitted.outcome == "new"
    assert store.size == 2


@pytest.mark.asyncio
async def test_same_key_with_different_fingerprint_conflicts() -> None:
    store = BoundedIdempotencyStore(max_entries=2, ttl_seconds=60)
    await store.claim(
        namespace="n", scope="s", key="same", fingerprint="body-a",
        event_id="event-1",
    )
    conflict = await store.claim(
        namespace="n", scope="s", key="same", fingerprint="body-b",
        event_id="event-1",
    )
    assert conflict.outcome == "conflict"


@pytest.mark.asyncio
async def test_stale_inflight_entry_is_reclaimed_with_unknown_outcome() -> None:
    now = [0.0]
    store = BoundedIdempotencyStore(
        max_entries=1, ttl_seconds=10, clock=lambda: now[0],
    )
    first = await store.claim(
        namespace="n", scope="s", key="first", fingerprint="body",
        event_id="event-1",
    )
    assert first.entry is not None
    now[0] = 11.0
    second = await store.claim(
        namespace="n", scope="s", key="second", fingerprint="body",
        event_id="event-2",
    )
    assert second.outcome == "new" and store.size == 1
    stale = await store.wait(first.entry)
    assert stale.status == 409
    assert "outcome unknown" in stale.payload["error"]


@pytest.mark.asyncio
@pytest.mark.parametrize("complete_by_event", [False, True])
async def test_completed_entry_ttl_starts_when_response_completes(
    complete_by_event: bool,
) -> None:
    now = [0.0]
    store = BoundedIdempotencyStore(
        max_entries=2, ttl_seconds=10, clock=lambda: now[0],
    )
    first = await store.claim(
        namespace="n", scope="s", key="slow", fingerprint="body",
        event_id="event-slow",
    )
    assert first.entry is not None
    now[0] = 9.0
    if complete_by_event:
        await store.complete_event(
            "event-slow", status=200, payload={"ok": True},
        )
    else:
        await store.complete(first.entry, status=200, payload={"ok": True})

    now[0] = 11.0
    replay = await store.claim(
        namespace="n", scope="s", key="slow", fingerprint="body",
        event_id="event-slow",
    )
    assert replay.outcome == "duplicate"
    assert replay.entry is first.entry

    now[0] = 19.0
    expired = await store.claim(
        namespace="n", scope="s", key="slow", fingerprint="body",
        event_id="event-slow",
    )
    assert expired.outcome == "new"


def _webhook() -> tuple[WebhookChannel, AsyncMock]:
    config = SimpleNamespace(
        host="127.0.0.1", port=0, path="/webhook", secret="",
        allow_from=[], max_pending=10,
    )
    bus = MagicMock()
    durable = _DurableIngressStub()
    bus.publish_inbound = AsyncMock(return_value=True)
    bus.claim_durable_idempotency = durable.claim_idempotency
    bus.mark_durable_idempotency_admitted = durable.mark_idempotency_admitted
    bus.release_durable_idempotency = durable.release_idempotency
    bus.sync_durable_idempotency = durable.sync_idempotency_from_turn
    bus.get_turn_run = durable.get
    channel = WebhookChannel(config, bus)
    return channel, bus.publish_inbound


@pytest.mark.asyncio
async def test_webhook_retry_publishes_one_event_and_replays_ack() -> None:
    channel, publish = _webhook()
    body = {"sender_id": "sender", "chat_id": "chat", "text": "do it"}
    headers = {"X-Idempotency-Key": "delivery-42"}

    first = await channel._handle_webhook(_Request(body, headers=headers))
    second = await channel._handle_webhook(_Request(body, headers=headers))

    assert first.status == second.status == 200
    assert _response_json(first) == _response_json(second)
    publish.assert_awaited_once()
    event = publish.await_args.args[0]
    assert event.event_id == _response_json(first)["event_id"]
    assert event.metadata[IDEMPOTENCY_NAMESPACE_METADATA] == "webhook"
    assert event.metadata[IDEMPOTENCY_FINGERPRINT_METADATA]


@pytest.mark.asyncio
async def test_webhook_fingerprint_uses_operation_not_json_or_key_transport() -> None:
    channel, publish = _webhook()
    first = await channel._handle_webhook(_Request({
        "idempotency_key": "delivery-42",
        "sender_id": "sender",
        "chat_id": "chat",
        "text": "do it",
        "wait": False,
    }))
    second = await channel._handle_webhook(_Request(
        {
            "wait": True,
            "text": "do it",
            "chat_id": "chat",
            "sender_id": "sender",
        },
        headers={"Idempotency-Key": "delivery-42"},
    ))

    assert first.status == second.status == 200
    assert _response_json(first) == _response_json(second)
    publish.assert_awaited_once()


@pytest.mark.asyncio
async def test_webhook_rejects_key_reuse_for_different_payload() -> None:
    channel, publish = _webhook()
    headers = {"Idempotency-Key": "delivery-42"}
    await channel._handle_webhook(_Request(
        {"sender_id": "sender", "chat_id": "chat", "text": "first"},
        headers=headers,
    ))
    conflict = await channel._handle_webhook(_Request(
        {"sender_id": "sender", "chat_id": "chat", "text": "changed"},
        headers=headers,
    ))
    assert conflict.status == 409
    publish.assert_awaited_once()


@pytest.mark.asyncio
async def test_webhook_pending_capacity_rejection_releases_claim() -> None:
    channel, publish = _webhook()
    for i in range(channel.config.max_pending):
        channel._pending_responses[str(i)] = asyncio.get_running_loop().create_future()
    request = _Request(
        {"sender_id": "sender", "chat_id": "chat", "text": "work", "wait": True},
        headers={"Idempotency-Key": "capacity-key"},
    )
    response = await channel._handle_webhook(request)
    assert response.status == 503
    assert channel._idempotency.size == 0
    publish.assert_not_awaited()


@pytest.mark.asyncio
async def test_webhook_publish_exception_releases_claim() -> None:
    channel, publish = _webhook()
    publish.side_effect = RuntimeError("bus failed")
    request = _Request(
        {"sender_id": "sender", "chat_id": "chat", "text": "work"},
        headers={"Idempotency-Key": "publish-key"},
    )
    with pytest.raises(RuntimeError, match="bus failed"):
        await channel._handle_webhook(request)
    assert channel._idempotency.size == 0


@pytest.mark.asyncio
async def test_webhook_keyed_request_fails_closed_without_durable_storage() -> None:
    channel, publish = _webhook()
    channel.bus.claim_durable_idempotency = AsyncMock(
        side_effect=RuntimeError("storage offline")
    )
    response = await channel._handle_webhook(_Request(
        {"sender_id": "sender", "chat_id": "chat", "text": "work"},
        headers={"Idempotency-Key": "durable-required"},
    ))

    assert response.status == 503
    assert "durable idempotency" in _response_json(response)["error"]
    assert channel._idempotency.size == 0
    publish.assert_not_awaited()


@pytest.mark.asyncio
async def test_webhook_definite_bus_rejection_can_be_retried() -> None:
    channel, publish = _webhook()
    publish.side_effect = [False, True]
    request = lambda: _Request(  # noqa: E731 - fresh request objects are intentional
        {"sender_id": "sender", "chat_id": "chat", "text": "work"},
        headers={"Idempotency-Key": "retryable-key"},
    )

    first = await channel._handle_webhook(request())
    second = await channel._handle_webhook(request())

    assert first.status == 503
    assert second.status == 200
    assert _response_json(second)["status"] == "accepted"
    assert publish.await_count == 2


@pytest.mark.asyncio
@pytest.mark.parametrize("wait", [False, True])
@pytest.mark.parametrize("ledger_status", ["running", "completed"])
async def test_webhook_replays_durable_turn_status_without_republishing(
    tmp_path, wait: bool, ledger_status: str,
) -> None:
    backend = SQLiteBackend(tmp_path / f"webhook-{wait}-{ledger_status}.db")
    await backend.initialize()
    try:
        store = TurnRunStore(backend)
        event_id = deterministic_event_id(
            "webhook", "sender\0chat", "durable-key",
        )
        await store.accept(event_id, "webhook:chat")
        assert await store.mark_running(
            event_id, "webhook:chat", context_key="ctx", trace_id="trace",
        )
        if ledger_status == "completed":
            await store.mark_terminal(
                event_id, "completed", response_text="durable answer",
            )

        channel, publish = _webhook()
        channel.bus.get_turn_run = store.get
        response = await channel._handle_webhook(_Request(
            {
                "sender_id": "sender", "chat_id": "chat", "text": "work",
                "wait": wait,
            },
            headers={"Idempotency-Key": "durable-key"},
        ))

        assert response.status == 200
        assert _response_json(response)["status"] == ledger_status
        if ledger_status == "completed":
            assert _response_json(response)["response"] == "durable answer"
        publish.assert_not_awaited()
    finally:
        await backend.close()


@pytest.mark.asyncio
async def test_webhook_durable_fingerprint_rejects_changed_operation(
    tmp_path,
) -> None:
    backend = SQLiteBackend(tmp_path / "webhook-fingerprint.db")
    await backend.initialize()
    try:
        store = TurnRunStore(backend)
        event_id = deterministic_event_id(
            "webhook", "sender\0chat", "durable-key",
        )
        fingerprint = canonical_operation_fingerprint({
            "sender_id": "sender",
            "chat_id": "chat",
            "text": "original work",
            "metadata": {},
        })
        await store.accept(event_id, "webhook:chat", metadata={
            IDEMPOTENCY_NAMESPACE_METADATA: "webhook",
            IDEMPOTENCY_FINGERPRINT_METADATA: fingerprint,
        })
        assert await store.mark_running(
            event_id, "webhook:chat", context_key="ctx", trace_id="trace",
        )

        channel, publish = _webhook()
        channel.bus.get_turn_run = store.get
        response = await channel._handle_webhook(_Request(
            {"sender_id": "sender", "chat_id": "chat", "text": "changed work"},
            headers={"Idempotency-Key": "durable-key"},
        ))

        assert response.status == 409
        assert "different request" in _response_json(response)["error"]
        publish.assert_not_awaited()
    finally:
        await backend.close()


@pytest.mark.asyncio
async def test_webhook_racing_retry_observes_publish_rejection() -> None:
    channel, publish = _webhook()
    entered = asyncio.Event()
    release = asyncio.Event()

    async def reject_after_race(_event) -> bool:
        entered.set()
        await release.wait()
        return False

    publish.side_effect = reject_after_race
    request = lambda: _Request(  # noqa: E731 - fresh request objects are intentional
        {"sender_id": "sender", "chat_id": "chat", "text": "work"},
        headers={"Idempotency-Key": "racing-key"},
    )
    first = asyncio.create_task(channel._handle_webhook(request()))
    await entered.wait()
    duplicate = asyncio.create_task(channel._handle_webhook(request()))
    await asyncio.sleep(0)
    assert not duplicate.done()

    release.set()
    first_response, duplicate_response = await asyncio.gather(first, duplicate)
    assert first_response.status == duplicate_response.status == 503
    assert _response_json(first_response) == _response_json(duplicate_response)
    publish.assert_awaited_once()


@pytest.mark.asyncio
async def test_webhook_heartbeat_does_not_finish_waiter_or_cache() -> None:
    channel, _publish = _webhook()
    claim = await channel._idempotency.claim(
        namespace="webhook",
        scope="sender\0chat",
        key="heartbeat-key",
        fingerprint="operation",
        event_id="event-heartbeat",
    )
    assert claim.entry is not None
    waiter = asyncio.get_running_loop().create_future()
    channel._pending_responses["event-heartbeat"] = waiter

    heartbeat = OutboundEvent.text_reply(
        channel="webhook",
        chat_id="chat",
        text="still working",
        is_final=False,
        message_kind="heartbeat",
    )
    heartbeat.metadata["_inbound_event_id"] = "event-heartbeat"
    await channel.send(heartbeat)

    assert channel._pending_responses["event-heartbeat"] is waiter
    assert not waiter.done()
    assert claim.entry.response is None

    final = OutboundEvent.text_reply(
        channel="webhook", chat_id="chat", text="done",
        is_final=True, message_kind="final",
    )
    final.metadata["_inbound_event_id"] = "event-heartbeat"
    await channel.send(final)

    assert waiter.result().payload == {
        "response": "done", "event_id": "event-heartbeat",
    }
    cached = await channel._idempotency.wait(claim.entry)
    assert cached.payload == {
        "response": "done", "event_id": "event-heartbeat",
    }


@pytest.mark.asyncio
async def test_webhook_final_outcome_sets_truthful_wait_and_cache_status() -> None:
    channel, _publish = _webhook()
    claim = await channel._idempotency.claim(
        namespace="webhook",
        scope="sender\0chat",
        key="incomplete-key",
        fingerprint="operation",
        event_id="event-incomplete",
    )
    assert claim.entry is not None
    waiter = asyncio.get_running_loop().create_future()
    channel._pending_responses["event-incomplete"] = waiter
    final = OutboundEvent.text_reply(
        channel="webhook",
        chat_id="chat",
        text="partial answer",
        is_final=True,
        message_kind="final",
    )
    final.metadata.update({
        "_inbound_event_id": "event-incomplete",
        "_turn_status": "incomplete",
        "_error": True,
        "_error_reason": "budget_halted",
        "_http_status": 409,
    })

    await channel.send(final)

    result = waiter.result()
    assert result.status == 409
    assert result.payload["status"] == "incomplete"
    assert result.payload["error"] == "budget_halted"
    cached = await channel._idempotency.wait(claim.entry)
    assert cached.status == 409
    assert cached.payload == result.payload


def _gateway(tmp_path):
    from echo_agent.bus.queue import MessageBus
    from echo_agent.config.schema import (
        GatewayAuthConfig,
        GatewayConfig,
        GatewaySessionPolicyConfig,
    )
    from echo_agent.gateway.server import GatewayServer

    config = GatewayConfig(
        enabled=True,
        host="127.0.0.1",
        port=0,
        auth=GatewayAuthConfig(mode="open"),
        session_policy=GatewaySessionPolicyConfig(mode="none"),
    )
    bus = MessageBus()
    bus.set_turn_run_store(_DurableIngressStub())
    bus.publish_inbound = AsyncMock(return_value=True)
    sessions = MagicMock()
    sessions.get_or_create = AsyncMock(return_value=MagicMock(status="active"))
    gateway = GatewayServer(
        config=config,
        bus=bus,
        channel_manager=MagicMock(),
        session_manager=sessions,
        workspace=tmp_path,
        agent_loop=None,
    )
    return gateway, bus.publish_inbound


@pytest.mark.asyncio
async def test_gateway_http_retry_publishes_one_event(tmp_path) -> None:
    gateway, publish = _gateway(tmp_path)
    body = {
        "platform": "api", "user_id": "user", "chat_id": "chat",
        "text": "do it",
    }
    headers = {"Idempotency-Key": "request-99"}

    first = await gateway._handle_message(_Request(body, headers=headers))
    second = await gateway._handle_message(_Request(body, headers=headers))

    assert first.status == second.status == 200
    assert _response_json(first) == _response_json(second)
    publish.assert_awaited_once()


@pytest.mark.asyncio
async def test_gateway_keyed_request_fails_closed_without_durable_storage(
    tmp_path,
) -> None:
    gateway, publish = _gateway(tmp_path)
    gateway._bus.set_turn_run_store(None)
    response = await gateway._handle_message(_Request(
        {
            "platform": "api",
            "user_id": "user",
            "chat_id": "chat",
            "text": "work",
        },
        headers={"Idempotency-Key": "durable-required"},
    ))

    assert response.status == 503
    assert "durable idempotency" in _response_json(response)["error"]
    assert gateway._message_idempotency.size == 0
    publish.assert_not_awaited()


@pytest.mark.asyncio
async def test_gateway_fingerprint_excludes_key_wait_and_timeout_controls(
    tmp_path,
) -> None:
    gateway, publish = _gateway(tmp_path)
    first_body = {
        "idempotency_key": "request-99",
        "platform": "api",
        "user_id": "user",
        "chat_id": "chat",
        "text": "do it",
        "wait": False,
        "timeout_seconds": 1,
    }
    second_body = {
        "timeout_seconds": 600,
        "wait": True,
        "text": "do it",
        "chat_id": "chat",
        "user_id": "user",
        "platform": "api",
    }

    first = await gateway._handle_message(_Request(first_body))
    second = await gateway._handle_message(_Request(
        second_body, headers={"Idempotency-Key": "request-99"},
    ))

    assert first.status == second.status == 200
    assert _response_json(first) == _response_json(second)
    publish.assert_awaited_once()


@pytest.mark.asyncio
async def test_gateway_http_retry_does_not_consume_another_rate_limit_token(
    tmp_path,
) -> None:
    gateway, publish = _gateway(tmp_path)
    gateway.rate_limiter.acquire = MagicMock(return_value=True)
    body = {
        "platform": "api", "user_id": "user", "chat_id": "chat",
        "text": "do it",
    }
    request = lambda: _Request(  # noqa: E731 - fresh request objects are intentional
        body, headers={"Idempotency-Key": "request-99"},
    )

    first = await gateway._handle_message(request())
    second = await gateway._handle_message(request())

    assert first.status == second.status == 200
    gateway.rate_limiter.acquire.assert_called_once_with("api", "chat")
    publish.assert_awaited_once()


@pytest.mark.asyncio
async def test_gateway_wait_maps_and_replays_bus_rate_limit_as_429(tmp_path) -> None:
    gateway, publish = _gateway(tmp_path)
    deliveries: list[asyncio.Task] = []

    async def accept_then_rate_limit(event) -> bool:
        reply = OutboundEvent.text_reply(
            channel=event.channel,
            chat_id=event.chat_id,
            text="too many requests",
            is_final=True,
            message_kind="final",
        )
        reply.metadata = {
            "_inbound_event_id": event.event_id,
            "_error": True,
            "_error_reason": "rate limited",
            "_http_status": 429,
        }

        async def deliver() -> None:
            await asyncio.sleep(0)
            await gateway._handle_outbound(reply)

        deliveries.append(asyncio.create_task(deliver()))
        return True

    publish.side_effect = accept_then_rate_limit
    body = {
        "platform": "api", "user_id": "user", "chat_id": "chat",
        "text": "work", "wait": True, "timeout_seconds": 2,
    }
    headers = {"Idempotency-Key": "rate-limited-key"}

    first = await gateway._handle_message(_Request(body, headers=headers))
    second = await gateway._handle_message(_Request(body, headers=headers))
    await asyncio.gather(*deliveries)

    assert first.status == second.status == 429
    assert _response_json(first) == _response_json(second)
    assert _response_json(first)["status"] == "failed"
    assert _response_json(first)["error"] == "rate limited"
    publish.assert_awaited_once()


def test_gateway_generic_error_final_is_not_reported_completed(tmp_path) -> None:
    gateway, _publish = _gateway(tmp_path)
    status, payload = gateway._http_final_response(
        "event-id",
        "gateway:api:chat",
        {"text": "failed", "metadata": {"_error": True}},
    )
    assert status == 500
    assert payload["status"] == "failed"
    assert payload["error"] == "agent processing failed"


def test_gateway_incomplete_final_preserves_turn_status(tmp_path) -> None:
    gateway, _publish = _gateway(tmp_path)
    status, payload = gateway._http_final_response(
        "event-id",
        "gateway:api:chat",
        {
            "text": "partial",
            "metadata": {
                "_turn_status": "incomplete",
                "_error": True,
                "_error_reason": "budget_halted",
                "_http_status": 409,
            },
        },
    )
    assert status == 409
    assert payload["status"] == "incomplete"
    assert payload["error"] == "budget_halted"


@pytest.mark.asyncio
async def test_gateway_http_key_is_bound_to_request_body(tmp_path) -> None:
    gateway, publish = _gateway(tmp_path)
    headers = {"X-Idempotency-Key": "request-99"}
    base = {"platform": "api", "user_id": "user", "chat_id": "chat"}
    await gateway._handle_message(_Request({**base, "text": "first"}, headers=headers))
    conflict = await gateway._handle_message(
        _Request({**base, "text": "changed"}, headers=headers)
    )
    assert conflict.status == 409
    publish.assert_awaited_once()


@pytest.mark.asyncio
async def test_gateway_pending_capacity_rejection_releases_claim(tmp_path) -> None:
    gateway, publish = _gateway(tmp_path)
    gateway._MAX_PENDING_HTTP = 1
    gateway._pending_http["occupied"] = asyncio.get_running_loop().create_future()
    response = await gateway._handle_message(_Request(
        {
            "platform": "api", "user_id": "user", "chat_id": "chat",
            "text": "work", "wait": True,
        },
        headers={"Idempotency-Key": "capacity-key"},
    ))
    assert response.status == 503
    assert gateway._message_idempotency.size == 0
    publish.assert_not_awaited()


@pytest.mark.asyncio
@pytest.mark.parametrize("wait", [False, True])
async def test_gateway_definite_bus_rejection_releases_ledger_for_retry(
    tmp_path, wait: bool,
) -> None:
    backend = SQLiteBackend(tmp_path / f"gateway-retry-{wait}.db")
    await backend.initialize()
    deliveries: list[asyncio.Task] = []
    try:
        store = TurnRunStore(backend)
        gateway, publish = _gateway(tmp_path)
        gateway._bus.set_turn_run_store(store)
        gateway._agent_loop = SimpleNamespace(turn_runs=store)
        calls = 0

        async def reject_then_accept(event) -> bool:
            nonlocal calls
            calls += 1
            if calls == 1:
                return False
            if wait:
                reply = OutboundEvent.text_reply(
                    channel=event.channel,
                    chat_id=event.chat_id,
                    text="done",
                    is_final=True,
                    message_kind="final",
                )
                reply.metadata["_inbound_event_id"] = event.event_id

                async def deliver() -> None:
                    await asyncio.sleep(0)
                    await gateway._handle_outbound(reply)

                deliveries.append(asyncio.create_task(deliver()))
            return True

        publish.side_effect = reject_then_accept
        body = {
            "platform": "api", "user_id": "user", "chat_id": "chat",
            "text": "work", "wait": wait, "timeout_seconds": 2,
        }
        headers = {"Idempotency-Key": "retryable-key"}

        first = await gateway._handle_message(_Request(body, headers=headers))
        assert first.status == 503
        event_id = deterministic_event_id(
            "gateway-message",
            "http\0anonymous\0gateway:api:chat",
            "retryable-key",
        )
        assert await store.get(event_id) is None

        second = await gateway._handle_message(_Request(body, headers=headers))
        await asyncio.gather(*deliveries)
        assert second.status == 200
        assert _response_json(second)["status"] == (
            "completed" if wait else "accepted"
        )
        persisted = await store.get(event_id)
        assert persisted is not None
        assert (
            persisted["metadata"][IDEMPOTENCY_NAMESPACE_METADATA]
            == "gateway-message"
        )
        assert persisted["metadata"][IDEMPOTENCY_FINGERPRINT_METADATA]
        assert publish.await_count == 2
    finally:
        await backend.close()


@pytest.mark.asyncio
@pytest.mark.parametrize("wait", [False, True])
@pytest.mark.parametrize("ledger_status", ["running", "completed"])
async def test_gateway_replays_durable_turn_status_without_republishing(
    tmp_path, wait: bool, ledger_status: str,
) -> None:
    backend = SQLiteBackend(tmp_path / f"gateway-{wait}-{ledger_status}.db")
    await backend.initialize()
    try:
        store = TurnRunStore(backend)
        event_id = deterministic_event_id(
            "gateway-message",
            "http\0anonymous\0gateway:api:chat",
            "durable-key",
        )
        await store.accept(event_id, "gateway:api:chat")
        assert await store.mark_running(
            event_id, "gateway:api:chat", context_key="ctx", trace_id="trace",
        )
        if ledger_status == "completed":
            await store.mark_terminal(
                event_id, "completed", response_text="durable answer",
            )

        gateway, publish = _gateway(tmp_path)
        gateway._bus.set_turn_run_store(store)
        response = await gateway._handle_message(_Request(
            {
                "platform": "api", "user_id": "user", "chat_id": "chat",
                "text": "work", "wait": wait,
            },
            headers={"Idempotency-Key": "durable-key"},
        ))

        assert response.status == 200
        assert _response_json(response)["status"] == ledger_status
        if ledger_status == "completed":
            assert _response_json(response)["reply"]["text"] == "durable answer"
        publish.assert_not_awaited()
    finally:
        await backend.close()


@pytest.mark.asyncio
async def test_gateway_durable_fingerprint_rejects_changed_operation(
    tmp_path,
) -> None:
    backend = SQLiteBackend(tmp_path / "gateway-fingerprint.db")
    await backend.initialize()
    try:
        gateway, publish = _gateway(tmp_path)
        store = TurnRunStore(backend)
        event_id = deterministic_event_id(
            "gateway-message",
            "http\0anonymous\0gateway:api:chat",
            "durable-key",
        )
        fingerprint = gateway._idempotency_fingerprint({
            "platform": "api",
            "user_id": "user",
            "chat_id": "chat",
            "text": "original work",
            "media_urls": [],
            "is_group": False,
            "session_key": "gateway:api:chat",
        })
        await store.accept(event_id, "gateway:api:chat", metadata={
            IDEMPOTENCY_NAMESPACE_METADATA: "gateway-message",
            IDEMPOTENCY_FINGERPRINT_METADATA: fingerprint,
        })
        assert await store.mark_running(
            event_id,
            "gateway:api:chat",
            context_key="ctx",
            trace_id="trace",
        )
        gateway._bus.set_turn_run_store(store)

        response = await gateway._handle_message(_Request(
            {
                "platform": "api",
                "user_id": "user",
                "chat_id": "chat",
                "text": "changed work",
            },
            headers={"Idempotency-Key": "durable-key"},
        ))

        assert response.status == 409
        assert "different request" in _response_json(response)["error"]
        publish.assert_not_awaited()
    finally:
        await backend.close()


@pytest.mark.asyncio
async def test_gateway_reset_failure_releases_claim(tmp_path) -> None:
    gateway, publish = _gateway(tmp_path)
    gateway._reset_session_if_needed = AsyncMock(
        side_effect=RuntimeError("reset failed")
    )
    request = _Request(
        {
            "platform": "api", "user_id": "user", "chat_id": "chat",
            "text": "work",
        },
        headers={"Idempotency-Key": "reset-key"},
    )

    with pytest.raises(RuntimeError, match="reset failed"):
        await gateway._handle_message(request)
    assert gateway._message_idempotency.size == 0
    publish.assert_not_awaited()


@pytest.mark.asyncio
async def test_gateway_publish_failure_preserves_unknown_claim_but_not_waiter(
    tmp_path,
) -> None:
    gateway, publish = _gateway(tmp_path)
    publish.side_effect = RuntimeError("publish failed")
    request = _Request(
        {
            "platform": "api", "user_id": "user", "chat_id": "chat",
            "text": "work", "wait": True,
        },
        headers={"Idempotency-Key": "publish-key"},
    )

    with pytest.raises(RuntimeError, match="publish failed"):
        await gateway._handle_message(request)
    # Once publish_inbound has been invoked, an exception does not prove the
    # event stayed out of the queue. Preserve the key as outcome-unknown; the
    # deterministic event ID and durable turn claim prevent a second execution.
    assert gateway._message_idempotency.size == 1
    assert gateway._pending_http == {}


@pytest.mark.asyncio
async def test_gateway_cancelled_publish_preserves_unknown_claim_but_not_waiter(
    tmp_path,
) -> None:
    gateway, publish = _gateway(tmp_path)
    publish.side_effect = asyncio.CancelledError
    request = _Request(
        {
            "platform": "api", "user_id": "user", "chat_id": "chat",
            "text": "work", "wait": True,
        },
        headers={"Idempotency-Key": "uncertain-key"},
    )

    with pytest.raises(asyncio.CancelledError):
        await gateway._handle_message(request)
    assert gateway._message_idempotency.size == 1
    assert gateway._pending_http == {}


@pytest.mark.asyncio
async def test_gateway_final_settles_unknown_nonwait_http_claim(tmp_path) -> None:
    gateway, _publish = _gateway(tmp_path)
    claim = await gateway._message_idempotency.claim(
        namespace="gateway-message",
        scope="http-scope",
        key="uncertain-key",
        fingerprint="request-body",
        event_id="uncertain-event",
        context={
            "transport": "http",
            "wait": False,
            "session_key": "gateway:api:chat",
        },
    )
    assert claim.entry is not None
    gateway.broadcast_to_ws = AsyncMock(return_value=False)

    await gateway._handle_outbound(OutboundEvent.text_reply(
        channel="gateway:api",
        chat_id="chat",
        text="finished despite disconnect",
        metadata={"_inbound_event_id": "uncertain-event"},
    ))

    settled = await gateway._message_idempotency.wait_admitted(claim.entry)
    assert settled.status == 200
    assert settled.payload["status"] == "completed"
    assert settled.payload["reply"]["text"] == "finished despite disconnect"


async def _ws_auth(ws) -> None:
    await ws.send_json({
        "type": "auth",
        "platform": "api",
        "user_id": "user",
        "chat_id": "chat",
    })
    assert (await asyncio.wait_for(ws.receive_json(), timeout=2))["type"] == "auth_ok"


@pytest.mark.asyncio
@pytest.mark.parametrize("ledger_status", ["running", "completed"])
async def test_gateway_ws_replays_durable_turn_without_republishing(
    tmp_path, ledger_status: str,
) -> None:
    backend = SQLiteBackend(tmp_path / f"gateway-ws-{ledger_status}.db")
    await backend.initialize()
    gateway, publish = _gateway(tmp_path)
    try:
        store = TurnRunStore(backend)
        event_id = deterministic_event_id(
            "gateway-message",
            "ws\0anonymous\0gateway:api:chat",
            "durable-ws-key",
        )
        fingerprint = gateway._idempotency_fingerprint({
            "text": "work",
            "is_group": False,
            "platform": "api",
            "user_id": "user",
            "chat_id": "chat",
        })
        await store.accept(event_id, "gateway:api:chat", metadata={
            IDEMPOTENCY_NAMESPACE_METADATA: "gateway-message",
            IDEMPOTENCY_FINGERPRINT_METADATA: fingerprint,
        })
        assert await store.mark_running(
            event_id,
            "gateway:api:chat",
            context_key="ctx",
            trace_id="trace",
        )
        if ledger_status == "completed":
            await store.mark_terminal(
                event_id, "completed", response_text="durable answer",
            )
        gateway._bus.set_turn_run_store(store)
        await gateway.start()

        async with aiohttp.ClientSession() as client:
            async with client.ws_connect(
                f"ws://127.0.0.1:{gateway.actual_port}/ws"
            ) as ws:
                await _ws_auth(ws)
                # Authentication emits a control event through the same bus.
                # Measure only whether the durable message itself is replayed.
                publish.reset_mock()
                await ws.send_json({
                    "type": "message",
                    "text": "work",
                    "idempotency_key": "durable-ws-key",
                })
                response = await asyncio.wait_for(ws.receive_json(), timeout=2)
                publish.assert_not_awaited()

        if ledger_status == "completed":
            assert response["type"] == "message"
            assert response["text"] == "durable answer"
        else:
            assert response["type"] == "accepted"
            assert response["status"] == "running"
    finally:
        await gateway.stop()
        await backend.close()


@pytest.mark.asyncio
async def test_gateway_ws_definite_rejection_releases_ledger_for_retry(
    tmp_path,
) -> None:
    backend = SQLiteBackend(tmp_path / "gateway-ws-retry.db")
    await backend.initialize()
    gateway, publish = _gateway(tmp_path)
    try:
        store = TurnRunStore(backend)
        gateway._bus.set_turn_run_store(store)
        gateway._agent_loop = SimpleNamespace(turn_runs=store)
        calls = 0

        async def reject_then_accept(event) -> bool:
            nonlocal calls
            if event.is_control:
                return True
            calls += 1
            return calls > 1

        publish.side_effect = reject_then_accept
        await gateway.start()
        event_id = deterministic_event_id(
            "gateway-message",
            "ws\0anonymous\0gateway:api:chat",
            "retryable-ws-key",
        )

        async with aiohttp.ClientSession() as client:
            async with client.ws_connect(
                f"ws://127.0.0.1:{gateway.actual_port}/ws"
            ) as ws:
                await _ws_auth(ws)
                request = {
                    "type": "message",
                    "text": "work",
                    "idempotency_key": "retryable-ws-key",
                }
                await ws.send_json(request)
                first = await asyncio.wait_for(ws.receive_json(), timeout=2)
                assert first == {"type": "error", "error": "server overloaded"}
                assert await store.get(event_id) is None

                await ws.send_json(request)
                second = await asyncio.wait_for(ws.receive_json(), timeout=2)
                assert second["type"] == "accepted"
                assert second["event_id"] == event_id
                assert (await store.get(event_id))["status"] == "accepted"
                assert calls == 2
    finally:
        await gateway.stop()
        await backend.close()
