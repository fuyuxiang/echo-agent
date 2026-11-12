"""Optional delivery telemetry cannot make the channel overview unavailable."""

import json
from types import SimpleNamespace
from unittest.mock import MagicMock

import pytest

from echo_agent.gateway.api.channels import ChannelsAPI


@pytest.fixture
def overview():
    adapters = {"weixin": None, "telegram": None}
    server = SimpleNamespace(
        _require_api_token=lambda *args, **kwargs: None,
        _agent_loop=SimpleNamespace(config=SimpleNamespace(channels=SimpleNamespace(
            weixin=SimpleNamespace(enabled=True), telegram=SimpleNamespace(enabled=True),
        ))),
        channel_manager=SimpleNamespace(
            active_channels=["weixin", "telegram"], get_channel=adapters.get,
        ),
    )
    return ChannelsAPI(server), adapters


async def channel_rows(api):
    response = await api.list_channels(SimpleNamespace())
    assert response.status == 200
    return {row["name"]: row for row in json.loads(response.text)["channels"]}


@pytest.mark.asyncio
@pytest.mark.parametrize("adapter", [None, SimpleNamespace(), MagicMock()])
async def test_unreported_delivery_status_keeps_the_overview_available(overview, adapter):
    api, adapters = overview
    adapters["weixin"] = adapter
    rows = await channel_rows(api)
    assert set(rows) == {"weixin", "telegram"}
    assert "delivery_recovery" not in rows["weixin"]
    assert "delivery_health" not in rows["weixin"]


@pytest.mark.asyncio
@pytest.mark.parametrize("summary", [
    None, MagicMock(), [], "retrying", {},
    {"pending_conversations": 1, "retry_at_ms": 1000},
    {"pending_conversations": True, "retry_at_ms": 1000, "retry_attempts": 1},
    {"pending_conversations": 1, "retry_at_ms": -1, "retry_attempts": 1},
    {"pending_conversations": 1, "retry_at_ms": "1000", "retry_attempts": 1},
    {"pending_conversations": 1, "retry_at_ms": 1000, "retry_attempts": float("nan")},
    {"pending_conversations": 1, "retry_at_ms": 2**53, "retry_attempts": 1},
])
async def test_invalid_recovery_summary_does_not_hide_delivery_health(overview, summary):
    api, adapters = overview
    adapters["weixin"] = SimpleNamespace(
        delivery_health=lambda: "cooling_down", delivery_recovery=lambda: summary,
    )
    rows = await channel_rows(api)
    assert rows["weixin"]["delivery_health"] == "cooling_down"
    assert "delivery_recovery" not in rows["weixin"]
    assert rows["telegram"]["running"]


@pytest.mark.asyncio
async def test_recovery_summary_exposes_only_public_fields(overview):
    api, adapters = overview
    public = {"pending_conversations": 2, "retry_at_ms": 1000, "retry_attempts": 1}
    adapters["weixin"] = SimpleNamespace(delivery_recovery=lambda: {
        **public, "credential": "private-token", "chats": {"private-peer": object()},
    })
    rows = await channel_rows(api)
    assert rows["weixin"]["delivery_recovery"] == public
    assert "private" not in json.dumps(rows)


@pytest.mark.asyncio
@pytest.mark.parametrize("broken_method", ["delivery_health", "delivery_recovery"])
async def test_telemetry_failure_preserves_other_status_and_channels(overview, broken_method):
    api, adapters = overview
    public = {"pending_conversations": 1, "retry_at_ms": 1000, "retry_attempts": 1}
    adapter = SimpleNamespace(delivery_health=lambda: "cooling_down", delivery_recovery=lambda: public)
    setattr(adapter, broken_method, MagicMock(side_effect=RuntimeError("private-credential")))
    adapters["weixin"] = adapter
    rows = await channel_rows(api)
    assert broken_method not in rows["weixin"]
    if broken_method == "delivery_health":
        assert rows["weixin"]["delivery_recovery"] == public
    else:
        assert rows["weixin"]["delivery_health"] == "cooling_down"
    assert rows["telegram"]["running"]
    assert "private-credential" not in json.dumps(rows)


@pytest.mark.asyncio
async def test_real_weixin_cooldown_and_recovery_are_visible_in_the_overview(overview, tmp_path):
    from echo_agent.bus.queue import MessageBus
    from echo_agent.channels.weixin import WeixinChannel
    from echo_agent.config.schema import WeixinChannelConfig

    api, adapters = overview
    channel = WeixinChannel(WeixinChannelConfig(
        account_id="private-account", token="private-token", data_dir=str(tmp_path),
    ), MessageBus())
    adapters["weixin"] = channel
    channel._token_store.set("private-account", "private-peer", "private-context")
    channel._delivery_recovery._clock = lambda: 100_000.0
    failure = channel._send_failure("private-peer", {"ret": -2}, "temporarily refused")
    rows = await channel_rows(api)
    assert rows["weixin"]["delivery_health"] == "cooling_down"
    assert rows["weixin"]["delivery_recovery"] == {
        "pending_conversations": 1, "retry_at_ms": failure.retry_at_ms, "retry_attempts": 1,
    }
    assert "private" not in json.dumps(rows)

    channel._delivery_recovery.accepted("private-peer")
    rows = await channel_rows(api)
    assert rows["weixin"]["delivery_health"] == "ready"
    assert rows["weixin"]["delivery_recovery"] == {
        "pending_conversations": 0, "retry_at_ms": 0, "retry_attempts": 0,
    }
