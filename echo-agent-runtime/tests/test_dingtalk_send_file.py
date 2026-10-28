"""Exercise send_file → bus → DingTalk with real HTTP and multipart encoding."""

from __future__ import annotations

import json
import asyncio
import re
from unittest.mock import AsyncMock, MagicMock

import aiohttp
from aiohttp import web
import pytest
import pytest_asyncio

from echo_agent.agent.tools.send_file import SendFileTool
from echo_agent.bus.events import ContentBlock, ContentType, OutboundEvent
from echo_agent.bus.queue import MessageBus
from echo_agent.channels import dingtalk
from echo_agent.config.schema import DingTalkChannelConfig


@pytest_asyncio.fixture
async def transport(monkeypatch, tmp_path):
    """Replace only DingTalk's remote endpoints; retain the real channel and bus."""
    calls = []
    replies = {
        "token": {"errcode": 0, "access_token": "test-token", "expires_in": 7200},
        "upload": {"errcode": 0, "media_id": "@test-media"},
        "send": {"processQueryKey": "test-receipt"},
        "gateway": {"endpoint": "wss://stream.example", "ticket": "test-ticket"},
    }
    statuses = {"token": 200, "upload": 200, "send": 200, "gateway": 200}

    async def handle(request):
        action = "token" if request.path == "/gettoken" else "send"
        record = {"path": request.path}
        if request.path == "/media/upload":
            action = "upload"
            reader = await request.multipart()
            fields = {}
            while part := await reader.next():
                fields[part.name] = bytes(await part.read())
                if part.name == "media":
                    record["filename"] = part.filename
            record["fields"] = fields
            record["query"] = dict(request.query)
        elif action == "send":
            if request.path == "/v1.0/gateway/connections/open":
                action = "gateway"
            record["payload"] = await request.json()
            record["body_bytes"] = len(await request.read())
            record["token"] = request.headers.get("x-acs-dingtalk-access-token")
        calls.append(record)
        reply = replies[action]
        if isinstance(reply, list):
            status, reply = reply.pop(0)
            if isinstance(reply, str):
                return web.Response(text=reply, status=status)
            return web.json_response(reply, status=status)
        if isinstance(reply, str):
            return web.Response(text=reply, status=statuses[action])
        return web.json_response(reply, status=statuses[action])

    app = web.Application(client_max_size=21 * 1024 * 1024)
    app.router.add_route("*", "/{path:.*}", handle)
    runner = web.AppRunner(app)
    await runner.setup()
    site = web.TCPSite(runner, "127.0.0.1", 0)
    await site.start()
    base = f"http://127.0.0.1:{site._server.sockets[0].getsockname()[1]}"
    monkeypatch.setattr(dingtalk, "_API_BASE", base)
    monkeypatch.setattr(dingtalk, "_OAPI_BASE", base)
    bus = MessageBus()
    from echo_agent.agent.turn_run_store import TurnRunStore
    from echo_agent.storage.sqlite import SQLiteBackend
    backend = SQLiteBackend(tmp_path / "delivery.db")
    await backend.initialize()
    bus.set_turn_run_store(TurnRunStore(backend))
    monkeypatch.setattr(dingtalk, "_TEXT_SEND_INTERVAL", 0, raising=False)
    channel = dingtalk.DingTalkChannel(DingTalkChannelConfig(
        app_key="test-key", app_secret="test-secret", robot_code="test-robot",
    ), bus)
    async with aiohttp.ClientSession() as session:
        channel._session = session
        bus.subscribe_outbound(channel.name, channel.send)
        try:
            yield channel, bus, calls, replies, statuses
        finally:
            await runner.cleanup()
            await backend.close()


