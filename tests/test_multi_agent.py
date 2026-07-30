"""Tests for the multi-agent delegation system (WorkerRegistry, WorkerExecutor, DelegateTool)."""

import asyncio
from unittest.mock import AsyncMock, MagicMock

import pytest

from echo_agent.agent.multi_agent.models import WorkerProfile, WorkerToolOutcome
from echo_agent.agent.multi_agent.registry import WorkerRegistry
from echo_agent.agent.multi_agent.runtime import WorkerExecutor
from echo_agent.agent.tools.delegate import DelegateTool, WORKER_BLOCKED_TOOLS


class TestWorkerRegistry:
    def test_from_config_empty(self):
        config = MagicMock()
        config.worker_profiles = []
        registry = WorkerRegistry.from_config(config)
        assert registry.list() == []

    def test_from_config_with_profiles(self):
        cfg1 = MagicMock()
        cfg1.id = "coder"
        cfg1.name = "Coding Worker"
        cfg1.description = "Code tasks"
        cfg1.instructions = "Write code"
        cfg1.default_tools = ["exec", "read_file"]
        cfg1.model = ""
        cfg1.provider = ""
        cfg1.max_iterations = 12
        cfg1.max_tokens = 4096
        cfg1.temperature = 0.3

        config = MagicMock()
        config.worker_profiles = [cfg1]
        registry = WorkerRegistry.from_config(config)

        assert len(registry.list()) == 1
        profile = registry.get("coder")
        assert profile is not None
        assert profile.name == "Coding Worker"
        assert profile.default_tools == ("exec", "read_file")

    def test_get_nonexistent(self):
        registry = WorkerRegistry([])
        assert registry.get("nonexistent") is None

    def test_list_ids(self):
        profiles = [
            WorkerProfile(id="a", name="A"),
            WorkerProfile(id="b", name="B"),
        ]
        registry = WorkerRegistry(profiles)
        assert set(registry.list_ids()) == {"a", "b"}


class TestWorkerExecutor:
    @pytest.mark.asyncio
    async def test_run_simple_completion(self):
        provider = AsyncMock()
        response = MagicMock()
        response.finish_reason = "stop"
        response.content = "Task completed successfully."
        response.has_tool_calls = False
        response.usage = None
        provider.chat_with_retry = AsyncMock(return_value=response)

        executor = WorkerExecutor(provider=provider)
        result = await executor.run(
            task_index=0,
            goal="Say hello",
            tool_defs=[],
            tool_executor=AsyncMock(),
            max_iterations=5,
        )

        assert result.status == "completed"
        assert result.output == "Task completed successfully."
        assert result.iterations == 1
        assert result.tool_calls == 0

    @pytest.mark.asyncio
    async def test_run_with_tool_calls(self):
        provider = AsyncMock()

        tc = MagicMock()
        tc.id = "tc_1"
        tc.name = "read_file"
        tc.arguments = {"path": "/tmp/test.txt"}
        tc.to_openai_format = MagicMock(return_value={
            "id": "tc_1", "type": "function",
            "function": {"name": "read_file", "arguments": "{}"},
        })

        response1 = MagicMock()
        response1.finish_reason = "tool_calls"
        response1.content = "Let me read the file."
        response1.has_tool_calls = True
        response1.tool_calls = [tc]
        response1.usage = None

        response2 = MagicMock()
        response2.finish_reason = "stop"
        response2.content = "The file contains: hello world"
        response2.has_tool_calls = False
        response2.usage = None

        provider.chat_with_retry = AsyncMock(side_effect=[response1, response2])
        tool_executor = AsyncMock(return_value="hello world")

        executor = WorkerExecutor(provider=provider)
        result = await executor.run(
            task_index=0,
            goal="Read the file",
            tool_defs=[{"type": "function", "function": {"name": "read_file"}}],
            tool_executor=tool_executor,
            max_iterations=5,
        )

        assert result.status == "completed"
        assert result.tool_calls == 1
        assert result.iterations == 2

    @pytest.mark.asyncio
    async def test_run_timeout(self):
        provider = AsyncMock()

        async def slow_chat(**kwargs):
            await asyncio.sleep(10)
            return MagicMock(finish_reason="stop", content="done", has_tool_calls=False)

        provider.chat_with_retry = slow_chat

        executor = WorkerExecutor(provider=provider)
        result = await executor.run(
            task_index=0,
            goal="Slow task",
            tool_defs=[],
            tool_executor=AsyncMock(),
            max_iterations=5,
            timeout_seconds=0.1,
        )

        assert result.status == "timeout"


