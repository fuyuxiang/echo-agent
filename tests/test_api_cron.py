# tests/test_api_cron.py
import pytest
from unittest.mock import AsyncMock, MagicMock
from aiohttp import web
from aiohttp.test_utils import TestServer, TestClient

from echo_agent.agent.loop import AgentLoop
from echo_agent.gateway.api.cron_api import CronAPI


@pytest.fixture
def mock_server():
    server = MagicMock()
    server._require_api_token = MagicMock(return_value=None)
    # Write endpoints moved to the admin guard; keep both mocked so these
    # tests exercise handler logic rather than authorization.
    server._require_admin_token = MagicMock(return_value=None)
    server.auth = MagicMock()
    server.auth.token_from_headers = MagicMock(return_value="")
    # spec_set=AgentLoop so assigning an attribute the loop does not expose
    # raises AttributeError here — catching contract drift the API would hit.
    server._agent_loop = MagicMock(spec_set=AgentLoop)
    server._agent_loop.scheduler = MagicMock()
    return server


@pytest.fixture
def api(mock_server):
    return CronAPI(mock_server)


@pytest.mark.asyncio
async def test_list_cron_jobs(mock_server, api):
    from echo_agent.scheduler.service import ScheduledJob, TriggerKind
    job = ScheduledJob(id="j1", name="daily_check", trigger=TriggerKind.CRON, cron_expr="0 9 * * *")
    mock_server._agent_loop.scheduler.list_jobs = MagicMock(return_value=[job])

    app = web.Application()
    app.router.add_get("/api/v1/cron", api.list_jobs)
    async with TestClient(TestServer(app)) as client:
        resp = await client.get("/api/v1/cron")
        assert resp.status == 200
        data = await resp.json()
        assert len(data["jobs"]) == 1
        assert data["jobs"][0]["name"] == "daily_check"
        assert data["total"] == 1


@pytest.mark.asyncio
async def test_create_cron_job(mock_server, api):
    from echo_agent.scheduler.service import ScheduledJob, TriggerKind
    created_job = ScheduledJob(id="j_new", name="new_job", trigger=TriggerKind.CRON, cron_expr="*/5 * * * *")
    mock_server._agent_loop.scheduler.add_job = MagicMock(return_value=created_job)

    app = web.Application()
    app.router.add_post("/api/v1/cron", api.create_job)
    async with TestClient(TestServer(app)) as client:
        resp = await client.post("/api/v1/cron", json={
            "name": "new_job", "cron_expr": "*/5 * * * *", "payload": {"command": "hello"}
        })
        assert resp.status == 201
        data = await resp.json()
        assert data["id"] == "j_new"


@pytest.mark.asyncio
async def test_create_cron_job_missing_expr(mock_server, api):
    app = web.Application()
    app.router.add_post("/api/v1/cron", api.create_job)
    async with TestClient(TestServer(app)) as client:
        resp = await client.post("/api/v1/cron", json={"name": "bad_job"})
        assert resp.status == 400


@pytest.mark.asyncio
async def test_delete_cron_job(mock_server, api):
    mock_server._agent_loop.scheduler.remove_job = MagicMock(return_value=True)

    app = web.Application()
    app.router.add_delete("/api/v1/cron/{id}", api.delete_job)
    async with TestClient(TestServer(app)) as client:
        resp = await client.delete("/api/v1/cron/j1")
        assert resp.status == 200
        data = await resp.json()
        assert data["status"] == "deleted"


@pytest.mark.asyncio
async def test_delete_cron_job_not_found(mock_server, api):
    mock_server._agent_loop.scheduler.remove_job = MagicMock(return_value=False)

    app = web.Application()
    app.router.add_delete("/api/v1/cron/{id}", api.delete_job)
    async with TestClient(TestServer(app)) as client:
        resp = await client.delete("/api/v1/cron/nonexistent")
        assert resp.status == 404


@pytest.mark.asyncio
async def test_trigger_cron_job(mock_server, api):
    mock_server._agent_loop.scheduler.trigger_job = AsyncMock(return_value=True)

    app = web.Application()
    app.router.add_post("/api/v1/cron/{id}/trigger", api.trigger_job)
    async with TestClient(TestServer(app)) as client:
        resp = await client.post("/api/v1/cron/j1/trigger")
        assert resp.status == 200
        data = await resp.json()
        assert data["status"] == "triggered"