@pytest.mark.asyncio
@pytest.mark.parametrize("chat_id", ["staff123", "cidTest=="])
@pytest.mark.parametrize("body", ["A" * 32054, ('中文🙂\\\"\n' * 4000)], ids=["ascii", "unicode-escaped"])
async def test_long_reply_preserves_content_and_bounds_encoded_template(transport, chat_id, body):
    channel, bus, calls, _, _ = transport
    event = OutboundEvent.text_reply(channel="dingtalk", chat_id=chat_id, text=body)
    receipt = await bus.publish_outbound(event)
    sends = [call["payload"] for call in calls if "payload" in call]
    assert len(sends) > 1
    chunks = [json.loads(payload["msgParam"])["content"] for payload in sends]
    assert all(len(payload["msgParam"].encode()) <= 15000 for payload in sends)
    assert all(len(json.dumps(payload["msgParam"], ensure_ascii=False).encode()) <= 15000 for payload in sends)
    assert all(call["body_bytes"] <= 16000 for call in calls if "payload" in call)
    assert ''.join(re.sub(r'^【第\d+/\d+段】\n', '', chunk) for chunk in chunks) == body
    assert all(chunk.startswith(f'【第{index}/{len(chunks)}段】\n') for index, chunk in enumerate(chunks, 1))
    assert receipt.stage.value == "delivered"
    assert len(receipt.detail["message_ids"]) == len(chunks)


@pytest.mark.asyncio
async def test_partial_rejection_resumes_unsent_parts_after_channel_restart(transport):
    channel, bus, calls, replies, _ = transport
    event = OutboundEvent.text_reply(channel="dingtalk", chat_id="staff123", text="A" * 32054)
    replies["send"] = [(200, {"processQueryKey": "first"}),
                       (429, {"code": "Throttling", "message": "retry later"})]
    failed = await bus.publish_outbound(event)
    assert failed.stage.value == "failed"
    assert failed.detail["message_ids"] == ["first"]
    assert len([call for call in calls if "payload" in call]) == 2
    restarted = dingtalk.DingTalkChannel(channel.config, bus)
    restarted._session = channel._session
    replies["send"] = [(200, {"processQueryKey": "second"}), (200, {"processQueryKey": "third"})]
    result = await restarted.send(event)
    assert result.success
    assert result.message_ids == ["first", "second", "third"]
    sends = [json.loads(call["payload"]["msgParam"])["content"] for call in calls if "payload" in call]
    assert sends[1] == sends[2] and sends[0] not in sends[2:]


@pytest.mark.asyncio
async def test_real_loop_stream_fallback_never_repeats_confirmed_segments(transport, tmp_path):
    from echo_agent.agent.loop import AgentLoop
    from echo_agent.bus.events import InboundEvent
    from echo_agent.config.schema import Config
    from tests.plugins.test_runtime_extension_reliability import ReplyProvider
    from echo_agent.models.provider import LLMResponse
    from tests.test_outbound_final_dedup import _manager
    channel, bus, calls, replies, _ = transport
    channel._running = True
    _manager(bus, channel)
    class LongReply(ReplyProvider):
        async def chat(self, *args, **kwargs):
            return LLMResponse(content="A" * 32054, finish_reason="stop")
    config = Config(workspace=str(tmp_path), memory={"enabled": False},
                    channels={"stream_channels": ["dingtalk"]}, observability={"otel_enabled": False})
    loop = AgentLoop(bus=bus, config=config, provider=LongReply(), workspace=tmp_path)
    loop._running = True
    replies["send"] = [(200, {"processQueryKey": "first"}), (429, {"code": "Throttling"}),
                       (200, {"processQueryKey": "second"}), (200, {"processQueryKey": "third"})]
    await loop._on_inbound(InboundEvent.text_message("dingtalk", "staff", "staff123", "长回复", event_id="native-long"))
    sends = [json.loads(call["payload"]["msgParam"])["content"] for call in calls if "payload" in call]
    assert len(sends) == 4 and sends[0] not in sends[2:] and sends[1] == sends[2]
    parts = await bus._turn_run_store.outbound_parts("reply:native-long", "dingtalk")
    assert len(parts) == 3 and all(part["message_id"] for part in parts)


