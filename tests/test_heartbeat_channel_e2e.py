import pytest
from echo_agent.bus.events import OutboundEvent
from echo_agent.bus.queue import MessageBus
from echo_agent.channels.manager import ChannelManager
from echo_agent.channels.base import SendResult
from echo_agent.config.schema import ChannelsConfig


class _FakeChannel:
    def __init__(self, supports_edit):
        self.supports_edit = supports_edit
        self.is_running = True
        self.config = type("C", (), {"reactions_enabled": False})()
        self.sent = []
        self.edits = []
        self.typings = 0
        self._mid = 0

    async def send(self, event):
        self._mid += 1
        self.sent.append(event.text)
        return SendResult(success=True, message_id=f"m{self._mid}")

    async def edit_message(self, chat_id, message_id, text, *, metadata=None, finalize=False):
        self.edits.append((message_id, text))
        return SendResult(success=True, message_id=message_id)

    async def send_typing(self, chat_id, metadata=None):
        self.typings += 1

    async def stop_typing(self, chat_id):
        pass


def _hb_event(uneditable="first_only", text="⏳ 已用时 1 分钟（思考中）"):
    out = OutboundEvent.text_reply(channel="x", chat_id="c1", text=text)
    out.is_final = False
    out.message_kind = "heartbeat"
    out.metadata = {"_heartbeat": True, "_inbound_event_id": "evt1",
                    "_hb_on_uneditable": uneditable}
    return out


@pytest.fixture
def manager():
    return ChannelManager(ChannelsConfig(), MessageBus())


@pytest.mark.asyncio
async def test_editable_channel_edits_single_message(manager):
    ch = _FakeChannel(supports_edit=True)
    manager._channels["x"] = ch
    await manager._filter_and_dispatch(_hb_event(text="hb1"))
    await manager._filter_and_dispatch(_hb_event(text="hb2"))
    assert len(ch.sent) == 1          # first beat sends
    assert len(ch.edits) == 1         # second beat edits
    assert ch.edits[0][1] == "hb2"
    assert ch.typings >= 2            # typing refreshed each beat


@pytest.mark.asyncio
async def test_uneditable_first_only_sends_once(manager):
    ch = _FakeChannel(supports_edit=False)
    manager._channels["x"] = ch
    await manager._filter_and_dispatch(_hb_event("first_only", "hb1"))
    await manager._filter_and_dispatch(_hb_event("first_only", "hb2"))
    assert len(ch.sent) == 1
    assert ch.typings >= 2            # still keeps typing alive


@pytest.mark.asyncio
async def test_uneditable_off_sends_nothing_but_typing(manager):
    ch = _FakeChannel(supports_edit=False)
    manager._channels["x"] = ch
    await manager._filter_and_dispatch(_hb_event("off", "hb1"))
    assert len(ch.sent) == 0
    assert ch.typings >= 1


@pytest.mark.asyncio
async def test_uneditable_every_sends_each(manager):
    ch = _FakeChannel(supports_edit=False)
    manager._channels["x"] = ch
    await manager._filter_and_dispatch(_hb_event("every", "hb1"))
    await manager._filter_and_dispatch(_hb_event("every", "hb2"))
    assert ch.sent == ["hb1", "hb2"]
