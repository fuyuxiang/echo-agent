"""Extra contract tests for memory, cronjob, and delegate/spawn tools."""

from __future__ import annotations

from types import SimpleNamespace
from unittest.mock import AsyncMock, MagicMock

import pytest

from echo_agent.agent.tools.base import ToolExecutionContext


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
        from echo_agent.memory.store import MemoryStore

        store = MemoryStore(memory_dir=tmp_path / "mem")
        return MemoryTool(store=store), store

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