@pytest.mark.asyncio
async def test_ambiguous_partial_send_does_not_replay_or_claim_delivery(transport):
    channel, _, calls, replies, _ = transport
    event = OutboundEvent.text_reply(channel="dingtalk", chat_id="staff123", text="A" * 32054)
    replies["send"] = [(200, {"processQueryKey": "first"}), (200, {})]
    result = await channel.send(event)
    assert not result.success
    count = len(calls)
    result = await channel.send(event)
    assert not result.success and "unknown" in result.error
    assert len(calls) == count


@pytest.mark.asyncio
async def test_http_server_failure_is_ambiguous_and_never_automatically_replayed(transport):
    channel, _, calls, replies, _ = transport
    event = OutboundEvent.text_reply(channel="dingtalk", chat_id="staff123", text="A" * 32054)
    replies["send"] = [(200, {"processQueryKey": "first"}), (500, {"code": "InternalError"})]
    assert not (await channel.send(event)).success
    count = len(calls)
    assert "unknown" in (await channel.send(event)).error
    assert len(calls) == count


@pytest.mark.asyncio
async def test_timeout_keeps_unknown_checkpoint(transport, monkeypatch):
    channel, _, calls, _, _ = transport
    event = OutboundEvent.text_reply(channel="dingtalk", chat_id="staff123", text="A" * 32054)
    original = channel._send_message
    attempts = 0

    async def send(event, key, param):
        nonlocal attempts
        attempts += 1
        if attempts == 2:
            raise asyncio.TimeoutError()
        return await original(event, key, param)

    monkeypatch.setattr(channel, "_send_message", send)
    assert not (await channel.send(event)).success
    count = len(calls)
    assert "unknown" in (await channel.send(event)).error
    assert attempts == 2 and len(calls) == count


@pytest.mark.asyncio
@pytest.mark.parametrize("response", ["Too Many Requests", {"errcode": 88, "errmsg": "rate limited"}])
async def test_definite_rejection_without_standard_error_json_can_resume(transport, response):
    channel, bus, calls, replies, _ = transport
    event = OutboundEvent.text_reply("dingtalk", "staff123", "A" * 32054)
    replies["send"] = [(200, {"processQueryKey": "first"}), (429 if isinstance(response, str) else 200, response)]
    assert not (await bus.publish_outbound(event)).ok
    replies["send"] = [(200, {"processQueryKey": "second"}), (200, {"processQueryKey": "third"})]
    recovered = await bus.publish_outbound(event)
    assert recovered.ok and recovered.detail["message_ids"] == ["first", "second", "third"]
    assert len([call for call in calls if "payload" in call]) == 4


@pytest.mark.asyncio
async def test_cancelling_throttle_does_not_reserve_an_unsent_part(transport, monkeypatch):
    channel, bus, calls, _, _ = transport
    monkeypatch.setattr(dingtalk, "_TEXT_SEND_INTERVAL", 60)
    event = OutboundEvent.text_reply("dingtalk", "staff123", "A" * 32054)
    sending = asyncio.create_task(channel.send(event))
    for _ in range(100):
        part = await bus.get_outbound_part(f"dingtalk:{event.event_id}:0")
        if part and part["message_id"]:
            break
        await asyncio.sleep(0.01)
    else:
        pytest.fail("first part was not acknowledged")
    assert await bus.get_outbound_part(f"dingtalk:{event.event_id}:1") is None
    sending.cancel()
    with pytest.raises(asyncio.CancelledError):
        await sending
    monkeypatch.setattr(dingtalk, "_TEXT_SEND_INTERVAL", 0)
    assert (await bus.publish_outbound(event)).ok
    assert len([call for call in calls if "payload" in call]) == 3


@pytest.mark.asyncio
async def test_file_rendering_request_is_not_silently_changed_to_image(transport, tmp_path):
    channel, bus, calls, _, _ = transport
    path = tmp_path / "cover.png"
    path.write_bytes(b"image data")
    tool = SendFileTool(str(tmp_path), publish_fn=bus.publish_outbound, channel_lookup=lambda name: channel)
    result = await tool.execute({"channel": "dingtalk", "chat_id": "staff1", "file_path": str(path), "as_image": False})
    assert not result.success and "ZIP" in result.error
    assert calls == []


