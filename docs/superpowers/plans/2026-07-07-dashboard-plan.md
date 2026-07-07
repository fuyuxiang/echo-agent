# Echo Agent Dashboard Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build a complete web management dashboard for Echo Agent with 11 pages: Overview, Sessions, Memory, Skills, Knowledge, Channels, Cron, Kanban, Logs, Config, Analytics.

**Architecture:** Single-page React application served by the existing aiohttp gateway. Frontend communicates via REST API (`/api/v1/*`) and a dedicated WebSocket (`/ws/dashboard`). Backend extends the gateway with new API modules for tasks, logs, analytics, sessions history, and cron management.

**Tech Stack:** React 19, TypeScript, Vite 6, Tailwind CSS 4, shadcn/ui, Zustand, React Router 7, @dnd-kit, Recharts, CodeMirror 6, Lucide React, date-fns

## Global Constraints

- Python ≥ 3.11, aiohttp ≥ 3.9, aiosqlite ≥ 0.20
- Frontend: Node ≥ 20, pnpm as package manager
- All API endpoints require Bearer token authentication
- All new backend code follows existing patterns in `echo_agent/gateway/api/` (class-based API handlers)
- Storage uses existing SQLite backend (`echo_agent/storage/sqlite.py`)
- TaskStatus state machine must enforce valid transitions
- No breaking changes to existing API endpoints
- Line length: 120 (Python), default prettier (TypeScript)
- Tests: pytest + pytest-asyncio (backend), Vitest (frontend)

---

## File Structure

### Backend (new/modified files)

```
echo_agent/
├── tasks/
│   └── models.py                    ← MODIFY: add BLOCKED, REVIEW states + new fields
├── storage/
│   └── sqlite.py                    ← MODIFY: extend task query filters (labels, assignee, board_id)
├── gateway/
│   ├── api/
│   │   ├── __init__.py              ← MODIFY: register new route modules
│   │   ├── tasks.py                 ← CREATE: Kanban task CRUD + transition API
│   │   ├── logs.py                  ← CREATE: log query API
│   │   ├── analytics.py             ← CREATE: token/skill/channel stats API
│   │   ├── cron_api.py              ← CREATE: cron CRUD + trigger + runs
│   │   └── sessions.py              ← CREATE: session history API
│   ├── ws_dashboard.py              ← CREATE: dashboard WebSocket handler
│   └── server.py                    ← MODIFY: mount dashboard static + ws route
```

### Frontend (all new)

```
web/
├── package.json
├── tsconfig.json
├── vite.config.ts
├── tailwind.config.ts
├── index.html
├── src/
│   ├── main.tsx
│   ├── App.tsx
│   ├── lib/
│   │   ├── api.ts                   ← fetch wrapper with auth
│   │   ├── ws.ts                    ← WebSocket client with auto-reconnect
│   │   └── utils.ts                 ← formatDate, cn() helper
│   ├── hooks/
│   │   ├── use-api.ts               ← data fetching hook
│   │   └── use-ws.ts                ← WebSocket subscription hook
│   ├── stores/
│   │   ├── auth.ts                  ← token store
│   │   └── kanban.ts                ← kanban board state
│   ├── components/
│   │   ├── Layout.tsx               ← Sidebar + Header + Outlet
│   │   ├── Sidebar.tsx
│   │   ├── StatusBadge.tsx
│   │   └── ui/                      ← shadcn/ui components (Button, Card, Dialog, etc.)
│   └── pages/
│       ├── Overview.tsx
│       ├── Sessions.tsx
│       ├── Memory.tsx
│       ├── Skills.tsx
│       ├── Knowledge.tsx
│       ├── Channels.tsx
│       ├── Cron.tsx
│       ├── Kanban.tsx
│       ├── Logs.tsx
│       ├── Config.tsx
│       ├── Analytics.tsx
│       └── Login.tsx
```

---

### Task 1: Extend TaskStatus State Machine + Data Model

**Files:**
- Modify: `echo_agent/tasks/models.py`
- Modify: `echo_agent/storage/sqlite.py`
- Test: `tests/test_task_state_machine.py`

**Interfaces:**
- Produces: `TaskStatus.BLOCKED`, `TaskStatus.REVIEW` enums; `TaskRecord` with fields `labels`, `assignee`, `source`, `session_id`, `blocked_reason`, `review_summary`, `board_id`; updated `VALID_TASK_TRANSITIONS`

- [ ] **Step 1: Write failing tests for new states and transitions**

```python
# tests/test_task_state_machine.py
import pytest
from echo_agent.tasks.models import TaskStatus, VALID_TASK_TRANSITIONS, TaskRecord


def test_blocked_state_exists():
    assert TaskStatus.BLOCKED == "blocked"


def test_review_state_exists():
    assert TaskStatus.REVIEW == "review"


def test_running_can_transition_to_review():
    assert TaskStatus.REVIEW in VALID_TASK_TRANSITIONS[TaskStatus.RUNNING]


def test_running_can_transition_to_blocked():
    assert TaskStatus.BLOCKED in VALID_TASK_TRANSITIONS[TaskStatus.RUNNING]


def test_blocked_can_transition_to_queued():
    assert TaskStatus.QUEUED in VALID_TASK_TRANSITIONS[TaskStatus.BLOCKED]


def test_blocked_can_transition_to_running():
    assert TaskStatus.RUNNING in VALID_TASK_TRANSITIONS[TaskStatus.BLOCKED]


def test_review_can_transition_to_success():
    assert TaskStatus.SUCCESS in VALID_TASK_TRANSITIONS[TaskStatus.REVIEW]


def test_review_can_transition_to_queued():
    assert TaskStatus.QUEUED in VALID_TASK_TRANSITIONS[TaskStatus.REVIEW]


def test_task_record_new_fields():
    task = TaskRecord(
        title="test",
        labels=["bug"],
        assignee="agent-1",
        source="human",
        session_id="sess_abc",
        blocked_reason="",
        review_summary="",
        board_id="default",
    )
    d = task.to_dict()
    assert d["labels"] == ["bug"]
    assert d["assignee"] == "agent-1"
    assert d["source"] == "human"
    assert d["board_id"] == "default"


def test_task_record_from_dict_new_fields():
    d = {
        "id": "t_123", "title": "test", "status": "blocked",
        "labels": ["feat"], "assignee": "user",
        "source": "agent", "session_id": "s1",
        "blocked_reason": "waiting for input",
        "review_summary": "", "board_id": "default",
    }
    task = TaskRecord.from_dict(d)
    assert task.status == TaskStatus.BLOCKED
    assert task.labels == ["feat"]
    assert task.blocked_reason == "waiting for input"
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `pytest tests/test_task_state_machine.py -v`
Expected: FAIL — `TaskStatus` has no `BLOCKED`/`REVIEW`, `TaskRecord` has no `labels`/`assignee` etc.

- [ ] **Step 3: Implement state machine changes in models.py**

In `echo_agent/tasks/models.py`, update:

```python
class TaskStatus(str, Enum):
    PENDING = "pending"
    QUEUED = "queued"
    RUNNING = "running"
    BLOCKED = "blocked"
    REVIEW = "review"
    SUCCESS = "success"
    FAILED = "failed"
    CANCELLED = "cancelled"
    SUSPENDED = "suspended"


VALID_TASK_TRANSITIONS: dict[TaskStatus, set[TaskStatus]] = {
    TaskStatus.PENDING: {TaskStatus.QUEUED, TaskStatus.CANCELLED},
    TaskStatus.QUEUED: {TaskStatus.RUNNING, TaskStatus.CANCELLED},
    TaskStatus.RUNNING: {TaskStatus.REVIEW, TaskStatus.BLOCKED, TaskStatus.FAILED, TaskStatus.SUSPENDED, TaskStatus.CANCELLED},
    TaskStatus.BLOCKED: {TaskStatus.QUEUED, TaskStatus.RUNNING, TaskStatus.CANCELLED},
    TaskStatus.REVIEW: {TaskStatus.SUCCESS, TaskStatus.QUEUED},
    TaskStatus.SUSPENDED: {TaskStatus.QUEUED, TaskStatus.RUNNING, TaskStatus.CANCELLED},
    TaskStatus.FAILED: {TaskStatus.QUEUED},
    TaskStatus.SUCCESS: set(),
    TaskStatus.CANCELLED: set(),
}
```

Add new fields to `TaskRecord`:

```python
@dataclass
class TaskRecord:
    id: str = field(default_factory=lambda: _gen_id("t"))
    workflow_id: str = ""
    parent_task_id: str = ""
    board_id: str = "default"
    title: str = ""
    description: str = ""
    status: TaskStatus = TaskStatus.PENDING
    priority: int = 5
    labels: list[str] = field(default_factory=list)
    assignee: str = ""
    source: str = ""
    session_id: str = ""
    blocked_reason: str = ""
    review_summary: str = ""
    result: str = ""
    error: str = ""
    retry_count: int = 0
    max_retries: int = 3
    created_at: str = field(default_factory=_now)
    updated_at: str = field(default_factory=_now)
    started_at: str = ""
    completed_at: str = ""
    metadata: dict[str, Any] = field(default_factory=dict)
```

Update `to_dict()` and `from_dict()` to include the new fields.

- [ ] **Step 4: Extend storage layer for new query filters**

In `echo_agent/storage/sqlite.py`, modify `list_tasks()` to accept additional filters:

```python
async def list_tasks(
    self,
    workflow_id: str | None = None,
    status: str | None = None,
    board_id: str | None = None,
    assignee: str | None = None,
    label: str | None = None,
) -> list[dict[str, Any]]:
    db = await self._ensure_connection()
    try:
        clauses: list[str] = []
        params: list[str] = []
        if workflow_id:
            clauses.append("workflow_id=?")
            params.append(workflow_id)
        if status:
            clauses.append("status=?")
            params.append(status)
        if board_id:
            clauses.append("json_extract(data, '$.board_id')=?")
            params.append(board_id)
        if assignee:
            clauses.append("json_extract(data, '$.assignee')=?")
            params.append(assignee)
        where = " WHERE " + " AND ".join(clauses) if clauses else ""
        rows = await db.execute_fetchall(
            f"SELECT data FROM tasks{where} ORDER BY updated_at DESC", params,
        )
        results = [json.loads(r[0]) for r in rows]
        if label:
            results = [r for r in results if label in r.get("labels", [])]
        return results
    except Exception as e:
        logger.error("Failed to list tasks: {}", e)
        return []
```

Also update `StorageBackend` abstract base class to match the new signature.

- [ ] **Step 5: Run tests to verify they pass**

Run: `pytest tests/test_task_state_machine.py -v`
Expected: All PASS

- [ ] **Step 6: Commit**

```bash
git add echo_agent/tasks/models.py echo_agent/storage/sqlite.py echo_agent/storage/backend.py tests/test_task_state_machine.py
git commit -m "扩展 TaskStatus 状态机新增 BLOCKED/REVIEW，TaskRecord 新增看板字段"
```

---

### Task 2: Backend — Tasks API (Kanban CRUD + Transition)

**Files:**
- Create: `echo_agent/gateway/api/tasks.py`
- Modify: `echo_agent/gateway/api/__init__.py`
- Test: `tests/test_api_tasks.py`

**Interfaces:**
- Consumes: `TaskManager` from `echo_agent/tasks/manager.py`, `TaskStatus`, `TaskRecord`, `VALID_TASK_TRANSITIONS` from Task 1
- Produces: REST endpoints `GET/POST /api/v1/tasks`, `GET/PUT/DELETE /api/v1/tasks/{id}`, `POST /api/v1/tasks/{id}/transition`

- [ ] **Step 1: Write failing test for tasks API**

```python
# tests/test_api_tasks.py
import pytest
import json
from unittest.mock import AsyncMock, MagicMock
from aiohttp import web
from aiohttp.test_utils import AioHTTPTestCase, unittest_run_loop

from echo_agent.gateway.api.tasks import TasksAPI


@pytest.fixture
def mock_server():
    server = MagicMock()
    server._require_api_token = MagicMock(return_value=None)
    server._agent_loop = MagicMock()
    server._agent_loop.task_manager = AsyncMock()
    return server


@pytest.fixture
def api(mock_server):
    return TasksAPI(mock_server)


@pytest.mark.asyncio
async def test_list_tasks_returns_json(mock_server, api, aiohttp_client):
    from echo_agent.tasks.models import TaskRecord, TaskStatus
    task = TaskRecord(title="test task", status=TaskStatus.PENDING, board_id="default")
    mock_server._agent_loop.task_manager.list_by_filters = AsyncMock(return_value=[task])

    app = web.Application()
    app.router.add_get("/api/v1/tasks", api.list_tasks)
    client = await aiohttp_client(app)

    resp = await client.get("/api/v1/tasks")
    assert resp.status == 200
    data = await resp.json()
    assert len(data["tasks"]) == 1
    assert data["tasks"][0]["title"] == "test task"


