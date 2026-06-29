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


def _hb_event(milestone=1, text="⏳ 已用时 1 分钟（思考中）", *, key="evt1", is_key=False):
    out = OutboundEvent.text_reply(channel="x", chat_id="c1", text=text)
    out.is_final = False
    out.message_kind = "heartbeat"
    out.metadata = {"_heartbeat": True, "_inbound_event_id": key,
                    "_hb_milestone": milestone, "_hb_key": is_key}
    return out


@pytest.fixture
def manager():
    return ChannelManager(ChannelsConfig(), MessageBus())


def _with_verbosity(manager, verbosity):
    from echo_agent.config.schema import HeartbeatConfig
    manager._heartbeat_cfg = HeartbeatConfig(verbosity=verbosity)


@pytest.mark.asyncio
async def test_editable_channel_edits_single_message(manager):
    ch = _FakeChannel(supports_edit=True)
    manager._channels["x"] = ch
    _with_verbosity(manager, "every_tool")
    await manager._filter_and_dispatch(_hb_event(1, "hb1"))
    await manager._filter_and_dispatch(_hb_event(2, "hb2"))
    assert len(ch.sent) == 1          # first beat sends
    assert len(ch.edits) == 1         # second beat edits
    assert ch.edits[0][1] == "hb2"
    assert ch.typings >= 2            # typing refreshed each beat


@pytest.mark.asyncio
async def test_uneditable_same_milestone_sends_once(manager):
    # New model: repeat beats for the same milestone seq collapse to one send
    # (was first_only). every_tool isolates dedup from key-milestone suppression.
    ch = _FakeChannel(supports_edit=False)
    manager._channels["x"] = ch
    _with_verbosity(manager, "every_tool")
    await manager._filter_and_dispatch(_hb_event(1, "hb1"))
    await manager._filter_and_dispatch(_hb_event(1, "hb2"))  # same milestone -> deduped
    assert len(ch.sent) == 1
    assert ch.typings >= 2            # still keeps typing alive


@pytest.mark.asyncio
async def test_silent_verbosity_sends_nothing_but_typing(manager):
    # New model: verbosity="silent" replaces the legacy on_uneditable="off".
    ch = _FakeChannel(supports_edit=False)
    manager._channels["x"] = ch
    _with_verbosity(manager, "silent")
    await manager._filter_and_dispatch(_hb_event(1, "hb1"))
    assert len(ch.sent) == 0
    assert ch.typings >= 1


@pytest.mark.asyncio
async def test_uneditable_new_milestones_each_send(manager):
    # New model: each advancing milestone sends once under every_tool (was "every").
    ch = _FakeChannel(supports_edit=False)
    manager._channels["x"] = ch
    _with_verbosity(manager, "every_tool")
    await manager._filter_and_dispatch(_hb_event(1, "hb1"))
    await manager._filter_and_dispatch(_hb_event(2, "hb2"))
    assert ch.sent == ["hb1", "hb2"]


def _final_event(key="evtX", text="最终答案"):
    out = OutboundEvent.text_reply(channel="x", chat_id="c1", text=text)
    out.is_final = True
    out.message_kind = "final"
    out.metadata = {"_inbound_event_id": key}
    return out


def _late_hb_event(key="evtX", text="正在处理中…", milestone=1):
    out = OutboundEvent.text_reply(channel="x", chat_id="c1", text=text)
    out.is_final = False
    out.message_kind = "heartbeat"
    out.metadata = {"_heartbeat": True, "_inbound_event_id": key,
                    "_hb_milestone": milestone, "_hb_key": False}
    return out


@pytest.mark.asyncio
async def test_late_heartbeat_after_final_is_discarded(manager):
    """Important 1: a heartbeat that lands after the turn is finalized must be
    dropped entirely — no fresh send, no edit — even after _on_outbound_final
    has cleaned up the heartbeat msg id mapping."""
    ch = _FakeChannel(supports_edit=True)
    manager._channels["x"] = ch
    # Final answer is delivered first (fresh send, no prior heartbeat).
    await manager._filter_and_dispatch(_final_event())
    assert ch.sent == ["最终答案"]
    sent_before = len(ch.sent)
    edits_before = len(ch.edits)
    # A timer-scheduled heartbeat for the same turn fires late.
    await manager._handle_heartbeat(_late_hb_event())
    # It must produce no new platform side effects.
    assert len(ch.sent) == sent_before
    assert len(ch.edits) == edits_before


class _SealFailChannel:
    """Editable channel whose finalize-edit fails, exercising the delete fallback."""

    def __init__(self):
        self.supports_edit = True
        self.is_running = True
        self.config = type("C", (), {"reactions_enabled": False})()
        self.sent = []
        self.edits = []
        self.deleted = []
        self._mid = 0

    async def send(self, event):
        self._mid += 1
        self.sent.append(event.text)
        return SendResult(success=True, message_id=f"m{self._mid}")

    async def edit_message(self, chat_id, message_id, text, *, metadata=None, finalize=False):
        self.edits.append((message_id, text, finalize))
        if finalize:
            return SendResult(success=False, error="message too old")
        return SendResult(success=True, message_id=message_id)

    async def delete_message(self, chat_id, message_id, metadata=None):
        self.deleted.append(message_id)
        return SendResult(success=True, message_id=message_id)

    async def send_typing(self, chat_id, metadata=None):
        pass

    async def stop_typing(self, chat_id):
        pass


@pytest.mark.asyncio
async def test_seal_edit_failure_deletes_stale_heartbeat(manager):
    """Important 2: when the finalize-edit fails, the stale heartbeat message
    is deleted before falling back to a fresh send, so the turn keeps one slot."""
    ch = _SealFailChannel()
    manager._channels["x"] = ch
    # First heartbeat records a platform msg id for the turn.
    await manager._handle_heartbeat(_late_hb_event(key="evt2", text="心跳"))
    assert ch.sent == ["心跳"]
    hb_msg_id = manager._heartbeat_msg_ids["evt2"]
    # Final delivery: seal-edit fails -> delete stale msg -> fresh send.
    await manager._deliver_final(_final_event(key="evt2"))
    assert hb_msg_id in ch.deleted
    assert "最终答案" in ch.sent