@pytest.mark.asyncio
async def test_attachment_retry_skips_confirmed_caption_and_files(transport, tmp_path):
    channel, bus, calls, replies, _ = transport
    paths = [tmp_path / "one.pdf", tmp_path / "two.pdf"]
    for path in paths:
        path.write_bytes(b"document")
    event = OutboundEvent(channel="dingtalk", chat_id="staff1", content=[
        ContentBlock(text="Reports"), *[ContentBlock(type=ContentType.FILE, url=str(path)) for path in paths],
    ])
    replies["send"] = [(200, {"processQueryKey": "caption"}), (200, {"processQueryKey": "one"}),
                       (429, {"code": "Throttling"})]
    assert not (await bus.publish_outbound(event)).ok
    replies["send"] = [(200, {"processQueryKey": "two"})]
    assert (await bus.publish_outbound(event)).ok
    uploads = [call["filename"] for call in calls if call["path"] == "/media/upload"]
    assert uploads == ["one.pdf", "two.pdf", "two.pdf"]
    texts = [call for call in calls if call.get("payload", {}).get("msgKey") == "sampleText"]
    assert len(texts) == 1


@pytest.mark.asyncio
@pytest.mark.parametrize("replacement", ["B" * 32054, "short changed reply"], ids=["long", "short"])
async def test_retry_cannot_change_long_reply_under_same_identity(transport, replacement):
    channel, _, calls, replies, _ = transport
    event = OutboundEvent.text_reply(channel="dingtalk", chat_id="staff123", text="A" * 32054)
    replies["send"] = [(200, {"processQueryKey": "first"}), (400, {"code": "InvalidmsgParam"})]
    assert not (await channel.send(event)).success
    count = len(calls)
    changed = OutboundEvent.text_reply(channel="dingtalk", chat_id="staff123", text=replacement)
    changed.event_id = event.event_id
    result = await channel.send(changed)
    assert not result.success and "conflict" in result.error
    assert len(calls) == count


@pytest.mark.asyncio
@pytest.mark.parametrize("chat_id,is_group", [("staff123", False), ("cidTest==", True)])
async def test_tool_delivers_real_file_with_caption(transport, tmp_path, chat_id, is_group):
    channel, bus, calls, _, _ = transport
    path = tmp_path / "每日报告.docx"
    path.write_bytes(b"test document bytes")
    tool = SendFileTool(str(tmp_path), publish_fn=bus.publish_outbound,
                        channel_lookup=lambda name: channel)
    result = await tool.execute({
        "channel": "dingtalk", "chat_id": chat_id,
        "file_path": str(path), "caption": "您的报告",
    })
    assert result.success, result.error
    uploads = [call for call in calls if call["path"] == "/media/upload"]
    assert len(uploads) == 1
    upload = uploads[0]
    assert upload["fields"]["media"] == path.read_bytes()
    assert upload["fields"].get("type", upload["query"].get("type", "").encode()) == b"file"
    assert upload["query"]["access_token"] == "test-token"
    assert upload["filename"] == path.name
    sends = [call for call in calls if "payload" in call]
    assert [call["payload"]["msgKey"] for call in sends] == ["sampleText", "sampleFile"]
    assert json.loads(sends[0]["payload"]["msgParam"]) == {"content": "您的报告"}
    assert json.loads(sends[1]["payload"]["msgParam"]) == {
        "mediaId": "@test-media", "fileName": path.name, "fileType": "docx",
    }
    for call in sends:
        assert call["token"] == "test-token"
        assert call["payload"]["robotCode"] == "test-robot"
        if is_group:
            assert call["path"].endswith("groupMessages/send")
            assert call["payload"]["openConversationId"] == chat_id
            assert "userIds" not in call["payload"]
        else:
            assert call["path"].endswith("oToMessages/batchSend")
            assert call["payload"]["userIds"] == [chat_id]
            assert "openConversationId" not in call["payload"]