@pytest.mark.asyncio
async def test_trigger_cron_job_not_found(mock_server, api):
    mock_server._agent_loop.scheduler.trigger_job = AsyncMock(return_value=False)

    app = web.Application()
    app.router.add_post("/api/v1/cron/{id}/trigger", api.trigger_job)
    async with TestClient(TestServer(app)) as client:
        resp = await client.post("/api/v1/cron/nonexistent/trigger")
        assert resp.status == 404


@pytest.mark.asyncio
async def test_update_cron_job(mock_server, api):
    from echo_agent.scheduler.service import ScheduledJob, TriggerKind
    job = ScheduledJob(id="j1", name="old_name", trigger=TriggerKind.CRON, cron_expr="0 9 * * *")
    mock_server._agent_loop.scheduler.get_job = MagicMock(return_value=job)

    def _update(job_id, *, name=None, cron_expr=None, enabled=None, payload=None,
                authorization=None, set_authorization=False):
        if name is not None:
            job.name = name
        if cron_expr is not None:
            job.cron_expr = cron_expr
        if payload is not None:
            job.payload = payload
        return job

    mock_server._agent_loop.scheduler.update_job = MagicMock(side_effect=_update)

    app = web.Application()
    app.router.add_put("/api/v1/cron/{id}", api.update_job)
    async with TestClient(TestServer(app)) as client:
        resp = await client.put("/api/v1/cron/j1", json={"name": "new_name", "cron_expr": "0 10 * * *"})
        assert resp.status == 200
        data = await resp.json()
        assert data["job"]["name"] == "new_name"
        assert data["job"]["cron_expr"] == "0 10 * * *"


@pytest.mark.asyncio
async def test_update_cron_job_not_found(mock_server, api):
    mock_server._agent_loop.scheduler.get_job = MagicMock(return_value=None)

    app = web.Application()
    app.router.add_put("/api/v1/cron/{id}", api.update_job)
    async with TestClient(TestServer(app)) as client:
        resp = await client.put("/api/v1/cron/nonexistent", json={"name": "x"})
        assert resp.status == 404


@pytest.mark.asyncio
async def test_update_merges_payload_keeping_authorization(mock_server, api):
    """PUT 必须合并 payload,不能整体替换。

    delivery.inbound_event_from_job 把**缺失**的 unattended_authorized 读作 True,
    所以整体替换不只是丢字段,而是静默提权:一个显式设为 False 的任务,改个名字或
    表达式就变成允许无人值守执行 EXEC/DANGEROUS。扩展元数据同理必须保留。"""
    from echo_agent.scheduler.service import ScheduledJob, TriggerKind
    job = ScheduledJob(
        id="j1", name="guarded", trigger=TriggerKind.CRON, cron_expr="0 9 * * *",
        payload={
            "command": "run report",
            "unattended_authorized": False,
            "is_group": True,
            "_inspection_tick": 3,
            "custom_ext": {"k": "v"},
        },
    )
    mock_server._agent_loop.scheduler.get_job = MagicMock(return_value=job)
    captured = {}

    def _update(job_id, *, name=None, cron_expr=None, enabled=None, payload=None,
                authorization=None, set_authorization=False):
        captured["payload"] = payload
        return job

    mock_server._agent_loop.scheduler.update_job = MagicMock(side_effect=_update)

    app = web.Application()
    app.router.add_put("/api/v1/cron/{id}", api.update_job)
    async with TestClient(TestServer(app)) as client:
        # 前端编辑表单只管这几个字段,其余不该被它的省略清掉。
        resp = await client.put("/api/v1/cron/j1", json={
            "name": "renamed",
            "payload": {"command": "run report", "deliver_channel": "slack"},
        })
        assert resp.status == 200

    merged = captured["payload"]
    assert merged["unattended_authorized"] is False
    assert merged["is_group"] is True
    assert merged["_inspection_tick"] == 3
    assert merged["custom_ext"] == {"k": "v"}
    assert merged["deliver_channel"] == "slack"