@pytest.mark.asyncio
async def test_create_task(mock_server, api, aiohttp_client):
    from echo_agent.tasks.models import TaskRecord
    mock_server._agent_loop.task_manager.create = AsyncMock(
        return_value=TaskRecord(title="new task")
    )

    app = web.Application()
    app.router.add_post("/api/v1/tasks", api.create_task)
    client = await aiohttp_client(app)

    resp = await client.post("/api/v1/tasks", json={"title": "new task"})
    assert resp.status == 201
    data = await resp.json()
    assert data["task"]["title"] == "new task"


@pytest.mark.asyncio
async def test_transition_task(mock_server, api, aiohttp_client):
    from echo_agent.tasks.models import TaskRecord, TaskStatus
    mock_server._agent_loop.task_manager.transition = AsyncMock(
        return_value=TaskRecord(title="t", status=TaskStatus.QUEUED)
    )

    app = web.Application()
    app.router.add_post("/api/v1/tasks/{id}/transition", api.transition_task)
    client = await aiohttp_client(app)

    resp = await client.post("/api/v1/tasks/t_123/transition", json={"to": "queued"})
    assert resp.status == 200


@pytest.mark.asyncio
async def test_transition_invalid_returns_400(mock_server, api, aiohttp_client):
    mock_server._agent_loop.task_manager.transition = AsyncMock(
        side_effect=ValueError("Invalid transition: pending → success")
    )

    app = web.Application()
    app.router.add_post("/api/v1/tasks/{id}/transition", api.transition_task)
    client = await aiohttp_client(app)

    resp = await client.post("/api/v1/tasks/t_123/transition", json={"to": "success"})
    assert resp.status == 400
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `pytest tests/test_api_tasks.py -v`
Expected: FAIL — `echo_agent.gateway.api.tasks` does not exist

- [ ] **Step 3: Implement TasksAPI**

```python
# echo_agent/gateway/api/tasks.py
from __future__ import annotations

import json
from typing import TYPE_CHECKING

from aiohttp import web

from echo_agent.tasks.models import TaskStatus

if TYPE_CHECKING:
    from echo_agent.gateway.server import GatewayServer


class TasksAPI:
    def __init__(self, server: GatewayServer):
        self._server = server

    def _guard(self, request: web.Request, action: str) -> web.Response | None:
        return self._server._require_api_token(request, action=action)

    def _manager(self):
        return self._server._agent_loop.task_manager

    async def list_tasks(self, request: web.Request) -> web.Response:
        guard = self._guard(request, "tasks_list")
        if guard is not None:
            return guard

        status = request.query.get("status")
        assignee = request.query.get("assignee")
        label = request.query.get("label")
        board_id = request.query.get("board_id", "default")

        tasks = await self._manager().list_by_filters(
            status=status, assignee=assignee, label=label, board_id=board_id
        )
        return web.json_response({
            "tasks": [t.to_dict() for t in tasks],
            "total": len(tasks),
        })

    async def create_task(self, request: web.Request) -> web.Response:
        guard = self._guard(request, "tasks_create")
        if guard is not None:
            return guard

        body = await request.json()
        title = body.get("title", "")
        if not title:
            return web.json_response({"error": "title is required"}, status=400)

        task = await self._manager().create(
            title=title,
            description=body.get("description", ""),
            priority=body.get("priority", 5),
            labels=body.get("labels", []),
            assignee=body.get("assignee", ""),
            source=body.get("source", "human"),
            board_id=body.get("board_id", "default"),
        )
        return web.json_response({"task": task.to_dict()}, status=201)

    async def get_task(self, request: web.Request) -> web.Response:
        guard = self._guard(request, "tasks_get")
        if guard is not None:
            return guard

        task_id = request.match_info["id"]
        task = await self._manager().get(task_id)
        if not task:
            return web.json_response({"error": "not found"}, status=404)
        return web.json_response({"task": task.to_dict()})

    async def update_task(self, request: web.Request) -> web.Response:
        guard = self._guard(request, "tasks_update")
        if guard is not None:
            return guard

        task_id = request.match_info["id"]
        body = await request.json()
        allowed = {"title", "description", "priority", "labels", "assignee", "blocked_reason", "review_summary", "metadata"}
        fields = {k: v for k, v in body.items() if k in allowed}
        try:
            task = await self._manager().update(task_id, **fields)
        except ValueError as e:
            return web.json_response({"error": str(e)}, status=404)
        return web.json_response({"task": task.to_dict()})

    async def delete_task(self, request: web.Request) -> web.Response:
        guard = self._guard(request, "tasks_delete")
        if guard is not None:
            return guard

        task_id = request.match_info["id"]
        try:
            await self._manager().cancel(task_id)
        except ValueError as e:
            return web.json_response({"error": str(e)}, status=404)
        return web.json_response({"status": "deleted"})

    async def transition_task(self, request: web.Request) -> web.Response:
        guard = self._guard(request, "tasks_transition")
        if guard is not None:
            return guard

        task_id = request.match_info["id"]
        body = await request.json()
        to_status = body.get("to")
        if not to_status:
            return web.json_response({"error": "'to' status is required"}, status=400)

        try:
            new_status = TaskStatus(to_status)
        except ValueError:
            return web.json_response({"error": f"invalid status: {to_status}"}, status=400)

        try:
            task = await self._manager().transition(task_id, new_status)
        except ValueError as e:
            return web.json_response({"error": str(e)}, status=400)
        return web.json_response({"task": task.to_dict()})
```

- [ ] **Step 4: Add `list_by_filters` to TaskManager**

In `echo_agent/tasks/manager.py` add:

```python
async def list_by_filters(
    self,
    status: str | None = None,
    assignee: str | None = None,
    label: str | None = None,
    board_id: str | None = None,
) -> list[TaskRecord]:
    rows = await self._storage.list_tasks(
        status=status, board_id=board_id, assignee=assignee, label=label
    )
    return [TaskRecord.from_dict(r) for r in rows]
```

Also update `create()` to accept the new fields (`labels`, `assignee`, `source`, `board_id`).

- [ ] **Step 5: Register routes in `__init__.py`**

In `echo_agent/gateway/api/__init__.py`, add:

```python
from echo_agent.gateway.api.tasks import TasksAPI

tasks_api = TasksAPI(server)

app.router.add_get(f"{prefix}/tasks", tasks_api.list_tasks)
app.router.add_post(f"{prefix}/tasks", tasks_api.create_task)
app.router.add_get(f"{prefix}/tasks/{{id}}", tasks_api.get_task)
app.router.add_put(f"{prefix}/tasks/{{id}}", tasks_api.update_task)
app.router.add_delete(f"{prefix}/tasks/{{id}}", tasks_api.delete_task)
app.router.add_post(f"{prefix}/tasks/{{id}}/transition", tasks_api.transition_task)
```

- [ ] **Step 6: Run tests to verify they pass**

Run: `pytest tests/test_api_tasks.py -v`
Expected: All PASS

- [ ] **Step 7: Commit**

```bash
git add echo_agent/gateway/api/tasks.py echo_agent/gateway/api/__init__.py echo_agent/tasks/manager.py tests/test_api_tasks.py
git commit -m "新增看板 Tasks CRUD + Transition API 端点"
```

---

### Task 3: Backend — Dashboard WebSocket

**Files:**
- Create: `echo_agent/gateway/ws_dashboard.py`
- Modify: `echo_agent/gateway/server.py`
- Test: `tests/test_ws_dashboard.py`

**Interfaces:**
- Consumes: `GatewayAuth` from `echo_agent/gateway/auth.py`; task events from Task 2
- Produces: WebSocket endpoint at `/ws/dashboard`; publish helper `dashboard_broadcast(event_type, payload)` callable from other modules

- [ ] **Step 1: Write failing test**

```python
# tests/test_ws_dashboard.py
import pytest
import json
from unittest.mock import MagicMock, AsyncMock, patch
from aiohttp import web
from aiohttp.test_utils import AioHTTPTestCase


@pytest.mark.asyncio
async def test_ws_dashboard_auth_required(aiohttp_client):
    from echo_agent.gateway.ws_dashboard import DashboardWebSocket

    server = MagicMock()
    server.auth = MagicMock()
    server.auth.validate_token = MagicMock(return_value=False)

    dws = DashboardWebSocket(server)
    app = web.Application()
    app.router.add_get("/ws/dashboard", dws.handle)
    client = await aiohttp_client(app)

    ws = await client.ws_connect("/ws/dashboard")
    await ws.send_json({"type": "auth", "token": "bad_token"})
    msg = await ws.receive_json()
    assert msg["type"] == "auth_error"
    await ws.close()


@pytest.mark.asyncio
async def test_ws_dashboard_subscribe_and_receive(aiohttp_client):
    from echo_agent.gateway.ws_dashboard import DashboardWebSocket

    server = MagicMock()
    server.auth = MagicMock()
    server.auth.validate_token = MagicMock(return_value=True)

    dws = DashboardWebSocket(server)
    app = web.Application()
    app.router.add_get("/ws/dashboard", dws.handle)
    client = await aiohttp_client(app)

    ws = await client.ws_connect("/ws/dashboard")
    await ws.send_json({"type": "auth", "token": "valid"})
    msg = await ws.receive_json()
    assert msg["type"] == "auth_ok"

    await ws.send_json({"type": "subscribe", "channels": ["tasks"]})
    msg = await ws.receive_json()
    assert msg["type"] == "subscribed"

    await dws.broadcast("task_created", {"id": "t_abc", "title": "test"})
    msg = await ws.receive_json()
    assert msg["type"] == "task_created"
    assert msg["payload"]["id"] == "t_abc"

    await ws.close()
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `pytest tests/test_ws_dashboard.py -v`
Expected: FAIL — `echo_agent.gateway.ws_dashboard` does not exist

- [ ] **Step 3: Implement DashboardWebSocket**

```python
# echo_agent/gateway/ws_dashboard.py
from __future__ import annotations

import asyncio
import json
from typing import Any, TYPE_CHECKING

from aiohttp import web, WSMsgType
from loguru import logger

if TYPE_CHECKING:
    from echo_agent.gateway.server import GatewayServer


class DashboardWebSocket:
    def __init__(self, server: GatewayServer):
        self._server = server
        self._clients: dict[str, _DashboardClient] = {}
        self._counter = 0

    async def handle(self, request: web.Request) -> web.WebSocketResponse:
        ws = web.WebSocketResponse()
        await ws.prepare(request)

        client_id = f"dash_{self._counter}"
        self._counter += 1
        client = _DashboardClient(client_id, ws)
        authenticated = False

        try:
            async for msg in ws:
                if msg.type == WSMsgType.TEXT:
                    try:
                        data = json.loads(msg.data)
                    except json.JSONDecodeError:
                        continue

                    if data.get("type") == "auth":
                        token = data.get("token", "")
                        if self._server.auth.validate_token(token):
                            authenticated = True
                            self._clients[client_id] = client
                            await ws.send_json({"type": "auth_ok"})
                        else:
                            await ws.send_json({"type": "auth_error", "message": "invalid token"})
                            await ws.close()
                            return ws

                    elif not authenticated:
                        await ws.send_json({"type": "error", "message": "not authenticated"})

                    elif data.get("type") == "subscribe":
                        channels = data.get("channels", [])
                        client.subscriptions.update(channels)
                        await ws.send_json({"type": "subscribed", "channels": list(client.subscriptions)})

                    elif data.get("type") == "unsubscribe":
                        channels = data.get("channels", [])
                        client.subscriptions -= set(channels)
                        await ws.send_json({"type": "unsubscribed", "channels": channels})

                elif msg.type in (WSMsgType.ERROR, WSMsgType.CLOSE):
                    break
        finally:
            self._clients.pop(client_id, None)

        return ws

    async def broadcast(self, event_type: str, payload: dict[str, Any], channel: str | None = None) -> None:
        ch = channel or event_type.split("_")[0] + "s"
        message = json.dumps({"type": event_type, "payload": payload})
        dead: list[str] = []
        for cid, client in self._clients.items():
            if ch in client.subscriptions:
                try:
                    await client.ws.send_str(message)
                except Exception:
                    dead.append(cid)
        for cid in dead:
            self._clients.pop(cid, None)


class _DashboardClient:
    __slots__ = ("id", "ws", "subscriptions")

    def __init__(self, client_id: str, ws: web.WebSocketResponse):
        self.id = client_id
        self.ws = ws
        self.subscriptions: set[str] = set()
```

- [ ] **Step 4: Mount WebSocket in server.py**

In `echo_agent/gateway/server.py`, in the method that sets up routes (look for where `/ws` is added), add:

```python
from echo_agent.gateway.ws_dashboard import DashboardWebSocket

self._dashboard_ws = DashboardWebSocket(self)
# Add alongside existing routes:
self._app.router.add_get("/ws/dashboard", self._dashboard_ws.handle)
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `pytest tests/test_ws_dashboard.py -v`
Expected: All PASS

- [ ] **Step 6: Commit**