@pytest.mark.asyncio
async def test_file_only_and_explicit_group_metadata(transport, tmp_path):
    channel, _, calls, _, _ = transport
    path = tmp_path / "REPORT.PDF"
    path.write_bytes(b"pdf data")
    result = await channel.send(OutboundEvent(
        channel="dingtalk", chat_id="group-without-prefix", metadata={"conversation_type": "2"},
        content=[ContentBlock(type=ContentType.FILE, url=str(path))],
    ))
    assert result.success, result.error
    assert result.message_id == "test-receipt"
    assert calls[-1]["payload"]["openConversationId"] == "group-without-prefix"
    assert json.loads(calls[-1]["payload"]["msgParam"])["fileType"] == "pdf"


@pytest.mark.asyncio
async def test_image_inferred_by_send_file_is_uploaded(transport, tmp_path):
    channel, bus, calls, _, _ = transport
    path = tmp_path / "cover.png"
    path.write_bytes(b"image data")
    tool = SendFileTool(str(tmp_path), publish_fn=bus.publish_outbound,
                        channel_lookup=lambda name: channel)
    result = await tool.execute({"channel": "dingtalk", "chat_id": "staff1", "file_path": str(path)})
    assert result.success, result.error
    assert calls[-1]["payload"]["msgKey"] == "sampleImageMsg"
    assert json.loads(calls[-1]["payload"]["msgParam"]) == {"photoURL": "@test-media"}
    upload = calls[-2]
    assert upload["fields"].get("type", upload["query"].get("type", "").encode()) == b"image"


@pytest.mark.asyncio
@pytest.mark.parametrize("kind,error", [
    ("missing", "not found"), ("directory", "regular file"),
    ("empty", "empty"), ("oversized", "20 MB"), ("unsupported", "unsupported"),
])
async def test_invalid_file_fails_before_sending_caption(transport, tmp_path, kind, error):
    channel, _, calls, _, _ = transport
    path = tmp_path / ("report.txt" if kind == "unsupported" else "report.pdf")
    if kind == "directory":
        path.mkdir()
    elif kind != "missing":
        path.write_bytes(b"" if kind == "empty" else b"data")
        if kind == "oversized":
            with path.open("r+b") as file:
                file.truncate(20 * 1024 * 1024 + 1)
    result = await channel.send(OutboundEvent(channel="dingtalk", chat_id="staff1", content=[
        ContentBlock(text="caption"), ContentBlock(type=ContentType.FILE, url=str(path)),
    ]))
    assert not result.success
    assert error.lower() in result.error.lower()
    assert not calls


@pytest.mark.asyncio
@pytest.mark.parametrize("reply,status,error", [
    ({"errcode": 40004, "errmsg": "invalid media type"}, 200, "40004"),
    ({"errcode": 0}, 200, "media_id"),
    ({"code": "Forbidden", "message": "no permission"}, 403, "403"),
    ("invalid json", 200, "JSON"),
])
async def test_upload_failure_reaches_tool(transport, tmp_path, reply, status, error):
    channel, bus, calls, replies, statuses = transport
    replies["upload"], statuses["upload"] = reply, status
    path = tmp_path / "report.pdf"
    path.write_bytes(b"data")
    tool = SendFileTool(str(tmp_path), publish_fn=bus.publish_outbound,
                        channel_lookup=lambda name: channel)
    result = await tool.execute({"channel": "dingtalk", "chat_id": "staff1", "file_path": str(path)})
    assert not result.success
    assert error in result.error
    assert not any("payload" in call for call in calls)


