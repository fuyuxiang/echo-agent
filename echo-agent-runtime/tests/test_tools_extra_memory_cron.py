"""Extra contract tests for memory, cronjob, and delegate/spawn tools."""

from __future__ import annotations

from types import SimpleNamespace
from unittest.mock import AsyncMock, MagicMock

import pytest

from echo_agent.tools import ToolExecutionContext


def _ctx(**kwargs) -> ToolExecutionContext:
    defaults = {"session_key": "cli:c1", "user_id": "u1"}
    defaults.update(kwargs)
    return ToolExecutionContext(**defaults)


# ===========================================================================
# MemoryTool — real MemoryStore on tmp_path
# ===========================================================================


class TestMemoryTool:
    def _make(self, tmp_path):
        from echo_agent.agent.tools.memory import MemoryTool
        from echo_agent.memory.service import MemoryService
        from echo_agent.memory.store import MemoryStore

        store = MemoryStore(memory_dir=tmp_path / "mem")
        return MemoryTool(service=MemoryService(store)), store

    @pytest.mark.asyncio
    async def test_unknown_action(self, tmp_path):
        tool, _ = self._make(tmp_path)
        result = await tool.execute({"action": "explode"}, _ctx())
        assert result.success is False
        assert "Unknown action" in result.error

    @pytest.mark.asyncio
    async def test_add_requires_key_and_content(self, tmp_path):
        tool, _ = self._make(tmp_path)
        result = await tool.execute({"action": "add", "key": "k"}, _ctx())
        assert result.success is False
        assert "required" in result.error

    @pytest.mark.asyncio
    async def test_add_then_list(self, tmp_path):
        tool, _ = self._make(tmp_path)
        add = await tool.execute(
            {"action": "add", "target": "user", "key": "likes_tea",
             "content": "User prefers green tea", "tags": "drink, habit"},
            _ctx(),
        )
        assert add.success is True
        assert "likes_tea" in add.output

        listed = await tool.execute({"action": "list", "target": "user"}, _ctx())
        assert listed.success is True
        assert "likes_tea" in listed.output

    @pytest.mark.asyncio
    async def test_add_invalid_importance_falls_back(self, tmp_path):
        tool, _ = self._make(tmp_path)
        result = await tool.execute(
            {"action": "add", "key": "k", "content": "c", "importance": "not-a-number"},
            _ctx(),
        )
        assert result.success is True

    @pytest.mark.asyncio
    async def test_replace_requires_content(self, tmp_path):
        tool, _ = self._make(tmp_path)
        result = await tool.execute(
            {"action": "replace", "key": "k", "old_text": "x"}, _ctx()
        )
        assert result.success is False
        assert "content is required" in result.error

    @pytest.mark.asyncio
    async def test_remove_no_match(self, tmp_path):
        tool, _ = self._make(tmp_path)
        result = await tool.execute(
            {"action": "remove", "key": "nonexistent"}, _ctx()
        )
        assert result.success is False
        assert "No matching memory" in result.error

    @pytest.mark.asyncio
    async def test_search_requires_query(self, tmp_path):
        tool, _ = self._make(tmp_path)
        result = await tool.execute({"action": "search"}, _ctx())
        assert result.success is False
        assert "query is required" in result.error

    @pytest.mark.asyncio
    async def test_search_no_results(self, tmp_path):
        tool, _ = self._make(tmp_path)
        result = await tool.execute(
            {"action": "search", "query": "nothing-here"}, _ctx()
        )
        assert result.success is True
        assert "No matching memories" in result.output

    @pytest.mark.asyncio
    async def test_list_empty(self, tmp_path):
        tool, _ = self._make(tmp_path)
        result = await tool.execute({"action": "list", "target": "environment"}, _ctx())
        assert result.success is True
        assert "No environment memories" in result.output

    @pytest.mark.asyncio
    async def test_add_replace_remove_roundtrip(self, tmp_path):
        tool, _ = self._make(tmp_path)
        await tool.execute(
            {"action": "add", "key": "city", "content": "lives in Tokyo"}, _ctx()
        )
        rep = await tool.execute(
            {"action": "replace", "key": "city", "content": "lives in Osaka"}, _ctx()
        )
        assert rep.success is True
        rem = await tool.execute({"action": "remove", "key": "city"}, _ctx())
        assert rem.success is True
        assert "removed" in rem.output.lower()


# ===========================================================================
# CronjobTool
# ===========================================================================