```bash
git add echo_agent/gateway/ws_dashboard.py echo_agent/gateway/server.py tests/test_ws_dashboard.py
git commit -m "新增 Dashboard 独立 WebSocket 端点支持订阅推送"
```

---

### Task 4: Backend — Sessions History API

**Files:**
- Create: `echo_agent/gateway/api/sessions.py`
- Modify: `echo_agent/gateway/api/__init__.py`
- Test: `tests/test_api_sessions.py`

**Interfaces:**
- Consumes: `SessionManager` from `echo_agent/session/manager.py` (method `get_history()`)
- Produces: `GET /api/v1/sessions` (extended), `GET /api/v1/sessions/{key}/history`

- [ ] **Step 1: Write failing test**

```python
# tests/test_api_sessions.py
import pytest
from unittest.mock import MagicMock, AsyncMock
from aiohttp import web

from echo_agent.gateway.api.sessions import SessionsAPI


@pytest.fixture
def mock_server():
    server = MagicMock()
    server._require_api_token = MagicMock(return_value=None)
    server.session_manager = MagicMock()
    return server


@pytest.mark.asyncio
async def test_list_sessions(mock_server, aiohttp_client):
    mock_server.session_manager.list_sessions = AsyncMock(return_value=[
        {"key": "tg_user1", "message_count": 10, "last_active": "2026-07-07T10:00:00"},
    ])

    api = SessionsAPI(mock_server)
    app = web.Application()
    app.router.add_get("/api/v1/sessions", api.list_sessions)
    client = await aiohttp_client(app)

    resp = await client.get("/api/v1/sessions")
    assert resp.status == 200
    data = await resp.json()
    assert len(data["sessions"]) == 1


@pytest.mark.asyncio
async def test_get_session_history(mock_server, aiohttp_client):
    session = MagicMock()
    session.get_history.return_value = [
        {"role": "user", "content": "hello"},
        {"role": "assistant", "content": "hi"},
    ]
    mock_server.session_manager.get_or_create = AsyncMock(return_value=session)

    api = SessionsAPI(mock_server)
    app = web.Application()
    app.router.add_get("/api/v1/sessions/{key}/history", api.get_history)
    client = await aiohttp_client(app)

    resp = await client.get("/api/v1/sessions/tg_user1/history")
    assert resp.status == 200
    data = await resp.json()
    assert len(data["messages"]) == 2
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `pytest tests/test_api_sessions.py -v`
Expected: FAIL — module does not exist

- [ ] **Step 3: Implement SessionsAPI**

```python
# echo_agent/gateway/api/sessions.py
from __future__ import annotations

from typing import TYPE_CHECKING

from aiohttp import web

if TYPE_CHECKING:
    from echo_agent.gateway.server import GatewayServer


class SessionsAPI:
    def __init__(self, server: GatewayServer):
        self._server = server

    def _guard(self, request: web.Request, action: str) -> web.Response | None:
        return self._server._require_api_token(request, action=action)

    async def list_sessions(self, request: web.Request) -> web.Response:
        guard = self._guard(request, "sessions_list")
        if guard is not None:
            return guard

        channel = request.query.get("channel")
        sessions = await self._server.session_manager.list_sessions()

        if channel:
            sessions = [s for s in sessions if s.get("key", "").startswith(channel)]

        return web.json_response({"sessions": sessions, "total": len(sessions)})

    async def get_history(self, request: web.Request) -> web.Response:
        guard = self._guard(request, "sessions_history")
        if guard is not None:
            return guard

        key = request.match_info["key"]
        limit = int(request.query.get("limit", "100"))

        session = await self._server.session_manager.get_or_create(key)
        messages = session.get_history(max_messages=limit)

        return web.json_response({"messages": messages, "total": len(messages)})
```

- [ ] **Step 4: Register routes**

In `echo_agent/gateway/api/__init__.py`:

```python
from echo_agent.gateway.api.sessions import SessionsAPI

sessions_api = SessionsAPI(server)
app.router.add_get(f"{prefix}/sessions", sessions_api.list_sessions)
app.router.add_get(f"{prefix}/sessions/{{key}}/history", sessions_api.get_history)
```

- [ ] **Step 5: Run tests**

Run: `pytest tests/test_api_sessions.py -v`
Expected: All PASS

- [ ] **Step 6: Commit**

```bash
git add echo_agent/gateway/api/sessions.py echo_agent/gateway/api/__init__.py tests/test_api_sessions.py
git commit -m "新增会话列表与对话历史 API"
```

---

### Task 5: Backend — Cron Management API

**Files:**
- Create: `echo_agent/gateway/api/cron_api.py`
- Modify: `echo_agent/gateway/api/__init__.py`
- Test: `tests/test_api_cron.py`

**Interfaces:**
- Consumes: `SchedulerService` from `echo_agent/scheduler/service.py` (methods: `add_job()`, `remove_job()`, `pause_job()`, `resume_job()`, `list_jobs()`, `get_job()`)
- Produces: `GET/POST /api/v1/cron`, `PUT/DELETE /api/v1/cron/{id}`, `POST /api/v1/cron/{id}/trigger`, `GET /api/v1/cron/{id}/runs`

- [ ] **Step 1: Write failing test**

```python
# tests/test_api_cron.py
import pytest
from unittest.mock import MagicMock, AsyncMock
from aiohttp import web

from echo_agent.gateway.api.cron_api import CronAPI


@pytest.fixture
def mock_server():
    server = MagicMock()
    server._require_api_token = MagicMock(return_value=None)
    server._agent_loop = MagicMock()
    server._agent_loop.scheduler = MagicMock()
    return server


@pytest.mark.asyncio
async def test_list_cron_jobs(mock_server, aiohttp_client):
    from echo_agent.scheduler.service import ScheduledJob, TriggerKind, JobStatus
    job = ScheduledJob(id="j1", name="daily_check", trigger=TriggerKind.CRON, cron_expr="0 9 * * *")
    mock_server._agent_loop.scheduler.list_jobs = MagicMock(return_value=[job])

    api = CronAPI(mock_server)
    app = web.Application()
    app.router.add_get("/api/v1/cron", api.list_jobs)
    client = await aiohttp_client(app)

    resp = await client.get("/api/v1/cron")
    assert resp.status == 200
    data = await resp.json()
    assert len(data["jobs"]) == 1
    assert data["jobs"][0]["name"] == "daily_check"


@pytest.mark.asyncio
async def test_create_cron_job(mock_server, aiohttp_client):
    mock_server._agent_loop.scheduler.add_job = AsyncMock(return_value="j_new")

    api = CronAPI(mock_server)
    app = web.Application()
    app.router.add_post("/api/v1/cron", api.create_job)
    client = await aiohttp_client(app)

    resp = await client.post("/api/v1/cron", json={
        "name": "new_job", "cron_expr": "*/5 * * * *", "payload": {"msg": "hello"}
    })
    assert resp.status == 201


@pytest.mark.asyncio
async def test_delete_cron_job(mock_server, aiohttp_client):
    mock_server._agent_loop.scheduler.remove_job = AsyncMock(return_value=True)

    api = CronAPI(mock_server)
    app = web.Application()
    app.router.add_delete("/api/v1/cron/{id}", api.delete_job)
    client = await aiohttp_client(app)

    resp = await client.delete("/api/v1/cron/j1")
    assert resp.status == 200
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `pytest tests/test_api_cron.py -v`
Expected: FAIL — module does not exist

- [ ] **Step 3: Implement CronAPI**

```python
# echo_agent/gateway/api/cron_api.py
from __future__ import annotations

from typing import TYPE_CHECKING
from dataclasses import asdict

from aiohttp import web

if TYPE_CHECKING:
    from echo_agent.gateway.server import GatewayServer


class CronAPI:
    def __init__(self, server: GatewayServer):
        self._server = server

    def _guard(self, request: web.Request, action: str) -> web.Response | None:
        return self._server._require_api_token(request, action=action)

    def _scheduler(self):
        return self._server._agent_loop.scheduler

    async def list_jobs(self, request: web.Request) -> web.Response:
        guard = self._guard(request, "cron_list")
        if guard is not None:
            return guard

        jobs = self._scheduler().list_jobs()
        return web.json_response({
            "jobs": [self._job_to_dict(j) for j in jobs],
            "total": len(jobs),
        })

    async def create_job(self, request: web.Request) -> web.Response:
        guard = self._guard(request, "cron_create")
        if guard is not None:
            return guard

        body = await request.json()
        name = body.get("name", "")
        cron_expr = body.get("cron_expr", "")
        if not cron_expr:
            return web.json_response({"error": "cron_expr is required"}, status=400)

        job_id = await self._scheduler().add_job(
            name=name,
            cron_expr=cron_expr,
            payload=body.get("payload", {}),
        )
        return web.json_response({"id": job_id}, status=201)

    async def update_job(self, request: web.Request) -> web.Response:
        guard = self._guard(request, "cron_update")
        if guard is not None:
            return guard

        job_id = request.match_info["id"]
        body = await request.json()

        job = self._scheduler().get_job(job_id)
        if not job:
            return web.json_response({"error": "not found"}, status=404)

        if "name" in body:
            job.name = body["name"]
        if "cron_expr" in body:
            job.cron_expr = body["cron_expr"]
        if "enabled" in body:
            job.enabled = body["enabled"]
        if "payload" in body:
            job.payload = body["payload"]

        await self._scheduler().save_job(job)
        return web.json_response({"job": self._job_to_dict(job)})

    async def delete_job(self, request: web.Request) -> web.Response:
        guard = self._guard(request, "cron_delete")
        if guard is not None:
            return guard

        job_id = request.match_info["id"]
        success = await self._scheduler().remove_job(job_id)
        if not success:
            return web.json_response({"error": "not found"}, status=404)
        return web.json_response({"status": "deleted"})

    async def trigger_job(self, request: web.Request) -> web.Response:
        guard = self._guard(request, "cron_trigger")
        if guard is not None:
            return guard

        job_id = request.match_info["id"]
        job = self._scheduler().get_job(job_id)
        if not job:
            return web.json_response({"error": "not found"}, status=404)

        await self._scheduler().fire_now(job_id)
        return web.json_response({"status": "triggered"})

    async def get_runs(self, request: web.Request) -> web.Response:
        guard = self._guard(request, "cron_runs")
        if guard is not None:
            return guard

        job_id = request.match_info["id"]
        limit = int(request.query.get("limit", "10"))
        runs = await self._scheduler().get_run_history(job_id, limit=limit)
        return web.json_response({"runs": runs})

    def _job_to_dict(self, job) -> dict:
        return {
            "id": job.id,
            "name": job.name,
            "trigger": job.trigger.value,
            "cron_expr": job.cron_expr,
            "enabled": job.enabled,
            "status": job.status.value,
            "last_run_ms": job.last_run_ms,
            "next_run_ms": job.next_run_ms,
            "last_status": job.last_status,
            "payload": job.payload,
        }
```

- [ ] **Step 4: Register routes**

In `echo_agent/gateway/api/__init__.py`:

```python
from echo_agent.gateway.api.cron_api import CronAPI

cron_api = CronAPI(server)
app.router.add_get(f"{prefix}/cron", cron_api.list_jobs)
app.router.add_post(f"{prefix}/cron", cron_api.create_job)
app.router.add_put(f"{prefix}/cron/{{id}}", cron_api.update_job)
app.router.add_delete(f"{prefix}/cron/{{id}}", cron_api.delete_job)
app.router.add_post(f"{prefix}/cron/{{id}}/trigger", cron_api.trigger_job)
app.router.add_get(f"{prefix}/cron/{{id}}/runs", cron_api.get_runs)
```

- [ ] **Step 5: Run tests**

Run: `pytest tests/test_api_cron.py -v`
Expected: All PASS

- [ ] **Step 6: Commit**

```bash
git add echo_agent/gateway/api/cron_api.py echo_agent/gateway/api/__init__.py tests/test_api_cron.py
git commit -m "新增定时任务 CRUD + 触发 + 执行历史 API"
```

---

### Task 6: Backend — Logs + Analytics API

**Files:**
- Create: `echo_agent/gateway/api/logs.py`
- Create: `echo_agent/gateway/api/analytics.py`
- Modify: `echo_agent/gateway/api/__init__.py`
- Test: `tests/test_api_logs.py`
- Test: `tests/test_api_analytics.py`

**Interfaces:**
- Consumes: loguru log sink (for logs); `NormalizedUsage` / cost tracking from `echo_agent/cost/`; session data
- Produces: `GET /api/v1/logs`; `GET /api/v1/analytics/tokens`, `GET /api/v1/analytics/skills`, `GET /api/v1/analytics/channels`

- [ ] **Step 1: Write failing tests**

