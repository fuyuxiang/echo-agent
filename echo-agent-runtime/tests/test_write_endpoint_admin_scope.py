"""Write-endpoint guardrails must follow token scope.

Reviewer P1-5 said several state-changing endpoints only required a read-scope
api token; P1-6 documented the same across them. After tightening, every
state-changing endpoint must reject a non-admin token when one is configured
*separately*, and the read endpoints must remain reachable by either token
type. Cross-site browser requests are caught by the admin guard's CSRF check;
native clients without Origin/Sec-Fetch-Site headers are unaffected.
"""

from __future__ import annotations

from unittest.mock import AsyncMock, MagicMock

import pytest

from echo_agent.bus.queue import MessageBus
from echo_agent.config.schema import (
    GatewayAuthConfig,
    GatewayConfig,
    GatewaySessionPolicyConfig,
)
from echo_agent.gateway.api.knowledge import KnowledgeAPI
from echo_agent.gateway.api.skills import SkillsAPI
from echo_agent.gateway.api.tasks import TasksAPI
from echo_agent.gateway.server import GatewayServer


def _gateway(api_tokens, admin_tokens):
    session_manager = MagicMock()
    session_manager.get_or_create = AsyncMock(return_value=MagicMock(status="active"))
    config = GatewayConfig(
        enabled=True,
        host="127.0.0.1",
        port=19993,
        auth=GatewayAuthConfig(
            mode="open",
            api_tokens=list(api_tokens),
            admin_tokens=list(admin_tokens),
        ),
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


def _request(token="", body=None):
    req = MagicMock()
    req.headers = {"X-Echo-Agent-Token": token} if token else {}
    # Native client shape: no Origin / Sec-Fetch-Site, so the CSRF primitive
    # treats the call as "not a browser request" and never short-circuits.
    req.headers.setdefault("Origin", "")
    req.headers.setdefault("Sec-Fetch-Site", "")
    req.query = {}
    req.match_info = {}
    # Body the endpoint parses past the guard. Empty body exercises the guard
    # branch (which is what these tests are about) without depending on each
    # handler's specific validation.
    req.json = AsyncMock(return_value=body if body is not None else {})
    req.text = AsyncMock(return_value="")
    req.read = AsyncMock(return_value=b"")
    return req


# ── tasks ─────────────────────────────────────────────────────────────────────


@pytest.mark.asyncio
async def test_tasks_create_rejects_read_token_when_admin_separate():
    gw = _gateway(api_tokens=["reader"], admin_tokens=["admin-tok"])
    api = TasksAPI(gw)

    response = await api.create_task(_request("reader"))

    assert response.status == 403
    assert "admin" in response.text.lower()


@pytest.mark.asyncio
async def test_tasks_create_accepts_admin_token():
    gw = _gateway(api_tokens=["reader"], admin_tokens=["admin-tok"])
    api = TasksAPI(gw)
    gw._agent_loop.task_manager = MagicMock()
    gw._agent_loop.task_manager.create = AsyncMock(return_value=MagicMock(to_dict=lambda: {"id": "t1"}))

    response = await api.create_task(_request("admin-tok", body={"title": "test"}))

    assert response.status == 201


@pytest.mark.asyncio
async def test_tasks_create_accepts_reader_when_no_admin_separate():
    """Single-token deployments retain their existing behavior: the token that
    passes the read guard also passes the admin guard."""
    gw = _gateway(api_tokens=["only"], admin_tokens=[])
    api = TasksAPI(gw)
    gw._agent_loop.task_manager = MagicMock()
    gw._agent_loop.task_manager.create = AsyncMock(return_value=MagicMock(to_dict=lambda: {"id": "t1"}))

    response = await api.create_task(_request("only", body={"title": "test"}))

    assert response.status == 201


@pytest.mark.asyncio
async def test_tasks_list_stays_reachable_to_read_token():
    """Reads do not require admin: a separate admin deployment still serves
    GET /tasks to any holder of the api token."""
    gw = _gateway(api_tokens=["reader"], admin_tokens=["admin-tok"])
    api = TasksAPI(gw)
    gw._agent_loop.task_manager = MagicMock()
    gw._agent_loop.task_manager.list_by_filters = AsyncMock(return_value=[])

    response = await api.list_tasks(_request("reader"))

    assert response.status == 200


@pytest.mark.asyncio
@pytest.mark.parametrize("method_name,body", [
    ("update_task", {"title": "x"}),
    ("transition_task", {"to": "cancelled"}),
])
async def test_tasks_other_writes_reject_reader(method_name, body):
    gw = _gateway(api_tokens=["reader"], admin_tokens=["admin-tok"])
    api = TasksAPI(gw)
    req = _request("reader")
    req.match_info["id"] = "task-1"

    response = await getattr(api, method_name)(req)

    assert response.status == 403, method_name


# ── skills ────────────────────────────────────────────────────────────────────


@pytest.mark.asyncio
async def test_skills_toggle_rejects_read_token_when_admin_separate():
    gw = _gateway(api_tokens=["reader"], admin_tokens=["admin-tok"])
    api = SkillsAPI(gw)
    req = _request("reader")
    req.match_info["name"] = "web search"

    response = await api.toggle_skill(req)

    assert response.status == 403


@pytest.mark.asyncio
async def test_skills_list_stays_reachable_to_read_token():
    gw = _gateway(api_tokens=["reader"], admin_tokens=["admin-tok"])
    api = SkillsAPI(gw)

    response = await api.list_skills(_request("reader"))

    assert response.status == 200


# ── knowledge ─────────────────────────────────────────────────────────────────


@pytest.mark.asyncio
async def test_knowledge_rebuild_rejects_read_token_when_admin_separate():
    gw = _gateway(api_tokens=["reader"], admin_tokens=["admin-tok"])
    api = KnowledgeAPI(gw)
    gw._agent_loop = MagicMock()
    gw._agent_loop.knowledge = MagicMock()

    response = await api.rebuild(_request("reader"))

    assert response.status == 403


@pytest.mark.asyncio
async def test_knowledge_status_stays_reachable_to_read_token():
    gw = _gateway(api_tokens=["reader"], admin_tokens=["admin-tok"])
    api = KnowledgeAPI(gw)
    gw._agent_loop = MagicMock()
    gw._agent_loop.knowledge = MagicMock(status=lambda: {"documents": 0})

    response = await api.get_status(_request("reader"))

    assert response.status == 200