@pytest.mark.asyncio
async def test_update_switching_instruction_key_clears_the_other(mock_server, api):
    """command 与 message 是同一个逻辑槽位的两种写法,不能各自独立合并。

    否则改用另一个键时旧指令仍留在 payload 里,而 fire-time 优先读 command,
    就会执行用户以为已经替换掉的文本。"""
    from echo_agent.scheduler.service import ScheduledJob, TriggerKind
    job = ScheduledJob(
        id="j1", name="j", trigger=TriggerKind.CRON, cron_expr="0 9 * * *",
        payload={"message": "old instruction", "unattended_authorized": False},
    )
    mock_server._agent_loop.scheduler.get_job = MagicMock(return_value=job)
    captured = {}

    def _update(job_id, *, name=None, cron_expr=None, enabled=None, payload=None,
                authorization=None, set_authorization=False):
        captured["payload"] = payload
        return job

    mock_server._agent_loop.scheduler.update_job = MagicMock(side_effect=_update)

    app = web.Application()
    app.router.add_put("/api/v1/cron/{id}", api.update_job)
    async with TestClient(TestServer(app)) as client:
        resp = await client.put(
            "/api/v1/cron/j1", json={"payload": {"command": "new instruction"}}
        )
        assert resp.status == 200

    merged = captured["payload"]
    assert merged["command"] == "new instruction"
    assert "message" not in merged
    # 换指令键不影响授权标记。
    assert merged["unattended_authorized"] is False


@pytest.mark.asyncio
async def test_update_allows_omitting_instruction_when_stored(mock_server, api):
    """只改投递目标的更新可以不带指令:合并后仍有内容即通过。

    内容校验必须针对合并结果而非请求体,否则这种合法更新会被 400 拒掉。"""
    from echo_agent.scheduler.service import ScheduledJob, TriggerKind
    job = ScheduledJob(
        id="j1", name="j", trigger=TriggerKind.CRON, cron_expr="0 9 * * *",
        payload={"command": "keep me"},
    )
    mock_server._agent_loop.scheduler.get_job = MagicMock(return_value=job)
    captured = {}

    def _update(job_id, *, name=None, cron_expr=None, enabled=None, payload=None,
                authorization=None, set_authorization=False):
        captured["payload"] = payload
        return job

    mock_server._agent_loop.scheduler.update_job = MagicMock(side_effect=_update)

    app = web.Application()
    app.router.add_put("/api/v1/cron/{id}", api.update_job)
    async with TestClient(TestServer(app)) as client:
        resp = await client.put(
            "/api/v1/cron/j1", json={"payload": {"deliver_channel": "feishu"}}
        )
        assert resp.status == 200

    assert captured["payload"]["command"] == "keep me"
    assert captured["payload"]["deliver_channel"] == "feishu"


@pytest.mark.asyncio
async def test_update_cron_job_rejects_clearing_the_instruction(mock_server, api):
    """update 不能把指令清空——否则会重开 create_job 已堵上的 P0 洞
    (fire-time delivery.inbound_event_from_job 抛 ValueError)。

    注意判定对象是**合并后**的 payload:传 {} 现在意为"payload 不变",合并后仍有
    原指令,所以是合法的空操作;真正要拒的是显式把 command/message 置空。"""
    from echo_agent.scheduler.service import ScheduledJob, TriggerKind
    job = ScheduledJob(
        id="j1", name="ok", trigger=TriggerKind.CRON, cron_expr="0 9 * * *",
        payload={"command": "do it"},
    )
    mock_server._agent_loop.scheduler.get_job = MagicMock(return_value=job)
    mock_server._agent_loop.scheduler.update_job = MagicMock(
        side_effect=AssertionError("校验未通过就不该改动任务")
    )

    app = web.Application()
    app.router.add_put("/api/v1/cron/{id}", api.update_job)
    async with TestClient(TestServer(app)) as client:
        resp = await client.put("/api/v1/cron/j1", json={"payload": {"command": "   "}})
        assert resp.status == 400
        data = await resp.json()
        assert "command" in data["error"] or "message" in data["error"]
        # Rejected before mutation: the original payload is untouched.
        assert job.payload == {"command": "do it"}


@pytest.mark.asyncio
async def test_update_rejects_non_object_payload(mock_server, api):
    """payload 必须是对象:合并前先定型,否则一个字符串/数组会静默变成空 dict
    并把已存的指令与授权字段一起丢掉。"""
    from echo_agent.scheduler.service import ScheduledJob, TriggerKind
    job = ScheduledJob(
        id="j1", name="ok", trigger=TriggerKind.CRON, cron_expr="0 9 * * *",
        payload={"command": "do it"},
    )
    mock_server._agent_loop.scheduler.get_job = MagicMock(return_value=job)
    mock_server._agent_loop.scheduler.update_job = MagicMock(
        side_effect=AssertionError("校验未通过就不该改动任务")
    )

    app = web.Application()
    app.router.add_put("/api/v1/cron/{id}", api.update_job)
    async with TestClient(TestServer(app)) as client:
        resp = await client.put("/api/v1/cron/j1", json={"payload": "oops"})
        assert resp.status == 400
        assert job.payload == {"command": "do it"}


