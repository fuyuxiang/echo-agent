"""``/capabilities`` must report whether the deployment authenticates at all.

The dashboard treated ``!!token`` as "logged in", which broke the officially
supported open / no-token mode (``auth.authenticate_token`` accepts every request
when no token is configured). An empty token is correct there, but Layout bounced
it to /login, Login's ``/stats`` probe succeeded and navigated back to /, and
Layout bounced it again — a redirect loop escapable only by typing a nonsense
non-empty token.

The UI cannot infer this on its own, so the server states it. These tests pin the
field the frontend now depends on; ``web/src/components/Layout.test.tsx`` pins the
consuming side.
"""

from __future__ import annotations

import json
from unittest.mock import AsyncMock, MagicMock

import pytest

from echo_agent.bus.queue import MessageBus
from echo_agent.config.schema import (
    GatewayAuthConfig,
    GatewayConfig,
    GatewaySessionPolicyConfig,
)
from echo_agent.gateway.server import GatewayServer


def _gateway(**auth_kw) -> GatewayServer:
    session_manager = MagicMock()
    session_manager.get_or_create = AsyncMock(return_value=MagicMock(status="active"))
    config = GatewayConfig(
        enabled=True,
        host="127.0.0.1",
        port=19997,
        auth=GatewayAuthConfig(mode="open", **auth_kw),
        session_policy=GatewaySessionPolicyConfig(mode="none"),
    )
    return GatewayServer(
        config=config,
        bus=MessageBus(),
        channel_manager=MagicMock(),
        session_manager=session_manager,
        workspace=MagicMock(),
        agent_loop=MagicMock(),
    )


def _request(token: str = "") -> MagicMock:
    request = MagicMock()
    request.headers = {"X-Echo-Agent-Token": token} if token else {}
    request.query = {}
    return request


def _body(response) -> dict:
    return json.loads(response.body.decode())


@pytest.mark.asyncio
async def test_open_mode_reports_auth_not_required():
    """No token of any kind configured: an empty token is the correct
    credential, and the UI must be told so rather than demanding a login."""
    gw = _gateway()

    payload = _body(await gw._handle_capabilities(_request()))

    assert payload["authRequired"] is False
    # No tokens configured means every caller is effectively admin — unchanged.
    assert payload["admin"] is True


@pytest.mark.asyncio
async def test_api_token_deployment_reports_auth_required():
    gw = _gateway(api_tokens=["s3cret"])

    payload = _body(await gw._handle_capabilities(_request("s3cret")))

    assert payload["authRequired"] is True


@pytest.mark.asyncio
async def test_admin_token_only_deployment_reports_auth_required():
    """admin_tokens alone is still an authenticating deployment. Keying this on
    api_tokens alone is the bug class that previously made such a deployment
    serve read endpoints unauthenticated."""
    gw = _gateway(admin_tokens=["adm"])

    payload = _body(await gw._handle_capabilities(_request("adm")))

    assert payload["authRequired"] is True
    assert payload["admin"] is True


@pytest.mark.asyncio
async def test_api_token_caller_is_not_admin_when_admin_tokens_configured():
    """The pre-existing admin/read split must survive the added field."""
    gw = _gateway(api_tokens=["reader"], admin_tokens=["adm"])

    payload = _body(await gw._handle_capabilities(_request("reader")))

    assert payload["authRequired"] is True
    assert payload["admin"] is False


@pytest.mark.asyncio
async def test_unauthenticated_caller_is_rejected_when_tokens_configured():
    """authRequired must not leak to callers who cannot read: the endpoint keeps
    its api-token guard, so a wrong token gets 401, not a capability report."""
    gw = _gateway(api_tokens=["s3cret"])

    response = await gw._handle_capabilities(_request("wrong"))

    assert response.status == 401
    assert "authRequired" not in _body(response)
