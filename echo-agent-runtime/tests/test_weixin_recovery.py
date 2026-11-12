import json
from types import SimpleNamespace

import pytest

from echo_agent.channels.weixin import _atomic_write_json
from echo_agent.channels.weixin_recovery import WeixinDeliveryRecovery


def test_recovery_cooldown_survives_restart_and_has_bounded_daily_attempts(tmp_path):
    now = [100_000.0]
    def create(token="secret"):
        return WeixinDeliveryRecovery(tmp_path, "account", token, _atomic_write_json, clock=lambda: now[0])
    state = create()
    for attempt, delay in [(1, 900_000), (2, 3_600_000), (3, 10_800_000)]:
        entry = state.reject("user")
        assert entry["attempts"] == attempt
        assert entry["retry_at_ms"] == int(now[0] * 1000) + delay
        state = create()
        assert state.chats["user"] == entry
        now[0] = entry["retry_at_ms"] / 1000
    entry = state.reject("user")
    assert entry["attempts"] == 4
    assert entry["retry_at_ms"] == entry["window_started_ms"] + state.WINDOW_MS
    now[0] = entry["retry_at_ms"] / 1000
    assert state.reject("user")["attempts"] == 1
    assert create("new-login-token").chats == {}
    assert (tmp_path / "account.delivery-state.json").stat().st_mode & 0o077 == 0


def test_fresh_message_recovers_only_its_own_chat_and_keeps_credentials(tmp_path):
    state = WeixinDeliveryRecovery(tmp_path, "account", "secret", _atomic_write_json)
    state.reject("one")
    state.reject("two")
    state.login_required = True
    state.save()
    restored = WeixinDeliveryRecovery(tmp_path, "account", "secret", _atomic_write_json)
    assert restored.login_required
    restored.fresh_context("one")
    assert not restored.login_required
    assert set(restored.chats) == {"two"}
    restored.accepted("two")
    assert restored.summary()["pending_conversations"] == 0


@pytest.mark.asyncio
async def test_scheduled_turn_does_not_consume_send_allowance_for_heartbeats():
    from echo_agent.agent.progress_heartbeat import ProgressHeartbeat, SharedActivityState

    heartbeat = ProgressHeartbeat(None, SimpleNamespace(unattended=True), SimpleNamespace(enabled=True))
    await heartbeat.start(SharedActivityState(started_at=0))
    assert heartbeat._task is None
    await heartbeat.stop()


def test_channel_delivery_outage_is_visible_in_readiness_without_leaking_peers():
    from echo_agent.gateway.server import GatewayServer

    channel = SimpleNamespace(delivery_health=lambda: "cooling_down")
    server = SimpleNamespace(channel_manager=SimpleNamespace(active_channels=["weixin"], get_channel=lambda _: channel))
    baseline = {"ready": True, "status": "ready", "checks": {"storage": {"ok": True}}}
    result = GatewayServer._with_delivery_health(server, baseline)
    assert result["ready"] is False and result["status"] == "degraded"
    assert result["channel_delivery"] == {"weixin": "cooling_down"}
    assert not result["checks"]["channel_delivery"]["ok"]
    assert baseline["ready"] and "channel_delivery" not in baseline["checks"]
    assert "user" not in json.dumps(result)