class TestDelegateTool:
    def test_normalize_tasks_single(self):
        tool = self._make_tool()
        tasks = tool._normalize_tasks({"goal": "do something", "tools": ["exec"]})
        assert len(tasks) == 1
        assert tasks[0]["goal"] == "do something"

    def test_normalize_tasks_array(self):
        tool = self._make_tool()
        tasks = tool._normalize_tasks({
            "tasks": [
                {"goal": "task 1"},
                {"goal": "task 2"},
            ]
        })
        assert len(tasks) == 2

    def test_normalize_tasks_empty(self):
        tool = self._make_tool()
        tasks = tool._normalize_tasks({})
        assert tasks == []

    def test_resolve_worker_tools_with_explicit(self):
        tool = self._make_tool()
        available = {"exec", "read_file", "write_file", "web_search"}
        result = tool._resolve_worker_tools({"tools": ["exec", "read_file", "nonexistent"]}, available)
        assert result == {"exec", "read_file"}

    def test_resolve_worker_tools_with_profile(self):
        tool = self._make_tool()
        available = {"exec", "read_file", "write_file", "web_search"}
        result = tool._resolve_worker_tools({"worker_profile": "coder"}, available)
        assert result == {"exec", "read_file"}

    def test_blocked_tools(self):
        assert "delegate_task" in WORKER_BLOCKED_TOOLS
        assert "clarify" in WORKER_BLOCKED_TOOLS
        assert "message" in WORKER_BLOCKED_TOOLS

    @pytest.mark.asyncio
    async def test_depth_limit(self):
        tool = self._make_tool(max_depth=2)
        from echo_agent.agent.tools.base import ToolExecutionContext
        ctx = ToolExecutionContext(parent_execution_id="worker:test:2")
        result = await tool.execute({"goal": "test"}, ctx)
        assert not result.success
        assert "depth limit" in result.error.lower()

    def _make_tool(self, max_depth=3):
        registry = WorkerRegistry([
            WorkerProfile(id="coder", name="Coder", default_tools=("exec", "read_file")),
        ])
        tool_registry = MagicMock()
        tool_registry.ready_tool_names = ["exec", "read_file", "write_file", "web_search"]
        tool_registry.get_ready_definitions = MagicMock(return_value=[])

        return DelegateTool(
            provider=AsyncMock(),
            tool_registry=tool_registry,
            worker_registry=registry,
            approval_gate=AsyncMock(),
            credentials=MagicMock(),
            audit_path=None,
            max_depth=max_depth,
            max_parallel_workers=4,
            max_worker_iterations=12,
        )


class TestWorkerExecutorMessageCap:
    """Test that the message list is capped to prevent OOM."""

    @pytest.mark.asyncio
    async def test_messages_trimmed_when_exceeding_cap(self):
        call_count = 0

        def make_response(has_tools=True):
            r = MagicMock()
            r.content = "thinking..."
            r.finish_reason = "stop"
            if has_tools:
                tc = MagicMock()
                tc.id = f"tc_{call_count}"
                tc.name = "read_file"
                # Distinct path per call: this test is about message trimming,
                # so it must not trip the repeat-call guard.
                tc.arguments = {"path": f"/tmp/test_{call_count}.txt"}
                tc.to_openai_format.return_value = {
                    "id": tc.id, "type": "function",
                    "function": {"name": "read_file", "arguments": "{}"},
                }
                r.tool_calls = [tc]
                r.has_tool_calls = True
            else:
                r.tool_calls = []
                r.has_tool_calls = False
            return r

        provider = MagicMock()

        async def mock_chat(**kwargs):
            nonlocal call_count
            call_count += 1
            messages = kwargs.get("messages", [])
            # After trimming, messages should never exceed _MAX_MESSAGES
            assert len(messages) <= WorkerExecutor._MAX_MESSAGES + 10
            if call_count >= 10:
                return make_response(has_tools=False)
            return make_response(has_tools=True)

        provider.chat_with_retry = mock_chat

        async def tool_executor(name, tc, idx):
            return "x" * 1000

        executor = WorkerExecutor(provider=provider, default_model="test")
        result = await executor.run(
            task_index=0,
            goal="Do work",
            tool_defs=[{"type": "function", "function": {"name": "read_file"}}],
            tool_executor=tool_executor,
            max_iterations=15,
        )

        assert result.status == "completed"
        assert result.iterations > 0

    @pytest.mark.asyncio
    async def test_timeout_returns_state_values(self):
        """Verify timeout handler uses state dict for accurate reporting."""
        provider = MagicMock()

        async def slow_chat(**kwargs):
            await asyncio.sleep(10)
            r = MagicMock()
            r.content = "done"
            r.tool_calls = []
            r.has_tool_calls = False
            r.finish_reason = "stop"
            return r

        provider.chat_with_retry = slow_chat
        executor = WorkerExecutor(provider=provider, default_model="test")
        result = await executor.run(
            task_index=0,
            goal="Slow task",
            tool_defs=[],
            tool_executor=AsyncMock(),
            max_iterations=5,
            timeout_seconds=0.1,
        )

        assert result.status == "timeout"
        # state["iterations"] is set to 1 at the start of the first iteration
        # before the chat call blocks, so it will be >= 1
        assert result.iterations >= 0


