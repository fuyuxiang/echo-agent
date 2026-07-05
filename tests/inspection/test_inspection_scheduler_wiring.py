import pytest

from echo_agent.scheduler.delivery import build_scheduled_job_handler


class _Job:
    def __init__(self, payload):
        self.payload = payload
        self.id = "j1"
        self.name = "n"


class _Bus:
    def __init__(self):
        self.published = []

    async def publish_inbound(self, event):
        self.published.append(event)
        return True


@pytest.mark.asyncio
async def test_handler_routes_inspection_tick_to_runner():
    calls = []

    async def _runner():
        calls.append(1)

    handler = build_scheduled_job_handler(_Bus(), inspection_runner=_runner)
    await handler(_Job({"_inspection_tick": True}))
    assert calls == [1]  # runner invoked, not the command path


@pytest.mark.asyncio
async def test_handler_routes_normal_job_to_command_path():
    bus = _Bus()
    handler = build_scheduled_job_handler(bus, inspection_runner=None)
    await handler(_Job({"command": "hello", "source_session_key": "weixin:room1"}))
    assert len(bus.published) == 1  # normal command still published as inbound
    assert bus.published[0].content[0].text == "hello"