class TestCronjobTool:
    def _make(self, scheduler=None):
        from echo_agent.agent.tools.cronjob import CronjobTool

        return CronjobTool(scheduler=scheduler)

    @pytest.mark.asyncio
    async def test_scheduler_disabled(self):
        tool = self._make(scheduler=None)
        result = await tool.execute({"action": "list"}, _ctx())
        assert result.success is False
        assert "Scheduler not enabled" in result.error

    @pytest.mark.asyncio
    async def test_create_requires_schedule_and_command(self):
        sched = MagicMock()
        tool = self._make(scheduler=sched)
        result = await tool.execute({"action": "create", "name": "x"}, _ctx())
        assert result.success is False
        assert "required" in result.error

    @pytest.mark.asyncio
    async def test_create_success(self):
        sched = MagicMock()
        sched.add_job.return_value = SimpleNamespace(id="job-1")
        tool = self._make(scheduler=sched)
        result = await tool.execute(
            {"action": "create", "name": "daily", "schedule": "0 9 * * *",
             "command": "say hi"},
            _ctx(session_key="cli:c1"),
        )
        assert result.success is True
        assert "job-1" in result.output
        assert result.metadata["job_id"] == "job-1"
        sched.add_job.assert_called_once()

    @pytest.mark.asyncio
    async def test_list_empty(self):
        sched = MagicMock()
        sched.list_jobs.return_value = []
        tool = self._make(scheduler=sched)
        result = await tool.execute({"action": "list"}, _ctx())
        assert result.success is True
        assert "No scheduled jobs" in result.output

    @pytest.mark.asyncio
    async def test_list_with_jobs(self):
        sched = MagicMock()
        sched.list_jobs.return_value = [
            SimpleNamespace(id="j1", name="job1", cron_expr="* * * * *",
                            interval_ms=0, at_ms=0, payload={"command": "echo hi"}),
        ]
        tool = self._make(scheduler=sched)
        result = await tool.execute({"action": "list"}, _ctx())
        assert result.success is True
        assert "j1" in result.output
        assert "echo hi" in result.output

    @pytest.mark.asyncio
    async def test_delete_requires_job_id(self):
        sched = MagicMock()
        tool = self._make(scheduler=sched)
        result = await tool.execute({"action": "delete"}, _ctx())
        assert result.success is False
        assert "job_id" in result.error

    @pytest.mark.asyncio
    async def test_delete_not_found(self):
        sched = MagicMock()
        sched.remove_job.return_value = False
        tool = self._make(scheduler=sched)
        result = await tool.execute({"action": "delete", "job_id": "bad"}, _ctx())
        assert result.success is False
        assert "not found" in result.error

    @pytest.mark.asyncio
    async def test_delete_success(self):
        sched = MagicMock()
        sched.remove_job.return_value = True
        tool = self._make(scheduler=sched)
        result = await tool.execute({"action": "delete", "job_id": "j1"}, _ctx())
        assert result.success is True
        assert "Deleted job j1" in result.output

    @pytest.mark.asyncio
    async def test_trigger_requires_job_id(self):
        sched = MagicMock()
        tool = self._make(scheduler=sched)
        result = await tool.execute({"action": "trigger"}, _ctx())
        assert result.success is False
        assert "job_id" in result.error

    @pytest.mark.asyncio
    async def test_trigger_success(self):
        sched = MagicMock()
        sched.trigger_job = AsyncMock(return_value=True)
        tool = self._make(scheduler=sched)
        result = await tool.execute({"action": "trigger", "job_id": "j1"}, _ctx())
        assert result.success is True
        assert "Triggered job j1" in result.output

    @pytest.mark.asyncio
    async def test_trigger_failed(self):
        sched = MagicMock()
        sched.trigger_job = AsyncMock(return_value=False)
        tool = self._make(scheduler=sched)
        result = await tool.execute({"action": "trigger", "job_id": "j1"}, _ctx())
        assert result.success is False
        assert "not found or failed" in result.error

    @pytest.mark.asyncio
    async def test_unknown_action(self):
        sched = MagicMock()
        tool = self._make(scheduler=sched)
        result = await tool.execute({"action": "boom"}, _ctx())
        assert result.success is False
        assert "Unknown action" in result.error


# ===========================================================================
# MemoryTool — 写后缓存失效回调 (invalidate_caches)
# ===========================================================================


