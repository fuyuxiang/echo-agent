"""QQ's entire ordered reply uses the same durable checkpoint, including media."""
from unittest.mock import AsyncMock
from types import SimpleNamespace

import pytest

from echo_agent.agent.turn_run_store import TurnRunStore
from echo_agent.bus.events import ContentBlock, ContentType, OutboundEvent
from echo_agent.bus.queue import MessageBus
from echo_agent.channels.qqbot import QQBotChannel
from echo_agent.config.schema import QQBotChannelConfig
from echo_agent.storage.sqlite import SQLiteBackend


def channel(bus, session, *, media=False):
    result = QQBotChannel(QQBotChannelConfig(app_id="fixture", app_secret="fixture", markdown_support=False,
                                           media_enabled=media, media_parse_tags=False), bus)
    result._session = session
    result._access_token, result._token_expires = "fixture", 9e18
    return result


@pytest.mark.asyncio
@pytest.mark.parametrize("status", [400, 500])
async def test_qq_partial_text_restarts_without_replaying_confirmed_parts(tmp_path, status):
    storage = SQLiteBackend(tmp_path / "qq.db")
    await storage.initialize()
    bus = MessageBus()
    store = TurnRunStore(storage)
    bus.set_turn_run_store(store)
    calls, statuses = [], [200, status, 200, 200]
    class Response:
        async def json(self):
            return {"id": f"receipt-{len(calls)}"}
        async def __aenter__(self):
            return self
        async def __aexit__(self, *args):
            pass
        async def text(self):
            return "rejected"
    def post(url, **kwargs):
        calls.append(kwargs["json"]["content"])
        response = Response()
        response.status = statuses.pop(0)
        return response
    session = SimpleNamespace(post=post)
    first = channel(bus, session)
    bus.subscribe_outbound("qqbot", first.send)
    event = OutboundEvent.text_reply("qqbot", "chat", "1" * 4000 + "2" * 4000 + "3" * 500)
    try:
        result = await bus.publish_outbound(event)
        assert not result.ok and bool(result.detail.get("delivery_unknown")) == (status == 500)
        assert len(calls) == 2
        second = channel(bus, session)
        if status == 500:
            assert not (await second.send(event)).success
            assert len(calls) == 2
            await store.resolve_outbound(event.event_id, confirmed_not_delivered=True)
        result = await second.send(event)
        assert result.success and len(result.message_ids) == 3
        assert calls == ["1" * 4000, "2" * 4000, "2" * 4000, "3" * 500]
    finally:
        await storage.close()


@pytest.mark.asyncio
async def test_qq_mixed_reply_does_not_resend_caption_or_a_confirmed_attachment(tmp_path):
    storage = SQLiteBackend(tmp_path / "qq-media.db")
    await storage.initialize()
    bus, store = MessageBus(), TurnRunStore(storage)
    bus.set_turn_run_store(store)
    first = channel(bus, object(), media=True)
    first._send_chunk = AsyncMock(return_value=True)
    first._send_media = AsyncMock(side_effect=[True, False])
    bus.subscribe_outbound("qqbot", first.send)
    event = OutboundEvent(channel="qqbot", chat_id="chat", content=[
        ContentBlock(text="文件说明"), ContentBlock(type=ContentType.FILE, url="https://files.example/one.pdf"),
        ContentBlock(type=ContentType.FILE, url="https://files.example/two.pdf"),
    ])
    try:
        assert not (await bus.publish_outbound(event)).ok
        await store.resolve_outbound(event.event_id, confirmed_not_delivered=True)
        second = channel(bus, object(), media=True)
        second._send_chunk, second._send_media = AsyncMock(return_value=True), AsyncMock(return_value=True)
        assert (await second.send(event)).success
        second._send_chunk.assert_not_awaited()
        assert second._send_media.await_count == 1
        assert second._send_media.call_args.args[2].content.endswith("two.pdf")
    finally:
        await storage.close()


@pytest.mark.asyncio
async def test_qq_upload_uses_preflight_bytes_and_refuses_changed_file_on_retry(tmp_path, monkeypatch):
    import base64
    from echo_agent.channels import qqbot
    storage = SQLiteBackend(tmp_path / "qq-file.db")
    await storage.initialize()
    bus = MessageBus()
    bus.set_turn_run_store(TurnRunStore(storage))
    ch = channel(bus, object(), media=True)
    path = tmp_path / "report.pdf"
    path.write_bytes(b"original-report")
    async def upload(*args, **kwargs):
        path.write_bytes(b"changed-report")
        assert base64.b64decode(kwargs["file_data"]) == b"original-report"
        assert kwargs["file_name"] == "report.pdf"
        return {"file_info": "fixture-file"}
    uploading = AsyncMock(side_effect=upload)
    monkeypatch.setattr(qqbot, "upload_media", uploading)
    monkeypatch.setattr(qqbot, "send_media_message", AsyncMock(return_value={"id": "remote-file"}))
    event = OutboundEvent(channel="qqbot", chat_id="chat", content=[ContentBlock(type=ContentType.FILE, url=str(path))])
    try:
        assert (await ch.send(event)).success
        conflict = await ch.send(event)
        assert not conflict.success and "identity conflict" in conflict.error
        assert uploading.await_count == 1
        path.write_bytes(b"original-report")
        assert (await ch.send(event)).success
        assert uploading.await_count == 1
    finally:
        await storage.close()