@pytest.mark.asyncio
@pytest.mark.parametrize("reply,status,error", [
    ({"code": "Forbidden", "message": "no permission"}, 403, "Forbidden"),
    ({"errcode": 88, "errmsg": "rate limited"}, 200, "88"),
    ({"processQueryKey": "receipt", "invalidStaffIdList": ["staff1"]}, 200, "invalidStaffIdList"),
    ({"processQueryKey": "receipt", "flowControlledStaffIdList": ["staff1"]}, 200, "flowControlledStaffIdList"),
    ({}, 200, "processQueryKey"),
])
async def test_send_failure_is_not_reported_as_file_sent(transport, tmp_path, reply, status, error):
    channel, bus, _, replies, statuses = transport
    replies["send"], statuses["send"] = reply, status
    path = tmp_path / "report.pdf"
    path.write_bytes(b"data")
    tool = SendFileTool(str(tmp_path), publish_fn=bus.publish_outbound,
                        channel_lookup=lambda name: channel)
    result = await tool.execute({"channel": "dingtalk", "chat_id": "staff1", "file_path": str(path)})
    assert not result.success
    assert error in result.error


@pytest.mark.asyncio
async def test_failed_token_refresh_prevents_upload(transport, tmp_path):
    channel, _, calls, replies, _ = transport
    replies["token"] = {"errcode": 40001, "errmsg": "invalid credential"}
    path = tmp_path / "report.pdf"
    path.write_bytes(b"data")
    result = await channel.send(OutboundEvent(channel="dingtalk", chat_id="staff1", content=[
        ContentBlock(type=ContentType.FILE, url=str(path)),
    ]))
    assert not result.success
    assert "token" in result.error.lower()
    assert [call["path"] for call in calls] == ["/gettoken"]


@pytest.mark.asyncio
async def test_nonfinal_file_is_skipped(transport):
    channel, _, calls, _, _ = transport
    result = await channel.send(OutboundEvent(channel="dingtalk", chat_id="staff1", is_final=False,
        content=[ContentBlock(type=ContentType.FILE, url="missing.pdf")]))
    assert result.success and result.skipped
    assert not calls


@pytest.mark.asyncio
async def test_plain_text_still_delivers(transport):
    channel, _, calls, _, _ = transport
    result = await channel.send(OutboundEvent.text_reply("dingtalk", "staff1", "hello"))
    assert result.success and result.message_id == "test-receipt"
    assert json.loads(calls[-1]["payload"]["msgParam"]) == {"content": "hello"}
    assert not any(call["path"] == "/media/upload" for call in calls)


@pytest.mark.asyncio
async def test_caption_failure_stops_attachment_upload(transport, tmp_path):
    channel, _, calls, replies, _ = transport
    replies["send"] = {"errcode": 88, "errmsg": "rate limited"}
    path = tmp_path / "report.pdf"
    path.write_bytes(b"data")
    result = await channel.send(OutboundEvent(channel="dingtalk", chat_id="staff1", content=[
        ContentBlock(text="caption"), ContentBlock(type=ContentType.FILE, url=str(path)),
    ]))
    assert not result.success
    assert not any(call["path"] == "/media/upload" for call in calls)


@pytest.mark.asyncio
async def test_multiple_files_stop_at_first_rejected_send(transport, tmp_path):
    channel, _, calls, replies, _ = transport
    replies["send"] = {"errcode": 88, "errmsg": "rate limited"}
    paths = [tmp_path / "one.pdf", tmp_path / "two.pdf"]
    for path in paths:
        path.write_bytes(b"data")
    result = await channel.send(OutboundEvent(channel="dingtalk", chat_id="staff1", content=[
        ContentBlock(type=ContentType.FILE, url=str(path)) for path in paths
    ]))
    assert not result.success
    assert len([call for call in calls if call["path"] == "/media/upload"]) == 1


@pytest.mark.asyncio
async def test_explicit_single_chat_overrides_cid_prefix(transport):
    channel, _, calls, _, _ = transport
    result = await channel.send(OutboundEvent.text_reply(
        "dingtalk", "cid-prefixed-staff", "hello", metadata={"conversation_type": "1"},
    ))
    assert result.success
    assert calls[-1]["payload"]["userIds"] == ["cid-prefixed-staff"]