class TestMemoryToolCacheInvalidation:
    def _make(self, tmp_path):
        from echo_agent.agent.tools.memory import MemoryTool
        from echo_agent.memory.service import MemoryService
        from echo_agent.memory.store import MemoryStore

        calls: list[tuple[str, bool]] = []

        async def _invalidate(session_key: str, global_scope: bool) -> None:
            calls.append((session_key, global_scope))

        store = MemoryStore(memory_dir=tmp_path / "mem")
        # 写后失效已下沉到 service;本测试验证 ENVIRONMENT 写触发全局失效,
        # 需放行模型写 ENVIRONMENT(默认禁止模型写 ENVIRONMENT/global)。
        service = MemoryService(store, invalidate_fn=_invalidate, allow_env_writes=True)
        tool = MemoryTool(service=service)
        return tool, store, calls

    @pytest.mark.asyncio
    async def test_add_user_memory_invalidates_session_scope(self, tmp_path):
        tool, _, calls = self._make(tmp_path)
        result = await tool.execute(
            {"action": "add", "target": "user", "key": "k1", "content": "v1"},
            _ctx(session_key="s1"),
        )
        assert result.success is True
        assert calls == [("s1", False)]

    @pytest.mark.asyncio
    async def test_add_environment_memory_invalidates_globally(self, tmp_path):
        tool, _, calls = self._make(tmp_path)
        result = await tool.execute(
            {"action": "add", "target": "environment", "key": "k1", "content": "v1"},
            _ctx(session_key="s1"),
        )
        assert result.success is True
        assert calls == [("s1", True)]

    @pytest.mark.asyncio
    async def test_remove_invalidates(self, tmp_path):
        tool, _, calls = self._make(tmp_path)
        await tool.execute(
            {"action": "add", "target": "user", "key": "k1", "content": "v1"},
            _ctx(session_key="s1"),
        )
        calls.clear()
        result = await tool.execute(
            {"action": "remove", "target": "user", "key": "k1"},
            _ctx(session_key="s1"),
        )
        assert result.success is True
        assert calls == [("s1", False)]

    @pytest.mark.asyncio
    async def test_replace_invalidates(self, tmp_path):
        tool, _, calls = self._make(tmp_path)
        await tool.execute(
            {"action": "add", "target": "user", "key": "k1", "content": "v1"},
            _ctx(session_key="s1"),
        )
        calls.clear()
        result = await tool.execute(
            {"action": "replace", "target": "user", "key": "k1", "content": "v2"},
            _ctx(session_key="s1"),
        )
        assert result.success is True
        assert calls == [("s1", False)]

    @pytest.mark.asyncio
    async def test_failed_write_does_not_invalidate(self, tmp_path):
        tool, _, calls = self._make(tmp_path)
        result = await tool.execute(
            {"action": "remove", "target": "user", "key": "missing"},
            _ctx(session_key="s1"),
        )
        assert result.success is False
        assert calls == []

    @pytest.mark.asyncio
    async def test_read_actions_do_not_invalidate(self, tmp_path):
        tool, _, calls = self._make(tmp_path)
        await tool.execute(
            {"action": "add", "target": "user", "key": "k1", "content": "v1"},
            _ctx(session_key="s1"),
        )
        calls.clear()
        await tool.execute({"action": "search", "query": "v1"}, _ctx(session_key="s1"))
        await tool.execute({"action": "list", "target": "user"}, _ctx(session_key="s1"))
        assert calls == []

    @pytest.mark.asyncio
    async def test_no_callback_is_noop(self, tmp_path):
        from echo_agent.agent.tools.memory import MemoryTool
        from echo_agent.memory.service import MemoryService
        from echo_agent.memory.store import MemoryStore

        store = MemoryStore(memory_dir=tmp_path / "mem")
        tool = MemoryTool(service=MemoryService(store))
        result = await tool.execute(
            {"action": "add", "target": "user", "key": "k1", "content": "v1"},
            _ctx(session_key="s1"),
        )
        assert result.success is True