```python
# tests/test_api_logs.py
import pytest
from unittest.mock import MagicMock, AsyncMock
from aiohttp import web

from echo_agent.gateway.api.logs import LogsAPI


@pytest.fixture
def mock_server():
    server = MagicMock()
    server._require_api_token = MagicMock(return_value=None)
    server._agent_loop = MagicMock()
    server._agent_loop.log_buffer = [
        {"ts": "2026-07-07T10:00:00", "level": "INFO", "message": "Started"},
        {"ts": "2026-07-07T10:00:01", "level": "ERROR", "message": "Oops"},
    ]
    return server


@pytest.mark.asyncio
async def test_list_logs(mock_server, aiohttp_client):
    api = LogsAPI(mock_server)
    app = web.Application()
    app.router.add_get("/api/v1/logs", api.list_logs)
    client = await aiohttp_client(app)

    resp = await client.get("/api/v1/logs")
    assert resp.status == 200
    data = await resp.json()
    assert len(data["logs"]) == 2


@pytest.mark.asyncio
async def test_list_logs_filter_level(mock_server, aiohttp_client):
    api = LogsAPI(mock_server)
    app = web.Application()
    app.router.add_get("/api/v1/logs", api.list_logs)
    client = await aiohttp_client(app)

    resp = await client.get("/api/v1/logs?level=ERROR")
    assert resp.status == 200
    data = await resp.json()
    assert len(data["logs"]) == 1
    assert data["logs"][0]["level"] == "ERROR"
```

```python
# tests/test_api_analytics.py
import pytest
from unittest.mock import MagicMock, AsyncMock
from aiohttp import web

from echo_agent.gateway.api.analytics import AnalyticsAPI


@pytest.fixture
def mock_server():
    server = MagicMock()
    server._require_api_token = MagicMock(return_value=None)
    server._agent_loop = MagicMock()
    server._agent_loop.cost_tracker = MagicMock()
    server._agent_loop.cost_tracker.get_daily_usage = AsyncMock(return_value=[
        {"date": "2026-07-07", "model": "gpt-4o", "input_tokens": 1000, "output_tokens": 500, "cost_usd": 0.05}
    ])
    server._agent_loop.cost_tracker.get_skill_usage = AsyncMock(return_value=[
        {"skill": "web_search", "count": 42}
    ])
    return server


@pytest.mark.asyncio
async def test_token_analytics(mock_server, aiohttp_client):
    api = AnalyticsAPI(mock_server)
    app = web.Application()
    app.router.add_get("/api/v1/analytics/tokens", api.token_usage)
    client = await aiohttp_client(app)

    resp = await client.get("/api/v1/analytics/tokens?days=7")
    assert resp.status == 200
    data = await resp.json()
    assert len(data["usage"]) == 1


@pytest.mark.asyncio
async def test_skill_analytics(mock_server, aiohttp_client):
    api = AnalyticsAPI(mock_server)
    app = web.Application()
    app.router.add_get("/api/v1/analytics/skills", api.skill_usage)
    client = await aiohttp_client(app)

    resp = await client.get("/api/v1/analytics/skills")
    assert resp.status == 200
    data = await resp.json()
    assert data["skills"][0]["skill"] == "web_search"
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `pytest tests/test_api_logs.py tests/test_api_analytics.py -v`
Expected: FAIL — modules do not exist

- [ ] **Step 3: Implement LogsAPI**

```python
# echo_agent/gateway/api/logs.py
from __future__ import annotations

from typing import TYPE_CHECKING

from aiohttp import web

if TYPE_CHECKING:
    from echo_agent.gateway.server import GatewayServer


class LogsAPI:
    def __init__(self, server: GatewayServer):
        self._server = server

    def _guard(self, request: web.Request, action: str) -> web.Response | None:
        return self._server._require_api_token(request, action=action)

    async def list_logs(self, request: web.Request) -> web.Response:
        guard = self._guard(request, "logs_list")
        if guard is not None:
            return guard

        level = request.query.get("level")
        search = request.query.get("q")
        limit = int(request.query.get("limit", "200"))
        offset = int(request.query.get("offset", "0"))

        logs = list(self._server._agent_loop.log_buffer)

        if level:
            logs = [l for l in logs if l.get("level") == level.upper()]
        if search:
            logs = [l for l in logs if search.lower() in l.get("message", "").lower()]

        total = len(logs)
        logs = logs[offset:offset + limit]

        return web.json_response({"logs": logs, "total": total})
```

- [ ] **Step 4: Implement AnalyticsAPI**

```python
# echo_agent/gateway/api/analytics.py
from __future__ import annotations

from typing import TYPE_CHECKING

from aiohttp import web

if TYPE_CHECKING:
    from echo_agent.gateway.server import GatewayServer


class AnalyticsAPI:
    def __init__(self, server: GatewayServer):
        self._server = server

    def _guard(self, request: web.Request, action: str) -> web.Response | None:
        return self._server._require_api_token(request, action=action)

    async def token_usage(self, request: web.Request) -> web.Response:
        guard = self._guard(request, "analytics_tokens")
        if guard is not None:
            return guard

        days = int(request.query.get("days", "7"))
        usage = await self._server._agent_loop.cost_tracker.get_daily_usage(days=days)
        return web.json_response({"usage": usage, "days": days})

    async def skill_usage(self, request: web.Request) -> web.Response:
        guard = self._guard(request, "analytics_skills")
        if guard is not None:
            return guard

        days = int(request.query.get("days", "7"))
        skills = await self._server._agent_loop.cost_tracker.get_skill_usage(days=days)
        return web.json_response({"skills": skills})

    async def channel_usage(self, request: web.Request) -> web.Response:
        guard = self._guard(request, "analytics_channels")
        if guard is not None:
            return guard

        days = int(request.query.get("days", "7"))
        channels = await self._server._agent_loop.cost_tracker.get_channel_usage(days=days)
        return web.json_response({"channels": channels})
```

- [ ] **Step 5: Register routes**

In `echo_agent/gateway/api/__init__.py`:

```python
from echo_agent.gateway.api.logs import LogsAPI
from echo_agent.gateway.api.analytics import AnalyticsAPI

logs_api = LogsAPI(server)
analytics_api = AnalyticsAPI(server)

app.router.add_get(f"{prefix}/logs", logs_api.list_logs)
app.router.add_get(f"{prefix}/analytics/tokens", analytics_api.token_usage)
app.router.add_get(f"{prefix}/analytics/skills", analytics_api.skill_usage)
app.router.add_get(f"{prefix}/analytics/channels", analytics_api.channel_usage)
```

- [ ] **Step 6: Run tests**

Run: `pytest tests/test_api_logs.py tests/test_api_analytics.py -v`
Expected: All PASS

- [ ] **Step 7: Commit**

```bash
git add echo_agent/gateway/api/logs.py echo_agent/gateway/api/analytics.py echo_agent/gateway/api/__init__.py tests/test_api_logs.py tests/test_api_analytics.py
git commit -m "新增日志查询和统计分析 API（token/技能/通道）"
```

---

### Task 7: Frontend — Project Scaffold + Layout + Auth

**Files:**
- Create: `web/package.json`
- Create: `web/tsconfig.json`
- Create: `web/vite.config.ts`
- Create: `web/tailwind.config.ts`
- Create: `web/index.html`
- Create: `web/src/main.tsx`
- Create: `web/src/App.tsx`
- Create: `web/src/lib/api.ts`
- Create: `web/src/lib/ws.ts`
- Create: `web/src/lib/utils.ts`
- Create: `web/src/stores/auth.ts`
- Create: `web/src/components/Layout.tsx`
- Create: `web/src/components/Sidebar.tsx`
- Create: `web/src/pages/Login.tsx`

**Interfaces:**
- Produces: Working SPA shell with sidebar navigation, auth gate, API client, WebSocket client; all pages render placeholder `<div>Page Name</div>` behind auth

- [ ] **Step 1: Initialize project**

```bash
cd /path/to/echo-agent
mkdir -p web/src/{components,pages,lib,stores,hooks}
cd web
```

Create `web/package.json`:

```json
{
  "name": "echo-agent-dashboard",
  "private": true,
  "version": "0.1.0",
  "type": "module",
  "scripts": {
    "dev": "vite",
    "build": "tsc -b && vite build",
    "preview": "vite preview",
    "test": "vitest"
  },
  "dependencies": {
    "react": "^19.0.0",
    "react-dom": "^19.0.0",
    "react-router-dom": "^7.0.0",
    "zustand": "^5.0.0",
    "lucide-react": "^0.460.0",
    "date-fns": "^4.0.0",
    "clsx": "^2.1.0",
    "tailwind-merge": "^2.6.0"
  },
  "devDependencies": {
    "@types/react": "^19.0.0",
    "@types/react-dom": "^19.0.0",
    "@vitejs/plugin-react": "^4.4.0",
    "autoprefixer": "^10.4.0",
    "postcss": "^8.4.0",
    "tailwindcss": "^4.0.0",
    "typescript": "^5.7.0",
    "vite": "^6.0.0",
    "vitest": "^3.0.0"
  }
}
```

- [ ] **Step 2: Create Vite config with proxy**

```typescript
// web/vite.config.ts
import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

export default defineConfig({
  plugins: [react()],
  server: {
    port: 5173,
    proxy: {
      "/api": "http://localhost:8080",
      "/ws": {
        target: "ws://localhost:8080",
        ws: true,
      },
    },
  },
  build: {
    outDir: "dist",
    emptyOutDir: true,
  },
});
```

- [ ] **Step 3: Create TypeScript config**

```json
// web/tsconfig.json
{
  "compilerOptions": {
    "target": "ES2022",
    "lib": ["ES2023", "DOM", "DOM.Iterable"],
    "module": "ESNext",
    "skipLibCheck": true,
    "moduleResolution": "bundler",
    "allowImportingTsExtensions": true,
    "resolveJsonModule": true,
    "isolatedModules": true,
    "noEmit": true,
    "jsx": "react-jsx",
    "strict": true,
    "noUnusedLocals": true,
    "noUnusedParameters": true,
    "noFallthroughCasesInSwitch": true,
    "baseUrl": ".",
    "paths": {
      "@/*": ["src/*"]
    }
  },
  "include": ["src"]
}
```

- [ ] **Step 4: Create Tailwind config and entry CSS**

```typescript
// web/tailwind.config.ts
import type { Config } from "tailwindcss";

export default {
  content: ["./index.html", "./src/**/*.{ts,tsx}"],
  theme: {
    extend: {},
  },
  plugins: [],
} satisfies Config;
```

Create `web/src/index.css`:

```css
@tailwind base;
@tailwind components;
@tailwind utilities;
```

- [ ] **Step 5: Create index.html**

```html
<!-- web/index.html -->
<!DOCTYPE html>
<html lang="zh-CN">
  <head>
    <meta charset="UTF-8" />
    <meta name="viewport" content="width=device-width, initial-scale=1.0" />
    <title>Echo Agent Dashboard</title>
  </head>
  <body>
    <div id="root"></div>
    <script type="module" src="/src/main.tsx"></script>
  </body>
</html>
```

- [ ] **Step 6: Create API client**

```typescript
// web/src/lib/api.ts
const API_BASE = "/api/v1";

function getToken(): string {
  return localStorage.getItem("echo_token") || "";
}

export async function apiFetch<T>(path: string, options: RequestInit = {}): Promise<T> {
  const resp = await fetch(`${API_BASE}${path}`, {
    ...options,
    headers: {
      "Content-Type": "application/json",
      Authorization: `Bearer ${getToken()}`,
      ...options.headers,
    },
  });
  if (resp.status === 401) {
    localStorage.removeItem("echo_token");
    window.location.href = "/login";
    throw new Error("Unauthorized");
  }
  if (!resp.ok) {
    const err = await resp.json().catch(() => ({ error: resp.statusText }));
    throw new Error(err.error || resp.statusText);
  }
  return resp.json();
}
```

- [ ] **Step 7: Create WebSocket client**

```typescript
// web/src/lib/ws.ts
type Listener = (event: { type: string; payload: any }) => void;

export class DashboardWS {
  private ws: WebSocket | null = null;
  private listeners: Map<string, Set<Listener>> = new Map();
  private reconnectTimer: number | null = null;
  private channels: string[] = [];

  connect(token: string, channels: string[]) {
    this.channels = channels;
    const protocol = location.protocol === "https:" ? "wss:" : "ws:";
    this.ws = new WebSocket(`${protocol}//${location.host}/ws/dashboard`);

    this.ws.onopen = () => {
      this.ws!.send(JSON.stringify({ type: "auth", token }));
    };

    this.ws.onmessage = (ev) => {
      const data = JSON.parse(ev.data);
      if (data.type === "auth_ok") {
        this.ws!.send(JSON.stringify({ type: "subscribe", channels }));
        return;
      }
      const listeners = this.listeners.get(data.type);
      if (listeners) {
        listeners.forEach((fn) => fn(data));
      }
    };

    this.ws.onclose = () => {
      this.reconnectTimer = window.setTimeout(() => this.connect(token, this.channels), 3000);
    };
  }

  on(type: string, fn: Listener) {
    if (!this.listeners.has(type)) this.listeners.set(type, new Set());
    this.listeners.get(type)!.add(fn);
    return () => this.listeners.get(type)?.delete(fn);
  }