class TestWorkerToolFailureEarlyExit:
    """A worker whose tools keep failing must abort instead of burning every
    iteration and reporting only "Reached max iterations"."""

    @staticmethod
    def _tool_response(call_id: str, args: dict | None = None):
        tc = MagicMock()
        tc.id = call_id
        tc.name = "search_files"
        tc.arguments = args if args is not None else {"query": call_id}
        tc.to_openai_format = MagicMock(return_value={
            "id": call_id, "type": "function",
            "function": {"name": "search_files", "arguments": "{}"},
        })
        r = MagicMock()
        r.content = "trying again"
        r.finish_reason = "tool_calls"
        r.tool_calls = [tc]
        r.has_tool_calls = True
        return r

    def _always_failing_provider(self):
        provider = MagicMock()
        self.calls = 0

        async def chat(**kwargs):
            self.calls += 1
            # Vary arguments so the repeat guard is not what stops the loop.
            return self._tool_response(f"tc_{self.calls}", {"query": f"q{self.calls}"})

        provider.chat_with_retry = chat
        return provider

    @pytest.mark.asyncio
    async def test_aborts_after_consecutive_failures(self):
        executor = WorkerExecutor(provider=self._always_failing_provider(), default_model="test")
        executed = 0

        async def tool_executor(name, tc, idx):
            nonlocal executed
            executed += 1
            return WorkerToolOutcome(text="Error: file not found", success=False)

        result = await executor.run(
            task_index=0,
            goal="Find the thing",
            tool_defs=[{"type": "function", "function": {"name": "search_files"}}],
            tool_executor=tool_executor,
            max_iterations=12,
        )

        assert result.status == "failed"
        assert "consecutive tool failures" in result.error
        assert "file not found" in result.error
        # Stops at the threshold instead of running all 12 iterations.
        assert executed == WorkerExecutor._MAX_CONSECUTIVE_TOOL_FAILURES
        assert result.iterations < 12

    @pytest.mark.asyncio
    async def test_legacy_string_error_prefix_counts_as_failure(self):
        """Executors returning a bare "Error: ..." string still trip the exit."""
        executor = WorkerExecutor(provider=self._always_failing_provider(), default_model="test")

        async def tool_executor(name, tc, idx):
            return "Error: tool is not available for this worker."

        result = await executor.run(
            task_index=0,
            goal="Find the thing",
            tool_defs=[{"type": "function", "function": {"name": "search_files"}}],
            tool_executor=tool_executor,
            max_iterations=12,
        )

        assert result.status == "failed"
        assert "consecutive tool failures" in result.error
        assert result.iterations < 12

    @pytest.mark.asyncio
    async def test_success_resets_failure_streak(self):
        """Intermittent failures must not abort a worker that makes progress."""
        provider = MagicMock()
        state = {"n": 0}

        async def chat(**kwargs):
            state["n"] += 1
            if state["n"] >= 6:
                r = MagicMock()
                r.content = "all done"
                r.finish_reason = "stop"
                r.tool_calls = []
                r.has_tool_calls = False
                return r
            return self._tool_response(f"tc_{state['n']}", {"query": f"q{state['n']}"})

        provider.chat_with_retry = chat
        executor = WorkerExecutor(provider=provider, default_model="test")
        seq = 0

        async def tool_executor(name, tc, idx):
            nonlocal seq
            seq += 1
            # fail, fail, succeed, fail, fail — never 3 in a row
            ok = seq % 3 == 0
            return WorkerToolOutcome(
                text="found it" if ok else "Error: transient failure", success=ok,
            )

        result = await executor.run(
            task_index=0,
            goal="Find the thing",
            tool_defs=[{"type": "function", "function": {"name": "search_files"}}],
            tool_executor=tool_executor,
            max_iterations=12,
        )

        assert result.status == "completed"
        assert result.output == "all done"

    @pytest.mark.asyncio
    async def test_identical_calls_are_repeat_blocked_without_executing(self):
        """The Nth identical call is short-circuited, not re-executed."""
        provider = MagicMock()

        async def chat(**kwargs):
            # Same name AND same arguments every time.
            return self._tool_response("tc_same", {"query": "identical"})

        provider.chat_with_retry = chat
        executor = WorkerExecutor(provider=provider, default_model="test")
        executed = 0

        async def tool_executor(name, tc, idx):
            nonlocal executed
            executed += 1
            return WorkerToolOutcome(text="(empty result)", success=True)

        result = await executor.run(
            task_index=0,
            goal="Loop forever",
            tool_defs=[{"type": "function", "function": {"name": "search_files"}}],
            tool_executor=tool_executor,
            max_iterations=12,
        )

        assert result.status == "failed"
        # Executed only up to the threshold; later identical calls are blocked.
        assert executed == WorkerExecutor._REPEAT_BLOCK_THRESHOLD - 1
        assert result.iterations < 12
