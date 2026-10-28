"""Server review regressions: persisted configuration and static boundaries."""

import json
from types import SimpleNamespace
from unittest.mock import AsyncMock

import pytest
import yaml
from aiohttp import web
from aiohttp.test_utils import TestClient, TestServer

from echo_agent.config.schema import Config, GatewayConfig
from echo_agent.gateway.api.config import ConfigAPI
from echo_agent.gateway.server import GatewayServer


class Request:
    def __init__(self, changes=None, match_info=None):
        self.changes = changes
        self.match_info = match_info or {}

    async def json(self):
        return {"changes": self.changes}


def config_api(tmp_path, config=None):
    config = config or Config()
    server = SimpleNamespace(
        _agent_loop=SimpleNamespace(config=config),
        _config_path=tmp_path / "echo-agent.yaml",
        _require_admin_token=lambda *args, **kwargs: None,
        dashboard_ws=SimpleNamespace(broadcast=AsyncMock()),
    )
    return ConfigAPI(server), config, server


@pytest.mark.asyncio
async def test_saved_config_is_separate_from_running_state_and_survives_page_reload(tmp_path):
    api, runtime, server = config_api(tmp_path)
    response = await api.update_config(Request({"memory.enabled": False}))
    assert response.status == 200
    assert runtime.memory.enabled is True
    # A fresh handler/page must discover pending state from the actual file.
    response = await ConfigAPI(server).get_config(Request())
    body = json.loads(response.body)
    assert body["memory"]["enabled"] is False
    assert body["_meta"]["restart_required"] is True
    assert "memory.enabled" in body["_meta"]["pending_paths"]
    assert body["_meta"]["running"]["memory"]["enabled"] is True
    # Restart loads the persisted value and clears the discrepancy.
    runtime.memory.enabled = False
    body = json.loads((await ConfigAPI(server).get_config(Request())).body)
    assert body["_meta"]["restart_required"] is False


@pytest.mark.asyncio
@pytest.mark.parametrize("changes", [
    {"channels.telegram": {"enabled": True, "token": "***"}},
    {"channels": {"telegram": {"token": "replacement"}}},
    {"channels.telegram": {"appSecret": "replacement"}},
    {"ui.no_such_field": "ignored"},
    {"ui.locale.invalid": "bad"},
])
async def test_config_rejects_nested_secrets_and_unknown_fields(tmp_path, changes):
    api, _, server = config_api(tmp_path)
    response = await api.update_config(Request(changes))
    assert response.status == 400
    assert not server._config_path.exists()


@pytest.mark.asyncio
async def test_partial_section_update_preserves_sibling_credentials_and_prior_saved_values(tmp_path):
    api, runtime, server = config_api(tmp_path)
    runtime.channels.telegram.token = "keep-secret"
    server._config_path.write_text("channels:\n  telegram:\n    token: keep-secret\n")
    assert (await api.update_config(Request({"channels.telegram": {"enabled": True}}))).status == 200
    assert (await api.update_config(Request({"ui.locale": "zh"}))).status == 200
    saved = yaml.safe_load(server._config_path.read_text())
    assert saved["channels"]["telegram"]["token"] == "keep-secret"
    assert saved["channels"]["telegram"]["enabled"] is True
    assert runtime.channels.telegram.enabled is False


@pytest.mark.asyncio
async def test_dashboard_rejects_symlink_into_same_prefix_sibling(tmp_path):
    dashboard = tmp_path / "dashboard"
    dashboard.mkdir()
    (dashboard / "index.html").write_text("Dashboard")
    sibling = tmp_path / "dashboard-secrets"
    sibling.mkdir()
    (sibling / "secret.txt").write_text("must-not-be-served")
    (dashboard / "leak").symlink_to(sibling, target_is_directory=True)
    server = object.__new__(GatewayServer)
    server._resolve_dashboard_dir = lambda: dashboard
    app = web.Application()
    app.router.add_get("/{path:.*}", server._handle_dashboard)
    async with TestClient(TestServer(app)) as client:
        response = await client.get("/leak/secret.txt")
        assert "must-not-be-served" not in await response.text()


@pytest.mark.asyncio
async def test_fallback_bootstrap_exposes_interactive_path_without_secrets():
    server = object.__new__(GatewayServer)
    server._config = GatewayConfig(api_prefix="/custom-api", ws_path="/custom-ws")
    server._tokens_configured = lambda: True
    response = await server._handle_meta(None)
    body = json.loads(response.body)
    assert body["api_prefix"] == "/custom-api"
    assert body["interactive_ws_path"] == "/custom-ws"
    assert body["auth_required"] is True


@pytest.mark.asyncio
async def test_cli_attach_selects_admin_token_when_no_api_token_exists(tmp_path):
    from echo_agent.cli.attach_client import resolve_connection

    path = tmp_path / "admin.yaml"
    path.write_text("gateway:\n  auth:\n    admin_tokens: [test-admin-only]\n")
    assert resolve_connection(str(path), str(tmp_path)).token == "test-admin-only"