@pytest.mark.asyncio
async def test_get_runs(mock_server, api):
    mock_server._agent_loop.scheduler.get_run_history = MagicMock(return_value=[
        {"ts": 1000, "status": "completed"},
        {"ts": 2000, "status": "error"},
    ])

    app = web.Application()
    app.router.add_get("/api/v1/cron/{id}/runs", api.get_runs)
    async with TestClient(TestServer(app)) as client:
        resp = await client.get("/api/v1/cron/j1/runs?limit=5")
        assert resp.status == 200
        data = await resp.json()
        assert len(data["runs"]) == 2


@pytest.mark.asyncio
async def test_create_cron_job_rejects_empty_payload(mock_server, api):
    """缺 command/message 的 Cron 触发时 delivery.inbound_event_from_job 会抛
    ValueError,永远跑不成——创建时就该 400 拦掉,而非默认空 dict 落库。"""
    app = web.Application()
    app.router.add_post("/api/v1/cron", api.create_job)
    async with TestClient(TestServer(app)) as client:
        resp = await client.post("/api/v1/cron", json={"name": "bad", "cron_expr": "*/5 * * * *"})
        assert resp.status == 400
        data = await resp.json()
        assert "command" in data["error"] or "message" in data["error"]


@pytest.mark.asyncio
async def test_create_cron_job_accepts_command_payload(mock_server, api):
    from echo_agent.scheduler.service import ScheduledJob, TriggerKind
    created = ScheduledJob(id="j_ok", name="ok", trigger=TriggerKind.CRON, cron_expr="*/5 * * * *")
    mock_server._agent_loop.scheduler.add_job = MagicMock(return_value=created)

    app = web.Application()
    app.router.add_post("/api/v1/cron", api.create_job)
    async with TestClient(TestServer(app)) as client:
        resp = await client.post("/api/v1/cron", json={
            "name": "ok", "cron_expr": "*/5 * * * *",
            "payload": {"command": "巡检磁盘", "deliver_channel": "cli", "deliver_chat_id": "c1"},
        })
        assert resp.status == 201


@pytest.mark.asyncio
async def test_create_cron_job_accepts_message_only(mock_server, api):
    """message-only payload (no command) must be accepted: delivery reads
    command then falls back to message, so a non-empty message alone is a
    valid trigger. Locks in the command/message fallback contract against a
    regression that would require command specifically."""
    from echo_agent.scheduler.service import ScheduledJob, TriggerKind
    created = ScheduledJob(id="j_msg", name="msg_only", trigger=TriggerKind.CRON, cron_expr="*/5 * * * *")
    mock_server._agent_loop.scheduler.add_job = MagicMock(return_value=created)

    app = web.Application()
    app.router.add_post("/api/v1/cron", api.create_job)
    async with TestClient(TestServer(app)) as client:
        resp = await client.post("/api/v1/cron", json={
            "name": "msg_only", "cron_expr": "*/5 * * * *",
            "payload": {"message": "hi"},
        })
        assert resp.status == 201


@pytest.mark.asyncio
async def test_job_to_dict_marks_config_valid(mock_server, api):
    from echo_agent.scheduler.service import ScheduledJob, TriggerKind
    good = ScheduledJob(id="g", name="g", trigger=TriggerKind.CRON, cron_expr="* * * * *",
                        payload={"command": "x"})
    bad = ScheduledJob(id="b", name="b", trigger=TriggerKind.CRON, cron_expr="* * * * *", payload={})
    mock_server._agent_loop.scheduler.list_jobs = MagicMock(return_value=[good, bad])

    app = web.Application()
    app.router.add_get("/api/v1/cron", api.list_jobs)
    async with TestClient(TestServer(app)) as client:
        resp = await client.get("/api/v1/cron")
        data = await resp.json()
        by_id = {j["id"]: j for j in data["jobs"]}
        assert by_id["g"]["config_valid"] is True
        assert by_id["b"]["config_valid"] is False