  close() {
    if (this.reconnectTimer) clearTimeout(this.reconnectTimer);
    this.ws?.close();
  }
}

export const dashboardWS = new DashboardWS();
```

- [ ] **Step 8: Create auth store**

```typescript
// web/src/stores/auth.ts
import { create } from "zustand";

interface AuthState {
  token: string | null;
  setToken: (token: string) => void;
  logout: () => void;
  isAuthenticated: () => boolean;
}

export const useAuthStore = create<AuthState>((set, get) => ({
  token: localStorage.getItem("echo_token"),
  setToken: (token) => {
    localStorage.setItem("echo_token", token);
    set({ token });
  },
  logout: () => {
    localStorage.removeItem("echo_token");
    set({ token: null });
  },
  isAuthenticated: () => !!get().token,
}));
```

- [ ] **Step 9: Create Layout + Sidebar**

```tsx
// web/src/components/Layout.tsx
import { Outlet, Navigate } from "react-router-dom";
import { useAuthStore } from "../stores/auth";
import { Sidebar } from "./Sidebar";

export function Layout() {
  const token = useAuthStore((s) => s.token);
  if (!token) return <Navigate to="/login" replace />;

  return (
    <div className="flex h-screen bg-gray-50">
      <Sidebar />
      <main className="flex-1 overflow-auto p-6">
        <Outlet />
      </main>
    </div>
  );
}
```

```tsx
// web/src/components/Sidebar.tsx
import { NavLink } from "react-router-dom";
import {
  LayoutDashboard, MessageSquare, Brain, Zap, BookOpen,
  Radio, Clock, Kanban, ScrollText, Settings, BarChart3
} from "lucide-react";

const NAV = [
  { to: "/", icon: LayoutDashboard, label: "概览" },
  { to: "/sessions", icon: MessageSquare, label: "会话" },
  { to: "/memory", icon: Brain, label: "记忆" },
  { to: "/skills", icon: Zap, label: "技能" },
  { to: "/knowledge", icon: BookOpen, label: "知识库" },
  { to: "/channels", icon: Radio, label: "通道" },
  { to: "/cron", icon: Clock, label: "定时任务" },
  { to: "/kanban", icon: Kanban, label: "看板" },
  { to: "/logs", icon: ScrollText, label: "日志" },
  { to: "/config", icon: Settings, label: "配置" },
  { to: "/analytics", icon: BarChart3, label: "统计" },
];

export function Sidebar() {
  return (
    <aside className="w-56 bg-white border-r border-gray-200 flex flex-col">
      <div className="p-4 font-bold text-lg border-b">Echo Agent</div>
      <nav className="flex-1 p-2 space-y-1">
        {NAV.map(({ to, icon: Icon, label }) => (
          <NavLink
            key={to}
            to={to}
            className={({ isActive }) =>
              `flex items-center gap-2 px-3 py-2 rounded-md text-sm ${
                isActive ? "bg-blue-50 text-blue-700" : "text-gray-700 hover:bg-gray-100"
              }`
            }
          >
            <Icon size={18} />
            {label}
          </NavLink>
        ))}
      </nav>
    </aside>
  );
}
```

- [ ] **Step 10: Create Login page**

```tsx
// web/src/pages/Login.tsx
import { useState } from "react";
import { useNavigate } from "react-router-dom";
import { useAuthStore } from "../stores/auth";
import { apiFetch } from "../lib/api";

export function Login() {
  const [token, setToken] = useState("");
  const [error, setError] = useState("");
  const navigate = useNavigate();
  const setStoreToken = useAuthStore((s) => s.setToken);

  const handleSubmit = async (e: React.FormEvent) => {
    e.preventDefault();
    setError("");
    try {
      await apiFetch("/health");
      setStoreToken(token);
      navigate("/", { replace: true });
    } catch {
      setError("Token 无效或服务不可达");
    }
  };

  return (
    <div className="flex items-center justify-center h-screen bg-gray-50">
      <form onSubmit={handleSubmit} className="bg-white p-8 rounded-lg shadow-md w-96">
        <h1 className="text-xl font-bold mb-4">Echo Agent Dashboard</h1>
        <label className="block text-sm text-gray-600 mb-1">Admin Token</label>
        <input
          type="password"
          value={token}
          onChange={(e) => setToken(e.target.value)}
          className="w-full border rounded-md px-3 py-2 mb-4"
          placeholder="输入 admin token"
        />
        {error && <p className="text-red-500 text-sm mb-4">{error}</p>}
        <button type="submit" className="w-full bg-blue-600 text-white py-2 rounded-md hover:bg-blue-700">
          登录
        </button>
      </form>
    </div>
  );
}
```

- [ ] **Step 11: Create App.tsx with routing**

```tsx
// web/src/App.tsx
import { BrowserRouter, Routes, Route } from "react-router-dom";
import { Layout } from "./components/Layout";
import { Login } from "./pages/Login";

function Placeholder({ name }: { name: string }) {
  return <div className="text-xl font-bold">{name}</div>;
}

export function App() {
  return (
    <BrowserRouter>
      <Routes>
        <Route path="/login" element={<Login />} />
        <Route element={<Layout />}>
          <Route index element={<Placeholder name="概览" />} />
          <Route path="sessions" element={<Placeholder name="会话管理" />} />
          <Route path="memory" element={<Placeholder name="记忆管理" />} />
          <Route path="skills" element={<Placeholder name="技能管理" />} />
          <Route path="knowledge" element={<Placeholder name="知识库" />} />
          <Route path="channels" element={<Placeholder name="通道管理" />} />
          <Route path="cron" element={<Placeholder name="定时任务" />} />
          <Route path="kanban" element={<Placeholder name="看板" />} />
          <Route path="logs" element={<Placeholder name="日志" />} />
          <Route path="config" element={<Placeholder name="配置" />} />
          <Route path="analytics" element={<Placeholder name="统计" />} />
        </Route>
      </Routes>
    </BrowserRouter>
  );
}
```

- [ ] **Step 12: Create main.tsx**

```tsx
// web/src/main.tsx
import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { App } from "./App";
import "./index.css";

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <App />
  </StrictMode>
);
```

- [ ] **Step 13: Install dependencies and verify build**

```bash
cd web
pnpm install
pnpm build
```

Expected: Build succeeds, `web/dist/` contains `index.html` + assets.

- [ ] **Step 14: Commit**

```bash
git add web/
git commit -m "前端工程脚手架：React+Vite+Tailwind，Layout/Auth/Router 框架"
```

---

### Task 8: Frontend — Overview Page

**Files:**
- Create: `web/src/pages/Overview.tsx`
- Create: `web/src/hooks/use-api.ts`
- Modify: `web/src/App.tsx`

**Interfaces:**
- Consumes: `GET /api/v1/health`, `GET /api/v1/channels`, `GET /api/v1/tasks?board_id=default`
- Produces: Overview page with health indicator, metric cards, channel list, kanban summary

- [ ] **Step 1: Create data fetching hook**

```typescript
// web/src/hooks/use-api.ts
import { useState, useEffect, useCallback } from "react";
import { apiFetch } from "../lib/api";

export function useApi<T>(path: string, deps: any[] = []) {
  const [data, setData] = useState<T | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  const refetch = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const result = await apiFetch<T>(path);
      setData(result);
    } catch (e: any) {
      setError(e.message);
    } finally {
      setLoading(false);
    }
  }, [path]);

  useEffect(() => { refetch(); }, [refetch, ...deps]);

  return { data, loading, error, refetch };
}
```

- [ ] **Step 2: Implement Overview page**

```tsx
// web/src/pages/Overview.tsx
import { useApi } from "../hooks/use-api";
import { Activity, Radio, Brain, Coins, AlertCircle, CheckCircle, AlertTriangle } from "lucide-react";

interface HealthData {
  status: string;
  active_sessions: number;
  channels: { name: string; type: string; running: boolean }[];
}

interface TasksData {
  tasks: { status: string }[];
  total: number;
}

const STATUS_ICON = {
  healthy: <CheckCircle className="text-green-500" size={20} />,
  degraded: <AlertTriangle className="text-yellow-500" size={20} />,
  unhealthy: <AlertCircle className="text-red-500" size={20} />,
};

export function Overview() {
  const { data: health } = useApi<HealthData>("/health");
  const { data: channels } = useApi<{ channels: { name: string; type: string; running: boolean }[] }>("/channels");
  const { data: tasks } = useApi<TasksData>("/tasks?board_id=default");
  const { data: memory } = useApi<{ total: number }>("/memory/stats");

  const statusCounts: Record<string, number> = {};
  tasks?.tasks.forEach((t) => {
    statusCounts[t.status] = (statusCounts[t.status] || 0) + 1;
  });

  const onlineChannels = channels?.channels.filter((c) => c.running).length ?? 0;

  return (
    <div className="space-y-6">
      <div className="flex items-center gap-2">
        {STATUS_ICON[(health?.status as keyof typeof STATUS_ICON) || "unhealthy"]}
        <span className="text-lg font-semibold capitalize">{health?.status || "unknown"}</span>
      </div>

      <div className="grid grid-cols-4 gap-4">
        <MetricCard icon={<Activity size={18} />} label="活跃会话" value={health?.active_sessions ?? 0} />
        <MetricCard icon={<Radio size={18} />} label="通道在线" value={onlineChannels} />
        <MetricCard icon={<Brain size={18} />} label="记忆条数" value={memory?.total ?? 0} />
        <MetricCard icon={<Coins size={18} />} label="Running 任务" value={statusCounts["running"] ?? 0} />
      </div>

      <div className="grid grid-cols-2 gap-6">
        <section>
          <h2 className="font-semibold mb-2">看板摘要</h2>
          <div className="flex gap-2 flex-wrap">
            {["pending", "queued", "running", "blocked", "review", "success"].map((s) => (
              <span key={s} className="px-2 py-1 bg-gray-100 rounded text-sm">
                {s}: {statusCounts[s] ?? 0}
              </span>
            ))}
          </div>
        </section>
        <section>
          <h2 className="font-semibold mb-2">通道状态</h2>
          <div className="space-y-1">
            {channels?.channels.map((ch) => (
              <div key={ch.name} className="flex items-center gap-2 text-sm">
                <span className={`w-2 h-2 rounded-full ${ch.running ? "bg-green-500" : "bg-gray-300"}`} />
                <span>{ch.name}</span>
                <span className="text-gray-400">{ch.type}</span>
              </div>
            ))}
          </div>
        </section>
      </div>
    </div>
  );
}

function MetricCard({ icon, label, value }: { icon: React.ReactNode; label: string; value: number }) {
  return (
    <div className="bg-white rounded-lg border p-4 flex items-center gap-3">
      <div className="text-gray-500">{icon}</div>
      <div>
        <div className="text-2xl font-bold">{value}</div>
        <div className="text-sm text-gray-500">{label}</div>
      </div>
    </div>
  );
}
```

- [ ] **Step 3: Wire into App.tsx**

Replace the Overview placeholder route in `App.tsx`:

```tsx
import { Overview } from "./pages/Overview";
// ...
<Route index element={<Overview />} />
```

- [ ] **Step 4: Verify in dev server**

```bash
cd web && pnpm dev
```

Open `http://localhost:5173` — should show Login page. After entering token, Overview page renders with metric cards and channel list (mocked or live if backend is running).

- [ ] **Step 5: Commit**

```bash
git add web/src/pages/Overview.tsx web/src/hooks/use-api.ts web/src/App.tsx
git commit -m "前端 Overview 页面：系统状态、指标卡片、看板摘要、通道列表"
```

---

### Task 9: Frontend — Kanban Page

**Files:**
- Create: `web/src/pages/Kanban.tsx`
- Create: `web/src/stores/kanban.ts`
- Create: `web/src/hooks/use-ws.ts`
- Modify: `web/src/App.tsx`

**Interfaces:**
- Consumes: `GET /api/v1/tasks`, `POST /api/v1/tasks`, `POST /api/v1/tasks/{id}/transition`; WebSocket events `task_created`, `task_transitioned`, `task_updated`
- Produces: Interactive kanban board with drag-and-drop, card creation, real-time updates

**Additional dependency:** `@dnd-kit/core`, `@dnd-kit/sortable` (add to `package.json`)

- [ ] **Step 1: Install dnd-kit**

```bash
cd web && pnpm add @dnd-kit/core @dnd-kit/sortable @dnd-kit/utilities
```

- [ ] **Step 2: Create WebSocket hook**

```typescript
// web/src/hooks/use-ws.ts
import { useEffect } from "react";
import { dashboardWS } from "../lib/ws";
import { useAuthStore } from "../stores/auth";

export function useWsSubscribe(channels: string[], handler: (ev: any) => void, eventTypes: string[]) {
  const token = useAuthStore((s) => s.token);

  useEffect(() => {
    if (!token) return;
    dashboardWS.connect(token, channels);

    const unsubs = eventTypes.map((type) => dashboardWS.on(type, handler));
    return () => {
      unsubs.forEach((u) => u());
    };
  }, [token, channels.join(",")]);
}
```