@pytest.mark.asyncio
@pytest.mark.parametrize("body", [
    {"text": "hello", "platform": {}},
    {"text": "hello", "user_id": []},
    {"text": "hello", "timeout_seconds": "not-a-number"},
    {"text": ["not", "text"]},
    {"text": "hello", "wait": "false"},
    {"text": "hello", "media_urls": [None]},
])
async def test_http_message_rejects_invalid_input_before_admission(tmp_path, body):
    from echo_agent.bus.queue import MessageBus
    from echo_agent.channels.manager import ChannelManager
    from echo_agent.session.manager import SessionManager

    config = Config()
    config.gateway.auth.mode = "open"
    gateway = GatewayServer(
        config.gateway, MessageBus(), ChannelManager(config.channels, MessageBus()),
        SessionManager(tmp_path / "sessions"), tmp_path,
    )
    app = web.Application()
    app.router.add_post("/message", gateway._handle_message)
    async with TestClient(TestServer(app)) as client:
        response = await client.post("/message", json={"timeout_seconds": 1, **body})
        assert response.status == 400
        assert "error" in await response.json()


@pytest.mark.asyncio
async def test_ws_authentication_cannot_change_identity_on_an_existing_socket(tmp_path):
    from echo_agent.bus.queue import MessageBus
    from echo_agent.channels.manager import ChannelManager
    from echo_agent.session.manager import SessionManager

    config = Config()
    config.gateway.auth.mode = "open"
    bus = MessageBus()
    gateway = GatewayServer(
        config.gateway, bus, ChannelManager(config.channels, bus),
        SessionManager(tmp_path / "sessions"), tmp_path,
    )
    app = web.Application()
    app.router.add_get("/ws", gateway._handle_websocket)
    async with TestClient(TestServer(app)) as client:
        async with client.ws_connect("/ws") as ws:
            await ws.send_json({"type": "auth", "platform": "web", "user_id": "first"})
            assert (await ws.receive_json())["type"] == "auth_ok"
            await ws.send_json({"type": "auth", "platform": "web", "user_id": "second"})
            response = await ws.receive_json()
            assert response["type"] == "error"
            assert list(gateway._ws_clients) == ["gateway:ws:first"]


@pytest.mark.asyncio
@pytest.mark.parametrize("endpoint", ["/ws", "/ws/dashboard"])
async def test_browser_ws_without_fetch_metadata_accepts_only_same_origin(tmp_path, endpoint):
    from echo_agent.bus.queue import MessageBus
    from echo_agent.channels.manager import ChannelManager
    from echo_agent.session.manager import SessionManager
    from aiohttp import WSServerHandshakeError

    config = Config()
    config.gateway.auth.mode = "open"
    config.gateway.auth.admin_tokens = ["browser-test"]
    bus = MessageBus()
    gateway = GatewayServer(
        config.gateway, bus, ChannelManager(config.channels, bus),
        SessionManager(tmp_path / "sessions"), tmp_path,
    )
    app = web.Application()
    app.router.add_get("/ws", gateway._handle_websocket)
    app.router.add_get("/ws/dashboard", gateway.dashboard_ws.handle)
    async with TestClient(TestServer(app)) as client:
        origin = str(client.make_url("/")).rstrip("/")
        async with client.ws_connect(endpoint, headers={"Origin": origin}) as ws:
            await ws.send_json({"type": "auth", "token": "browser-test"})
            assert (await ws.receive_json())["type"] == "auth_ok"
        with pytest.raises(WSServerHandshakeError) as rejected:
            await client.ws_connect(endpoint, headers={"Origin": "http://127.0.0.1:1"})
        assert rejected.value.status == 403


@pytest.mark.asyncio
async def test_channel_lifecycle_activates_only_its_saved_configuration(tmp_path):
    from echo_agent.bus.queue import MessageBus
    from echo_agent.channels.manager import ChannelManager
    from echo_agent.gateway.api.channels import ChannelsAPI

    api, runtime, server = config_api(tmp_path)
    server._require_api_token = lambda *args, **kwargs: None
    manager = ChannelManager(runtime.channels, MessageBus())
    server.channel_manager = manager
    channels = ChannelsAPI(server)
    try:
        response = await api.update_config(Request({"channels.cron.enabled": True, "memory.enabled": False}))
        assert response.status == 200
        assert runtime.channels.cron.enabled is False
        listed = json.loads((await channels.list_channels(Request())).body)
        cron = next(item for item in listed["channels"] if item["name"] == "cron")
        assert cron["enabled"] is True
        assert cron["runtime_enabled"] is False
        response = await channels.lifecycle(Request(match_info={"name": "cron", "action": "start"}))
        assert response.status == 200
        assert manager.get_channel("cron").is_running
        assert runtime.channels.cron.enabled is True
        assert runtime.memory.enabled is True
        assert (await api.update_config(Request({"channels.cron.enabled": False}))).status == 200
        assert (await channels.lifecycle(Request(match_info={"name": "cron", "action": "stop"}))).status == 200
        assert not manager.active_channels
        assert runtime.channels.cron.enabled is False
    finally:
        await manager.stop_all()