@pytest.mark.asyncio
async def test_cronjob_create_grants_authorization_on_human_approval():
    """A grant is issued when — and only when — a human approved THIS call.

    Reaching execute() was previously treated as proof of that, on the grounds
    that the tool is risk_level=dangerous. It is not: with the shipped defaults
    the gate auto-approved every risk level on cli channels. So the gate now
    reports how the call was approved and the tool keys off that."""
    from unittest.mock import MagicMock

    from echo_agent.agent.approval_gate import APPROVAL_SOURCE_HUMAN
    from echo_agent.agent.tools.cronjob import CronjobTool
    from echo_agent.scheduler.authorization import verify
    from echo_agent.tools import ToolExecutionContext

    captured = {}
    scheduler = MagicMock()
    scheduler.add_job = MagicMock(side_effect=lambda job: captured.setdefault("job", job) or job)

    tool = CronjobTool(scheduler)
    ctx = ToolExecutionContext(
        session_key="telegram:123", user_id="alice",
        approval_source=APPROVAL_SOURCE_HUMAN,
    )
    result = await tool.execute(
        {"action": "create", "name": "nightly", "schedule": "0 9 * * *", "command": "echo hi"},
        ctx,
    )

    assert result.success is True
    job = captured["job"]
    assert job.authorization is not None
    assert job.authorization.operator == "alice"
    assert job.authorization.source == "tui-approval"
    assert verify(job) is True
    # The grant must live on the job itself, never smuggled into the payload —
    # payload keys are exactly the inferred-permission channel this replaces.
    assert "authorization" not in job.payload


@pytest.mark.asyncio
async def test_cronjob_create_without_human_approval_does_not_grant():
    """An auto-approved call creates a working job but no unattended grant.

    This is the hole that made the whole per-job authorization scheme
    decorative: cli_auto_approve let the model reach execute() with nobody
    watching, and the tool signed a grant labelled "tui-approval" for content no
    human had seen. The job must still be created (it fires; only its privileged
    tool calls are denied) and the user must be told how to authorize it."""
    from unittest.mock import MagicMock

    from echo_agent.agent.tools.cronjob import CronjobTool
    from echo_agent.scheduler.authorization import verify
    from echo_agent.tools import ToolExecutionContext

    captured = {}
    scheduler = MagicMock()
    scheduler.add_job = MagicMock(side_effect=lambda job: captured.setdefault("job", job) or job)

    tool = CronjobTool(scheduler)
    result = await tool.execute(
        {"action": "create", "name": "nightly", "schedule": "0 9 * * *", "command": "echo hi"},
        ToolExecutionContext(session_key="telegram:123", user_id="alice", approval_source="auto"),
    )

    assert result.success is True
    assert captured["job"].authorization is None
    assert verify(captured["job"]) is False
    assert "cron authorize" in result.output


@pytest.mark.asyncio
async def test_cronjob_create_rejects_invalid_cron_expression():
    """An unparseable expression must not be stored.

    It used to be accepted: the job was persisted and even granted an
    authorization, but _compute_next_run returned None so it never fired — a
    "created successfully" that silently does nothing."""
    from unittest.mock import MagicMock

    from echo_agent.agent.approval_gate import APPROVAL_SOURCE_HUMAN
    from echo_agent.agent.tools.cronjob import CronjobTool
    from echo_agent.tools import ToolExecutionContext

    scheduler = MagicMock()
    tool = CronjobTool(scheduler)
    result = await tool.execute(
        {"action": "create", "name": "n", "schedule": "every monday", "command": "echo hi"},
        ToolExecutionContext(approval_source=APPROVAL_SOURCE_HUMAN),
    )

    assert result.success is False
    assert result.error_kind == "validation"
    scheduler.add_job.assert_not_called()


@pytest.mark.asyncio
async def test_cronjob_create_without_ctx_does_not_grant():
    """A missing ctx says nothing about consent, so it must not grant.

    This used to assert the opposite — that a ctx-less call still produced a
    valid grant — on the theory that reaching execute() implied approval. Absence
    of evidence is not consent, and this is the path a non-pipeline caller
    (plugin, script, future entry point) takes, so it has to fail closed."""
    from unittest.mock import MagicMock

    from echo_agent.agent.tools.cronjob import CronjobTool
    from echo_agent.scheduler.authorization import verify

    captured = {}
    scheduler = MagicMock()
    scheduler.add_job = MagicMock(side_effect=lambda job: captured.setdefault("job", job) or job)

    tool = CronjobTool(scheduler)
    result = await tool.execute(
        {"action": "create", "name": "n", "schedule": "0 9 * * *", "command": "echo hi",
         "target_channel": "telegram", "target_chat_id": "1"},
        None,
    )

    assert result.success is True
    assert captured["job"].authorization is None
    assert verify(captured["job"]) is False