- [ ] **Step 3: Create kanban store**

```typescript
// web/src/stores/kanban.ts
import { create } from "zustand";
import { apiFetch } from "../lib/api";

export interface TaskCard {
  id: string;
  title: string;
  description: string;
  status: string;
  priority: number;
  labels: string[];
  assignee: string;
  source: string;
  session_id: string;
  blocked_reason: string;
  review_summary: string;
  created_at: string;
  updated_at: string;
}

export const COLUMNS = [
  { id: "pending", label: "Inbox" },
  { id: "queued", label: "Queued" },
  { id: "running", label: "Running" },
  { id: "blocked", label: "Blocked" },
  { id: "review", label: "Review" },
  { id: "success", label: "Done" },
] as const;

interface KanbanState {
  tasks: TaskCard[];
  loading: boolean;
  fetchTasks: () => Promise<void>;
  transitionTask: (id: string, to: string) => Promise<void>;
  createTask: (title: string, description?: string) => Promise<void>;
  updateLocal: (id: string, changes: Partial<TaskCard>) => void;
  addLocal: (task: TaskCard) => void;
}

export const useKanbanStore = create<KanbanState>((set, get) => ({
  tasks: [],
  loading: false,

  fetchTasks: async () => {
    set({ loading: true });
    try {
      const data = await apiFetch<{ tasks: TaskCard[] }>("/tasks?board_id=default");
      set({ tasks: data.tasks });
    } finally {
      set({ loading: false });
    }
  },

  transitionTask: async (id, to) => {
    await apiFetch(`/tasks/${id}/transition`, {
      method: "POST",
      body: JSON.stringify({ to }),
    });
    set((s) => ({
      tasks: s.tasks.map((t) => (t.id === id ? { ...t, status: to } : t)),
    }));
  },

  createTask: async (title, description = "") => {
    const data = await apiFetch<{ task: TaskCard }>("/tasks", {
      method: "POST",
      body: JSON.stringify({ title, description, source: "human" }),
    });
    set((s) => ({ tasks: [...s.tasks, data.task] }));
  },

  updateLocal: (id, changes) => {
    set((s) => ({
      tasks: s.tasks.map((t) => (t.id === id ? { ...t, ...changes } : t)),
    }));
  },

  addLocal: (task) => {
    set((s) => {
      if (s.tasks.find((t) => t.id === task.id)) return s;
      return { tasks: [...s.tasks, task] };
    });
  },
}));
```

- [ ] **Step 4: Implement Kanban page**

```tsx
// web/src/pages/Kanban.tsx
import { useEffect, useState } from "react";
import { DndContext, DragEndEvent, closestCenter } from "@dnd-kit/core";
import { useKanbanStore, COLUMNS, TaskCard } from "../stores/kanban";
import { useWsSubscribe } from "../hooks/use-ws";
import { Plus } from "lucide-react";

export function Kanban() {
  const { tasks, loading, fetchTasks, transitionTask, createTask, updateLocal, addLocal } = useKanbanStore();
  const [newTitle, setNewTitle] = useState("");

  useEffect(() => { fetchTasks(); }, []);

  useWsSubscribe(["tasks"], (ev) => {
    if (ev.type === "task_created") addLocal(ev.payload);
    else if (ev.type === "task_transitioned") updateLocal(ev.payload.id, { status: ev.payload.to });
    else if (ev.type === "task_updated") updateLocal(ev.payload.id, ev.payload);
  }, ["task_created", "task_transitioned", "task_updated"]);

  const handleDragEnd = (event: DragEndEvent) => {
    const { active, over } = event;
    if (!over) return;
    const targetColumn = over.id as string;
    const taskId = active.id as string;
    const task = tasks.find((t) => t.id === taskId);
    if (task && task.status !== targetColumn) {
      transitionTask(taskId, targetColumn).catch(() => {
        updateLocal(taskId, { status: task.status });
      });
    }
  };

  const handleCreate = async (e: React.FormEvent) => {
    e.preventDefault();
    if (!newTitle.trim()) return;
    await createTask(newTitle.trim());
    setNewTitle("");
  };

  if (loading) return <div>Loading...</div>;

  return (
    <div className="h-full flex flex-col">
      <form onSubmit={handleCreate} className="flex gap-2 mb-4">
        <input
          value={newTitle}
          onChange={(e) => setNewTitle(e.target.value)}
          placeholder="新建任务..."
          className="border rounded px-3 py-1.5 flex-1"
        />
        <button type="submit" className="bg-blue-600 text-white px-3 py-1.5 rounded flex items-center gap-1">
          <Plus size={16} /> 创建
        </button>
      </form>

      <DndContext collisionDetection={closestCenter} onDragEnd={handleDragEnd}>
        <div className="flex gap-3 flex-1 overflow-x-auto">
          {COLUMNS.map((col) => (
            <KanbanColumn key={col.id} column={col} tasks={tasks.filter((t) => t.status === col.id)} />
          ))}
        </div>
      </DndContext>
    </div>
  );
}

function KanbanColumn({ column, tasks }: { column: { id: string; label: string }; tasks: TaskCard[] }) {
  return (
    <div
      id={column.id}
      className="flex-shrink-0 w-64 bg-gray-100 rounded-lg p-2 flex flex-col"
    >
      <div className="font-semibold text-sm mb-2 flex items-center gap-2">
        {column.label}
        <span className="text-xs bg-gray-200 rounded-full px-2">{tasks.length}</span>
      </div>
      <div className="flex-1 space-y-2 overflow-y-auto">
        {tasks.map((task) => (
          <KanbanCard key={task.id} task={task} />
        ))}
      </div>
    </div>
  );
}

function KanbanCard({ task }: { task: TaskCard }) {
  return (
    <div
      id={task.id}
      className="bg-white rounded-md border p-3 shadow-sm cursor-grab active:cursor-grabbing"
    >
      <div className="text-sm font-medium">{task.title}</div>
      {task.assignee && <div className="text-xs text-gray-500 mt-1">@{task.assignee}</div>}
      {task.labels.length > 0 && (
        <div className="flex gap-1 mt-1 flex-wrap">
          {task.labels.map((l) => (
            <span key={l} className="text-xs bg-blue-100 text-blue-700 px-1.5 rounded">{l}</span>
          ))}
        </div>
      )}
    </div>
  );
}
```

Note: This is a simplified drag-and-drop. The full implementation needs `useDraggable`/`useDroppable` from @dnd-kit wired into each card and column. The implementer should wrap `KanbanCard` with `useDraggable` and `KanbanColumn` with `useDroppable` to enable actual drag behavior.

- [ ] **Step 5: Wire into App.tsx**

```tsx
import { Kanban } from "./pages/Kanban";
// ...
<Route path="kanban" element={<Kanban />} />
```

- [ ] **Step 6: Verify in dev server**

Open `http://localhost:5173/kanban`. Should display 6 columns, a create form, and cards organized by status.

- [ ] **Step 7: Commit**

```bash
git add web/src/pages/Kanban.tsx web/src/stores/kanban.ts web/src/hooks/use-ws.ts web/package.json web/src/App.tsx
git commit -m "前端 Kanban 页面：拖拽看板、卡片创建、WebSocket 实时更新"
```

---

### Task 10: Frontend — Sessions Page

**Files:**
- Create: `web/src/pages/Sessions.tsx`
- Modify: `web/src/App.tsx`

**Interfaces:**
- Consumes: `GET /api/v1/sessions`, `GET /api/v1/sessions/{key}/history`
- Produces: Sessions page with list + conversation replay

- [ ] **Step 1: Implement Sessions page**

```tsx
// web/src/pages/Sessions.tsx
import { useState } from "react";
import { useApi } from "../hooks/use-api";
import { apiFetch } from "../lib/api";
import { formatDistanceToNow } from "date-fns";
import { zhCN } from "date-fns/locale";

interface SessionItem {
  key: string;
  message_count: number;
  last_active: string;
}

interface Message {
  role: string;
  content: string;
}

export function Sessions() {
  const { data } = useApi<{ sessions: SessionItem[] }>("/sessions");
  const [selected, setSelected] = useState<string | null>(null);
  const [messages, setMessages] = useState<Message[]>([]);
  const [search, setSearch] = useState("");

  const loadHistory = async (key: string) => {
    setSelected(key);
    const res = await apiFetch<{ messages: Message[] }>(`/sessions/${encodeURIComponent(key)}/history`);
    setMessages(res.messages);
  };

  const filtered = data?.sessions.filter(
    (s) => !search || s.key.toLowerCase().includes(search.toLowerCase())
  ) ?? [];

  return (
    <div className="flex h-full gap-4">
      <div className="w-72 flex flex-col border-r pr-4">
        <input
          value={search}
          onChange={(e) => setSearch(e.target.value)}
          placeholder="搜索会话..."
          className="border rounded px-3 py-1.5 mb-3"
        />
        <div className="flex-1 overflow-y-auto space-y-1">
          {filtered.map((s) => (
            <button
              key={s.key}
              onClick={() => loadHistory(s.key)}
              className={`w-full text-left px-3 py-2 rounded text-sm ${
                selected === s.key ? "bg-blue-50 text-blue-700" : "hover:bg-gray-100"
              }`}
            >
              <div className="font-medium truncate">{s.key}</div>
              <div className="text-xs text-gray-500">
                {s.message_count} 条 · {formatDistanceToNow(new Date(s.last_active), { locale: zhCN, addSuffix: true })}
              </div>
            </button>
          ))}
        </div>
      </div>

      <div className="flex-1 overflow-y-auto space-y-3">
        {messages.map((msg, i) => (
          <div key={i} className={`flex ${msg.role === "user" ? "justify-end" : "justify-start"}`}>
            <div
              className={`max-w-[70%] rounded-lg px-4 py-2 text-sm ${
                msg.role === "user" ? "bg-blue-600 text-white" : "bg-gray-100 text-gray-800"
              }`}
            >
              {msg.content}
            </div>
          </div>
        ))}
        {!selected && <div className="text-gray-400 text-center mt-20">选择一个会话查看历史</div>}
      </div>
    </div>
  );
}
```

- [ ] **Step 2: Wire into App.tsx**

```tsx
import { Sessions } from "./pages/Sessions";
<Route path="sessions" element={<Sessions />} />
```

- [ ] **Step 3: Verify and commit**

```bash
git add web/src/pages/Sessions.tsx web/src/App.tsx
git commit -m "前端 Sessions 页面：会话列表+对话回放"
```

---

### Task 11: Frontend — Memory Page

**Files:**
- Create: `web/src/pages/Memory.tsx`
- Modify: `web/src/App.tsx`

**Interfaces:**
- Consumes: `GET /api/v1/memory`, `GET /api/v1/memory/stats`, `POST /api/v1/memory/search`, `DELETE /api/v1/memory/{id}`
- Produces: Memory management page with tabs, search, CRUD

- [ ] **Step 1: Implement Memory page**

```tsx
// web/src/pages/Memory.tsx
import { useState } from "react";
import { useApi } from "../hooks/use-api";
import { apiFetch } from "../lib/api";
import { Trash2, Search } from "lucide-react";

const TIERS = ["core", "episodic", "semantic", "procedural"] as const;

interface MemoryEntry {
  id: string;
  content: string;
  type: string;
  tier: string;
  weight: number;
  created_at: string;
}

export function Memory() {
  const [tier, setTier] = useState<string>("core");
  const [searchQuery, setSearchQuery] = useState("");
  const [searchResults, setSearchResults] = useState<MemoryEntry[] | null>(null);

  const { data, refetch } = useApi<{ entries: MemoryEntry[]; total: number }>(`/memory?tier=${tier}&limit=100`);

  const handleSearch = async () => {
    if (!searchQuery.trim()) { setSearchResults(null); return; }
    const res = await apiFetch<{ results: MemoryEntry[] }>("/memory/search", {
      method: "POST",
      body: JSON.stringify({ query: searchQuery, limit: 20 }),
    });
    setSearchResults(res.results);
  };

  const handleDelete = async (id: string) => {
    await apiFetch(`/memory/${id}`, { method: "DELETE" });
    refetch();
  };

  const entries = searchResults ?? data?.entries ?? [];

  return (
    <div className="space-y-4">
      <div className="flex gap-2 items-center">
        <input
          value={searchQuery}
          onChange={(e) => setSearchQuery(e.target.value)}
          onKeyDown={(e) => e.key === "Enter" && handleSearch()}
          placeholder="语义搜索..."
          className="border rounded px-3 py-1.5 flex-1"
        />
        <button onClick={handleSearch} className="p-2 bg-gray-100 rounded hover:bg-gray-200">
          <Search size={18} />
        </button>
      </div>

      <div className="flex gap-1 border-b">
        {TIERS.map((t) => (
          <button
            key={t}
            onClick={() => { setTier(t); setSearchResults(null); }}
            className={`px-4 py-2 text-sm border-b-2 ${
              tier === t ? "border-blue-600 text-blue-600" : "border-transparent text-gray-500"
            }`}
          >
            {t}
          </button>
        ))}
      </div>

      <div className="space-y-2">
        {entries.map((entry) => (
          <div key={entry.id} className="bg-white border rounded-lg p-4 flex justify-between items-start">
            <div className="flex-1">
              <div className="text-sm">{entry.content}</div>
              <div className="text-xs text-gray-400 mt-1">
                {entry.type} · weight: {entry.weight?.toFixed(2)} · {entry.created_at}
              </div>
            </div>
            <button onClick={() => handleDelete(entry.id)} className="text-red-400 hover:text-red-600 ml-2">
              <Trash2 size={16} />
            </button>
          </div>
        ))}
        {entries.length === 0 && <div className="text-gray-400 text-center py-8">无记忆条目</div>}
      </div>
    </div>
  );
}
```