@pytest.mark.asyncio
async def test_errors_redact_credentials(transport, tmp_path):
    channel, _, _, replies, _ = transport
    replies["upload"] = {"errcode": 40004, "errmsg": "test-token test-key test-secret"}
    path = tmp_path / "report.pdf"
    path.write_bytes(b"data")
    result = await channel.send(OutboundEvent(channel="dingtalk", chat_id="staff1", content=[
        ContentBlock(type=ContentType.FILE, url=str(path)),
    ]))
    assert not result.success
    assert "40004" in result.error
    assert all(secret not in result.error for secret in ("test-token", "test-key", "test-secret"))


@pytest.mark.asyncio
async def test_gateway_registers_application_and_robot_callback(transport, monkeypatch):
    channel, _, calls, _, _ = transport

    async def listen(endpoint, ticket):
        assert (endpoint, ticket) == ("wss://stream.example", "test-ticket")
        channel._running = False

    monkeypatch.setattr(channel, "_stream_listen", listen)
    channel._running = True
    await asyncio.wait_for(channel._callback_register_and_poll(), timeout=2)
    payload = calls[-1]["payload"]
    assert payload["clientId"] == "test-key"
    assert payload["clientSecret"] == "test-secret"
    assert payload["subscriptions"] == [{"type": "CALLBACK", "topic": "/v1.0/im/bot/messages/get"}]


@pytest.mark.asyncio
async def test_upload_network_error_does_not_expose_token(transport, tmp_path, monkeypatch):
    channel, _, _, _, _ = transport
    await channel._ensure_token()
    monkeypatch.setattr(channel._session, "post", MagicMock(side_effect=aiohttp.ClientConnectionError(
        "connection failed https://oapi.dingtalk.com/media/upload?access_token=test-token",
    )))
    path = tmp_path / "report.pdf"
    path.write_bytes(b"data")
    result = await channel.send(OutboundEvent(channel="dingtalk", chat_id="staff1", content=[
        ContentBlock(type=ContentType.FILE, url=str(path)),
    ]))
    assert not result.success
    assert "test-token" not in result.error


@pytest.mark.asyncio
async def test_stream_echoes_ping_and_receives_group_callback(transport):
    channel, _, _, _, _ = transport
    channel._handle_message = AsyncMock()
    acknowledgements = []

    async def stream(request):
        ws = web.WebSocketResponse()
        await ws.prepare(request)
        frames = [
            {"type": "SYSTEM", "headers": {"messageId": "ping1", "topic": "ping"},
             "data": '{"opaque":"heartbeat"}'},
            {"type": "CALLBACK", "headers": {"messageId": "callback1", "topic": "/v1.0/im/bot/messages/get"},
             "data": json.dumps({"senderStaffId": "staff1", "conversationId": "cidTest==",
                                 "conversationType": "2", "msgtype": "text", "text": {"content": "send report"}})},
        ]
        for frame in frames:
            await ws.send_json(frame)
            acknowledgements.append(await ws.receive_json(timeout=2))
        await ws.close()
        return ws

    app = web.Application()
    app.router.add_get("/stream", stream)
    runner = web.AppRunner(app)
    await runner.setup()
    site = web.TCPSite(runner, "127.0.0.1", 0)
    await site.start()
    endpoint = f"http://127.0.0.1:{site._server.sockets[0].getsockname()[1]}/stream"
    try:
        await asyncio.wait_for(channel._stream_listen(endpoint, "test-ticket"), timeout=3)
    finally:
        await runner.cleanup()
    assert acknowledgements[0]["data"] == '{"opaque":"heartbeat"}'
    assert acknowledgements[0]["headers"]["messageId"] == "ping1"
    assert json.loads(acknowledgements[1]["data"]) == {"response": "OK"}
    assert channel._handle_message.await_args.kwargs["chat_id"] == "cidTest=="
    assert channel._handle_message.await_args.kwargs["metadata"]["conversation_type"] == "2"
