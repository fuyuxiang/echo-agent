from pathlib import Path

import pytest

from echo_agent.agent.inspection.store import InspectStore
from echo_agent.agent.inspection.tick import run_inspection_tick


class _Cfg:
    inspect_file = "INSPECT.md"
    max_items_per_tick = 5
    deliver_channel = "weixin"
    deliver_chat_id = "room1"


class _Bus:
    def __init__(self):
        self.published = []

    async def publish_inbound(self, event):
        self.published.append(event)
        return True


def _store_with_items(tmp_path: Path, body: str) -> InspectStore:
    md = tmp_path / "INSPECT.md"
    md.write_text(body, encoding="utf-8")
    return InspectStore(md, tmp_path / "state.json")


@pytest.mark.asyncio
async def test_tick_no_due_items_does_not_publish(tmp_path: Path):
    store = _store_with_items(tmp_path, "# c\n")  # no items
    bus = _Bus()
    n = await run_inspection_tick(store, _Cfg(), bus, now_sec=1000)
    assert n == 0
    assert bus.published == []


@pytest.mark.asyncio
async def test_tick_publishes_cron_event_for_due(tmp_path: Path):
    store = _store_with_items(
        tmp_path, "## 官网\n- interval: 10m\n- check: 访问 x.com\n"
    )
    bus = _Bus()
    n = await run_inspection_tick(store, _Cfg(), bus, now_sec=1000)
    assert n == 1
    assert len(bus.published) == 1
    ev = bus.published[0]
    assert ev.metadata.get("_inspection") is True
    assert ev.channel == "weixin" and ev.chat_id == "room1"
    assert "官网" in ev.content[0].text


@pytest.mark.asyncio
async def test_tick_marks_last_checked(tmp_path: Path):
    store = _store_with_items(
        tmp_path, "## 官网\n- interval: 10m\n- check: 访问 x.com\n"
    )
    bus = _Bus()
    await run_inspection_tick(store, _Cfg(), bus, now_sec=1000)
    state = store.load_state()
    assert state["官网"]["last_checked_at"] == 1000
    # not due again immediately
    n2 = await run_inspection_tick(store, _Cfg(), bus, now_sec=1200)
    assert n2 == 0


class _RaisingBus:
    async def publish_inbound(self, event):
        raise RuntimeError("bus exploded")


class _RejectingBus:
    def __init__(self):
        self.published = []

    async def publish_inbound(self, event):
        self.published.append(event)
        return False


@pytest.mark.asyncio
async def test_tick_failopen_on_publish_exception(tmp_path: Path):
    store = _store_with_items(
        tmp_path, "## 官网\n- interval: 10m\n- check: 访问 x.com\n"
    )
    n = await run_inspection_tick(store, _Cfg(), _RaisingBus(), now_sec=1000)
    assert n == 0
    # state must NOT be marked checked (item stays due for retry)
    assert store.load_state().get("官网", {}).get("last_checked_at") is None


@pytest.mark.asyncio
async def test_tick_failopen_on_bus_rejection(tmp_path: Path):
    store = _store_with_items(
        tmp_path, "## 官网\n- interval: 10m\n- check: 访问 x.com\n"
    )
    n = await run_inspection_tick(store, _Cfg(), _RejectingBus(), now_sec=1000)
    assert n == 0
    assert store.load_state().get("官网", {}).get("last_checked_at") is None


@pytest.mark.asyncio
async def test_tick_failopen_on_load_error(tmp_path: Path, monkeypatch):
    store = _store_with_items(
        tmp_path, "## 官网\n- interval: 10m\n- check: 访问 x.com\n"
    )

    def _boom():
        raise RuntimeError("load exploded")

    monkeypatch.setattr(store, "load_items", _boom)
    bus = _Bus()
    n = await run_inspection_tick(store, _Cfg(), bus, now_sec=1000)
    assert n == 0
    assert bus.published == []