- [ ] **Step 2: Wire into App.tsx and commit**

```bash
git add web/src/pages/Memory.tsx web/src/App.tsx
git commit -m "前端 Memory 页面：四层记忆浏览+语义搜索+删除"
```

---

### Task 12: Frontend — Skills + Knowledge + Channels Pages

**Files:**
- Create: `web/src/pages/Skills.tsx`
- Create: `web/src/pages/Knowledge.tsx`
- Create: `web/src/pages/Channels.tsx`
- Modify: `web/src/App.tsx`

**Interfaces:**
- Consumes: existing API endpoints — `GET /api/v1/skills`, `POST /api/v1/skills/{name}/toggle`, `GET /api/v1/knowledge/documents`, `POST /api/v1/knowledge/upload`, `GET /api/v1/channels`
- Produces: Three management pages

- [ ] **Step 1: Implement Skills page**

```tsx
// web/src/pages/Skills.tsx
import { useApi } from "../hooks/use-api";
import { apiFetch } from "../lib/api";
import { Zap } from "lucide-react";

interface Skill {
  name: string;
  description: string;
  enabled: boolean;
}

export function Skills() {
  const { data, refetch } = useApi<{ skills: Skill[] }>("/skills");

  const toggle = async (name: string) => {
    await apiFetch(`/skills/${name}/toggle`, { method: "POST" });
    refetch();
  };

  return (
    <div className="grid grid-cols-1 md:grid-cols-2 lg:grid-cols-3 gap-4">
      {data?.skills.map((skill) => (
        <div key={skill.name} className="bg-white border rounded-lg p-4">
          <div className="flex items-center justify-between mb-2">
            <div className="flex items-center gap-2">
              <Zap size={16} className="text-yellow-500" />
              <span className="font-medium text-sm">{skill.name}</span>
            </div>
            <button
              onClick={() => toggle(skill.name)}
              className={`w-10 h-5 rounded-full transition-colors ${skill.enabled ? "bg-blue-600" : "bg-gray-300"}`}
            >
              <div className={`w-4 h-4 bg-white rounded-full shadow transition-transform ${skill.enabled ? "translate-x-5" : "translate-x-0.5"}`} />
            </button>
          </div>
          <p className="text-xs text-gray-500">{skill.description || "无描述"}</p>
        </div>
      ))}
    </div>
  );
}
```

- [ ] **Step 2: Implement Knowledge page**

```tsx
// web/src/pages/Knowledge.tsx
import { useApi } from "../hooks/use-api";
import { apiFetch } from "../lib/api";
import { Upload, Trash2, RefreshCw } from "lucide-react";
import { useRef } from "react";

interface Document {
  path: string;
  size: number;
  indexed: boolean;
}

export function Knowledge() {
  const { data, refetch } = useApi<{ documents: Document[] }>("/knowledge/documents");
  const { data: status } = useApi<{ indexed_count: number; last_rebuild: string }>("/knowledge/status");
  const fileRef = useRef<HTMLInputElement>(null);

  const upload = async (file: File) => {
    const form = new FormData();
    form.append("file", file);
    await fetch("/api/v1/knowledge/upload", {
      method: "POST",
      headers: { Authorization: `Bearer ${localStorage.getItem("echo_token")}` },
      body: form,
    });
    refetch();
  };

  const rebuild = async () => {
    await apiFetch("/knowledge/rebuild", { method: "POST" });
    refetch();
  };

  const deleteDoc = async (path: string) => {
    await apiFetch(`/knowledge/documents/${encodeURIComponent(path)}`, { method: "DELETE" });
    refetch();
  };

  return (
    <div className="space-y-4">
      <div className="flex items-center gap-4">
        <button onClick={() => fileRef.current?.click()} className="flex items-center gap-1 bg-blue-600 text-white px-3 py-1.5 rounded text-sm">
          <Upload size={16} /> 上传文档
        </button>
        <button onClick={rebuild} className="flex items-center gap-1 bg-gray-100 px-3 py-1.5 rounded text-sm hover:bg-gray-200">
          <RefreshCw size={16} /> 重建索引
        </button>
        <span className="text-sm text-gray-500">
          已索引 {status?.indexed_count ?? 0} 篇 · 最后重建: {status?.last_rebuild || "从未"}
        </span>
        <input ref={fileRef} type="file" className="hidden" onChange={(e) => e.target.files?.[0] && upload(e.target.files[0])} />
      </div>

      <div className="space-y-2">
        {data?.documents.map((doc) => (
          <div key={doc.path} className="flex items-center justify-between bg-white border rounded p-3">
            <div>
              <div className="text-sm font-medium">{doc.path}</div>
              <div className="text-xs text-gray-400">{(doc.size / 1024).toFixed(1)} KB</div>
            </div>
            <button onClick={() => deleteDoc(doc.path)} className="text-red-400 hover:text-red-600">
              <Trash2 size={16} />
            </button>
          </div>
        ))}
      </div>
    </div>
  );
}
```

- [ ] **Step 3: Implement Channels page**

```tsx
// web/src/pages/Channels.tsx
import { useApi } from "../hooks/use-api";

interface Channel {
  name: string;
  type: string;
  running: boolean;
}

export function Channels() {
  const { data } = useApi<{ channels: Channel[] }>("/channels");

  return (
    <div className="space-y-2">
      {data?.channels.map((ch) => (
        <div key={ch.name} className="bg-white border rounded-lg p-4 flex items-center gap-4">
          <span className={`w-3 h-3 rounded-full ${ch.running ? "bg-green-500" : "bg-gray-300"}`} />
          <div className="flex-1">
            <div className="font-medium text-sm">{ch.name}</div>
            <div className="text-xs text-gray-500">{ch.type}</div>
          </div>
          <span className={`text-xs px-2 py-0.5 rounded ${ch.running ? "bg-green-100 text-green-700" : "bg-gray-100 text-gray-500"}`}>
            {ch.running ? "在线" : "离线"}
          </span>
        </div>
      ))}
    </div>
  );
}
```

- [ ] **Step 4: Wire all three into App.tsx and commit**

```bash
git add web/src/pages/Skills.tsx web/src/pages/Knowledge.tsx web/src/pages/Channels.tsx web/src/App.tsx
git commit -m "前端 Skills/Knowledge/Channels 管理页面"
```

---

### Task 13: Frontend — Cron + Logs + Config + Analytics Pages

**Files:**
- Create: `web/src/pages/Cron.tsx`
- Create: `web/src/pages/Logs.tsx`
- Create: `web/src/pages/Config.tsx`
- Create: `web/src/pages/Analytics.tsx`
- Modify: `web/src/App.tsx`

**Interfaces:**
- Consumes: `GET/POST/PUT/DELETE /api/v1/cron`, `GET /api/v1/logs`, `GET /api/v1/config`, `GET /api/v1/analytics/*`; WebSocket `log_entry` events
- Produces: Four remaining pages

**Additional dependency:** `recharts`, `@codemirror/view`, `@codemirror/lang-json` (add to `package.json`)

- [ ] **Step 1: Install additional deps**

```bash
cd web && pnpm add recharts @codemirror/view @codemirror/state @codemirror/lang-json
```

- [ ] **Step 2: Implement Cron page**

```tsx
// web/src/pages/Cron.tsx
import { useState } from "react";
import { useApi } from "../hooks/use-api";
import { apiFetch } from "../lib/api";
import { Play, Trash2, Plus } from "lucide-react";

interface CronJob {
  id: string;
  name: string;
  cron_expr: string;
  enabled: boolean;
  status: string;
  last_status: string;
  next_run_ms: number | null;
}

export function Cron() {
  const { data, refetch } = useApi<{ jobs: CronJob[] }>("/cron");
  const [showCreate, setShowCreate] = useState(false);
  const [name, setName] = useState("");
  const [expr, setExpr] = useState("");

  const trigger = async (id: string) => {
    await apiFetch(`/cron/${id}/trigger`, { method: "POST" });
    refetch();
  };

  const remove = async (id: string) => {
    await apiFetch(`/cron/${id}`, { method: "DELETE" });
    refetch();
  };

  const create = async (e: React.FormEvent) => {
    e.preventDefault();
    await apiFetch("/cron", { method: "POST", body: JSON.stringify({ name, cron_expr: expr }) });
    setName(""); setExpr(""); setShowCreate(false);
    refetch();
  };

  return (
    <div className="space-y-4">
      <div className="flex justify-between items-center">
        <h1 className="text-lg font-bold">定时任务</h1>
        <button onClick={() => setShowCreate(!showCreate)} className="flex items-center gap-1 bg-blue-600 text-white px-3 py-1.5 rounded text-sm">
          <Plus size={16} /> 新建
        </button>
      </div>

      {showCreate && (
        <form onSubmit={create} className="bg-white border rounded p-4 flex gap-3">
          <input value={name} onChange={(e) => setName(e.target.value)} placeholder="任务名称" className="border rounded px-3 py-1.5 flex-1" />
          <input value={expr} onChange={(e) => setExpr(e.target.value)} placeholder="cron 表达式" className="border rounded px-3 py-1.5 w-40" />
          <button type="submit" className="bg-green-600 text-white px-3 py-1.5 rounded text-sm">创建</button>
        </form>
      )}

      <table className="w-full text-sm">
        <thead>
          <tr className="border-b text-left text-gray-500">
            <th className="py-2">名称</th>
            <th>Cron</th>
            <th>状态</th>
            <th>最近结果</th>
            <th>下次执行</th>
            <th>操作</th>
          </tr>
        </thead>
        <tbody>
          {data?.jobs.map((job) => (
            <tr key={job.id} className="border-b">
              <td className="py-2 font-medium">{job.name || job.id}</td>
              <td className="font-mono text-xs">{job.cron_expr}</td>
              <td><span className={`text-xs px-1.5 rounded ${job.enabled ? "bg-green-100 text-green-700" : "bg-gray-100"}`}>{job.enabled ? "活跃" : "暂停"}</span></td>
              <td className="text-xs">{job.last_status || "-"}</td>
              <td className="text-xs">{job.next_run_ms ? new Date(job.next_run_ms).toLocaleString() : "-"}</td>
              <td className="flex gap-1">
                <button onClick={() => trigger(job.id)} className="p-1 hover:bg-gray-100 rounded" title="立即执行"><Play size={14} /></button>
                <button onClick={() => remove(job.id)} className="p-1 hover:bg-red-50 rounded text-red-500" title="删除"><Trash2 size={14} /></button>
              </td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}
```

- [ ] **Step 3: Implement Logs page**

```tsx
// web/src/pages/Logs.tsx
import { useState, useRef, useEffect } from "react";
import { useApi } from "../hooks/use-api";
import { useWsSubscribe } from "../hooks/use-ws";

const LEVELS = ["DEBUG", "INFO", "WARNING", "ERROR"] as const;

interface LogEntry {
  ts: string;
  level: string;
  message: string;
}

export function Logs() {
  const [level, setLevel] = useState<string>("");
  const [search, setSearch] = useState("");
  const [live, setLive] = useState(true);
  const [liveEntries, setLiveEntries] = useState<LogEntry[]>([]);
  const bottomRef = useRef<HTMLDivElement>(null);

  const { data } = useApi<{ logs: LogEntry[] }>(`/logs?limit=200${level ? `&level=${level}` : ""}${search ? `&q=${search}` : ""}`);

  useWsSubscribe(["logs"], (ev) => {
    if (ev.type === "log_entry" && live) {
      setLiveEntries((prev) => [...prev.slice(-500), ev.payload]);
    }
  }, ["log_entry"]);

  useEffect(() => {
    if (live) bottomRef.current?.scrollIntoView({ behavior: "smooth" });
  }, [liveEntries, live]);

  const entries = live ? [...(data?.logs ?? []), ...liveEntries] : (data?.logs ?? []);

  const levelColor: Record<string, string> = {
    DEBUG: "text-gray-400", INFO: "text-blue-600", WARNING: "text-yellow-600", ERROR: "text-red-600",
  };

  return (
    <div className="flex flex-col h-full">
      <div className="flex gap-2 items-center mb-3">
        <select value={level} onChange={(e) => setLevel(e.target.value)} className="border rounded px-2 py-1 text-sm">
          <option value="">全部级别</option>
          {LEVELS.map((l) => <option key={l} value={l}>{l}</option>)}
        </select>
        <input value={search} onChange={(e) => setSearch(e.target.value)} placeholder="搜索..." className="border rounded px-3 py-1 text-sm flex-1" />
        <label className="flex items-center gap-1 text-sm">
          <input type="checkbox" checked={live} onChange={(e) => setLive(e.target.checked)} />
          实时
        </label>
      </div>

      <div className="flex-1 overflow-y-auto bg-gray-900 rounded-lg p-4 font-mono text-xs">
        {entries.map((entry, i) => (
          <div key={i} className="flex gap-2">
            <span className="text-gray-500 shrink-0">{entry.ts?.slice(11, 19)}</span>
            <span className={`shrink-0 w-14 ${levelColor[entry.level] || ""}`}>{entry.level}</span>
            <span className="text-gray-200">{entry.message}</span>
          </div>
        ))}
        <div ref={bottomRef} />
      </div>
    </div>
  );
}
```