@pytest.mark.asyncio
async def test_readiness_distinguishes_live_ui_from_runtime_and_persistent_dependencies(tmp_path):
    import aiohttp
    from echo_agent.bus.queue import MessageBus
    from echo_agent.channels.manager import ChannelManager
    from echo_agent.session.manager import SessionManager
    from echo_agent.storage.sqlite import SQLiteBackend

    config = Config()
    config.gateway.port = 0
    config.gateway.api_prefix = "/custom"
    bus = MessageBus()
    storage = SQLiteBackend(tmp_path / "ready.db")
    await storage.initialize()
    agent = SimpleNamespace(config=config, provider=SimpleNamespace(is_stub=True), is_running=True,
                            _storage=storage, _assert_runtime_healthy=lambda: None)
    gateway = GatewayServer(config.gateway, bus, ChannelManager(config.channels, bus),
                            SessionManager(tmp_path / "sessions", storage=storage), tmp_path, agent_loop=agent)
    await bus.start()
    await gateway.start()
    try:
        async with aiohttp.ClientSession() as client:
            url = f"http://127.0.0.1:{gateway.actual_port}/custom"
            async with client.get(f"{url}/health") as response:
                assert response.status == 200
            async with client.get(f"{url}/ready") as response:
                assert response.status == 503
                assert (await response.json())["checks"]["provider"]["ok"] is False
            agent.provider = SimpleNamespace(is_stub=False)
            async with client.get(f"{url}/ready") as response:
                assert response.status == 200
                assert (await response.json())["provider_remote_verified"] is False
            agent.is_running = False
            async with client.get(f"{url}/ready") as response:
                assert response.status == 503
            agent.is_running = True
            def fail_required():
                raise RuntimeError("required extension failed with secret details")
            agent._assert_runtime_healthy = fail_required
            async with client.get(f"{url}/ready") as response:
                assert response.status == 503
                assert "secret details" not in await response.text()
            agent._assert_runtime_healthy = lambda: None
            await storage.close()
            async with client.get(f"{url}/ready") as response:
                assert response.status == 503
                assert (await response.json())["checks"]["storage"]["ok"] is False
    finally:
        await gateway.stop()
        await bus.stop()
        await storage.close()


@pytest.mark.asyncio
async def test_dashboard_ws_respects_http_session_admin_scope_and_redacts_memory_invalidation(tmp_path):
    from echo_agent.bus.queue import MessageBus
    from echo_agent.channels.manager import ChannelManager
    from echo_agent.session.manager import SessionManager

    config = Config()
    config.gateway.auth.api_tokens = ["read-only"]
    config.gateway.auth.admin_tokens = ["admin-only"]
    bus = MessageBus()
    gateway = GatewayServer(config.gateway, bus, ChannelManager(config.channels, bus),
                            SessionManager(tmp_path / "sessions"), tmp_path)
    app = web.Application()
    app.router.add_get("/sessions", gateway._handle_list_sessions)
    app.router.add_get("/ws/dashboard", gateway.dashboard_ws.handle)
    async with TestClient(TestServer(app)) as client:
        assert (await client.get("/sessions", headers={"Authorization": "Bearer read-only"})).status == 403
        async with client.ws_connect("/ws/dashboard") as reader, client.ws_connect("/ws/dashboard") as admin:
            await reader.send_json({"type": "auth", "token": "read-only"})
            await admin.send_json({"type": "auth", "token": "admin-only"})
            assert (await reader.receive_json())["type"] == "auth_ok"
            assert (await admin.receive_json())["type"] == "auth_ok"
            await reader.send_json({"type": "subscribe", "channels": ["sessions", "memory"]})
            response = await reader.receive_json()
            assert response["forbidden"] == ["sessions"]
            await admin.send_json({"type": "subscribe", "channels": ["sessions", "memory"]})
            assert (await admin.receive_json())["type"] == "subscribed"
            memory_payload = {"scope": "other-principal", "entry_id": "private-id"}
            await gateway.dashboard_ws.broadcast("memory_changed", memory_payload)
            assert (await reader.receive_json())["payload"] == {"changed": True}
            assert (await admin.receive_json())["payload"] == memory_payload
            await gateway.dashboard_ws.broadcast("session_turn_updated", {"response_text": "private conversation"})
            assert (await admin.receive_json())["payload"]["response_text"] == "private conversation"