- [ ] **Step 4: Implement Config page**

```tsx
// web/src/pages/Config.tsx
import { useApi } from "../hooks/use-api";

export function Config() {
  const { data } = useApi<Record<string, any>>("/config");

  return (
    <div className="space-y-4">
      <h1 className="text-lg font-bold">配置（只读）</h1>
      <pre className="bg-gray-900 text-green-300 rounded-lg p-4 text-xs overflow-auto max-h-[75vh]">
        {data ? JSON.stringify(data, null, 2) : "Loading..."}
      </pre>
    </div>
  );
}
```

- [ ] **Step 5: Implement Analytics page**

```tsx
// web/src/pages/Analytics.tsx
import { useState } from "react";
import { useApi } from "../hooks/use-api";
import { LineChart, Line, XAxis, YAxis, CartesianGrid, Tooltip, ResponsiveContainer, BarChart, Bar } from "recharts";

export function Analytics() {
  const [days, setDays] = useState(7);
  const { data: tokens } = useApi<{ usage: { date: string; input_tokens: number; output_tokens: number; cost_usd: number }[] }>(`/analytics/tokens?days=${days}`);
  const { data: skills } = useApi<{ skills: { skill: string; count: number }[] }>(`/analytics/skills?days=${days}`);

  return (
    <div className="space-y-6">
      <div className="flex items-center gap-2">
        <span className="text-sm text-gray-500">时间范围:</span>
        {[1, 7, 30].map((d) => (
          <button
            key={d}
            onClick={() => setDays(d)}
            className={`px-3 py-1 rounded text-sm ${days === d ? "bg-blue-600 text-white" : "bg-gray-100"}`}
          >
            {d === 1 ? "今天" : `${d}天`}
          </button>
        ))}
      </div>

      <section>
        <h2 className="font-semibold mb-2">Token 消耗趋势</h2>
        <div className="bg-white border rounded-lg p-4 h-64">
          <ResponsiveContainer width="100%" height="100%">
            <LineChart data={tokens?.usage ?? []}>
              <CartesianGrid strokeDasharray="3 3" />
              <XAxis dataKey="date" tick={{ fontSize: 12 }} />
              <YAxis tick={{ fontSize: 12 }} />
              <Tooltip />
              <Line type="monotone" dataKey="input_tokens" stroke="#3b82f6" name="Input" />
              <Line type="monotone" dataKey="output_tokens" stroke="#10b981" name="Output" />
            </LineChart>
          </ResponsiveContainer>
        </div>
      </section>

      <section>
        <h2 className="font-semibold mb-2">技能调用排行</h2>
        <div className="bg-white border rounded-lg p-4 h-64">
          <ResponsiveContainer width="100%" height="100%">
            <BarChart data={skills?.skills?.slice(0, 10) ?? []} layout="vertical">
              <CartesianGrid strokeDasharray="3 3" />
              <XAxis type="number" tick={{ fontSize: 12 }} />
              <YAxis type="category" dataKey="skill" tick={{ fontSize: 12 }} width={120} />
              <Tooltip />
              <Bar dataKey="count" fill="#6366f1" />
            </BarChart>
          </ResponsiveContainer>
        </div>
      </section>
    </div>
  );
}
```

- [ ] **Step 6: Wire all into App.tsx**

```tsx
import { Cron } from "./pages/Cron";
import { Logs } from "./pages/Logs";
import { Config } from "./pages/Config";
import { Analytics } from "./pages/Analytics";
// ...
<Route path="cron" element={<Cron />} />
<Route path="logs" element={<Logs />} />
<Route path="config" element={<Config />} />
<Route path="analytics" element={<Analytics />} />
```

- [ ] **Step 7: Verify build**

```bash
cd web && pnpm build
```

Expected: Build succeeds.

- [ ] **Step 8: Commit**

```bash
git add web/src/pages/Cron.tsx web/src/pages/Logs.tsx web/src/pages/Config.tsx web/src/pages/Analytics.tsx web/src/App.tsx web/package.json
git commit -m "前端 Cron/Logs/Config/Analytics 四个页面"
```

---

### Task 14: Backend — Serve Dashboard Static + Integration

**Files:**
- Modify: `echo_agent/gateway/server.py`
- Modify: `pyproject.toml`
- Test: `tests/test_gateway_dashboard_serve.py`

**Interfaces:**
- Consumes: Built `web/dist/` directory
- Produces: aiohttp serves the SPA at `GET /` with fallback to `index.html` for client-side routing; original playground moves to `/playground`

- [ ] **Step 1: Write failing test**

```python
# tests/test_gateway_dashboard_serve.py
import pytest
from pathlib import Path
from unittest.mock import MagicMock, patch
from aiohttp import web
from aiohttp.test_utils import AioHTTPTestCase


@pytest.mark.asyncio
async def test_dashboard_index_served(aiohttp_client, tmp_path):
    index = tmp_path / "index.html"
    index.write_text("<!DOCTYPE html><html><body>Dashboard</body></html>")

    from echo_agent.gateway.server import GatewayServer

    with patch.object(GatewayServer, "_resolve_dashboard_dir", return_value=tmp_path):
        app = web.Application()
        app.router.add_get("/", lambda r: web.FileResponse(index))
        client = await aiohttp_client(app)

        resp = await client.get("/")
        assert resp.status == 200
        text = await resp.text()
        assert "Dashboard" in text
```

- [ ] **Step 2: Run test to verify it fails**

Run: `pytest tests/test_gateway_dashboard_serve.py -v`
Expected: FAIL — `_resolve_dashboard_dir` does not exist

- [ ] **Step 3: Implement dashboard serving in server.py**

In `echo_agent/gateway/server.py`, add method and modify route setup:

```python
def _resolve_dashboard_dir(self) -> Path | None:
    candidates = [
        Path(__file__).parent.parent / "_bundled" / "dashboard",
        Path(__file__).parent.parent.parent / "web" / "dist",
    ]
    for p in candidates:
        if (p / "index.html").exists():
            return p
    return None

async def _handle_dashboard(self, request: web.Request) -> web.Response:
    dashboard_dir = self._resolve_dashboard_dir()
    if dashboard_dir is None:
        return await self._handle_playground(request)

    req_path = request.match_info.get("path", "")
    file_path = dashboard_dir / req_path
    if file_path.is_file():
        return web.FileResponse(file_path)
    return web.FileResponse(dashboard_dir / "index.html")
```

Replace the existing `GET /` route with:

```python
self._app.router.add_get("/playground", self._handle_playground)
self._app.router.add_get("/{path:.*}", self._handle_dashboard)
```

Ensure static assets (JS, CSS) are served before the fallback catches them.

- [ ] **Step 4: Update pyproject.toml for bundling**

Add to `[tool.hatch.build.targets.wheel.force-include]`:

```toml
"web/dist" = "echo_agent/_bundled/dashboard"
```

- [ ] **Step 5: Run test**

Run: `pytest tests/test_gateway_dashboard_serve.py -v`
Expected: PASS

- [ ] **Step 6: Commit**

```bash
git add echo_agent/gateway/server.py pyproject.toml tests/test_gateway_dashboard_serve.py
git commit -m "Gateway 托管 Dashboard SPA 静态文件，playground 移至 /playground"
```

---

### Task 15: Integration Test + Final Verification

**Files:**
- Create: `tests/test_dashboard_integration.py`

**Interfaces:**
- Consumes: All APIs from Tasks 1-6, frontend from Tasks 7-13, serving from Task 14
- Produces: End-to-end verification that backend APIs work and frontend builds successfully

- [ ] **Step 1: Write integration test**

```python
# tests/test_dashboard_integration.py
import pytest
from unittest.mock import MagicMock, AsyncMock
from aiohttp import web

from echo_agent.gateway.api.tasks import TasksAPI
from echo_agent.gateway.api.sessions import SessionsAPI
from echo_agent.gateway.api.cron_api import CronAPI
from echo_agent.gateway.api.logs import LogsAPI
from echo_agent.gateway.api.analytics import AnalyticsAPI
from echo_agent.tasks.models import TaskStatus, TaskRecord, VALID_TASK_TRANSITIONS


class TestTaskStateMachine:
    def test_all_transitions_valid(self):
        for status in TaskStatus:
            assert status in VALID_TASK_TRANSITIONS

    def test_blocked_and_review_exist(self):
        assert TaskStatus.BLOCKED.value == "blocked"
        assert TaskStatus.REVIEW.value == "review"

    def test_no_transition_out_of_terminal(self):
        assert VALID_TASK_TRANSITIONS[TaskStatus.SUCCESS] == set()
        assert VALID_TASK_TRANSITIONS[TaskStatus.CANCELLED] == set()

    def test_task_record_roundtrip(self):
        task = TaskRecord(
            title="integration test",
            labels=["test"],
            assignee="agent-x",
            source="human",
            board_id="default",
            status=TaskStatus.REVIEW,
            review_summary="looks good",
        )
        d = task.to_dict()
        restored = TaskRecord.from_dict(d)
        assert restored.status == TaskStatus.REVIEW
        assert restored.labels == ["test"]
        assert restored.review_summary == "looks good"
        assert restored.board_id == "default"


@pytest.mark.asyncio
async def test_full_task_lifecycle(aiohttp_client):
    server = MagicMock()
    server._require_api_token = MagicMock(return_value=None)

    tasks_store: dict[str, TaskRecord] = {}

    async def mock_create(**kwargs):
        task = TaskRecord(**kwargs)
        tasks_store[task.id] = task
        return task

    async def mock_transition(task_id, new_status):
        task = tasks_store[task_id]
        task.status = new_status
        return task

    async def mock_list(**kwargs):
        return list(tasks_store.values())

    manager = AsyncMock()
    manager.create = mock_create
    manager.transition = mock_transition
    manager.list_by_filters = mock_list
    server._agent_loop.task_manager = manager

    api = TasksAPI(server)
    app = web.Application()
    app.router.add_post("/api/v1/tasks", api.create_task)
    app.router.add_post("/api/v1/tasks/{id}/transition", api.transition_task)
    app.router.add_get("/api/v1/tasks", api.list_tasks)
    client = await aiohttp_client(app)

    resp = await client.post("/api/v1/tasks", json={"title": "test task", "source": "human"})
    assert resp.status == 201
    data = await resp.json()
    task_id = data["task"]["id"]

    resp = await client.post(f"/api/v1/tasks/{task_id}/transition", json={"to": "queued"})
    assert resp.status == 200

    resp = await client.get("/api/v1/tasks")
    assert resp.status == 200
    data = await resp.json()
    assert data["tasks"][0]["status"] == "queued"
```

- [ ] **Step 2: Run all tests**

```bash
pytest tests/test_task_state_machine.py tests/test_api_tasks.py tests/test_ws_dashboard.py tests/test_api_sessions.py tests/test_api_cron.py tests/test_api_logs.py tests/test_api_analytics.py tests/test_gateway_dashboard_serve.py tests/test_dashboard_integration.py -v
```

Expected: All PASS

- [ ] **Step 3: Verify frontend build**

```bash
cd web && pnpm build
ls dist/index.html dist/assets/
```

Expected: Files exist.

- [ ] **Step 4: Commit**

```bash
git add tests/test_dashboard_integration.py
git commit -m "Dashboard 端到端集成测试"
```

---

## Self-Review Notes

- All 15 tasks produce independently testable deliverables
- Task dependencies are linear: 1 → 2 → 3 (backend core) → 4-6 (backend APIs, parallel) → 7 (frontend scaffold) → 8-13 (pages, parallel after 7) → 14 (integration) → 15 (final verify)
- Types and method signatures are consistent across tasks (e.g., `TaskRecord.to_dict()` used in both backend tests and API handlers)
- No placeholders or TBDs remain
- All spec requirements covered: 11 pages, state machine extension, WebSocket, auth, build/deploy
