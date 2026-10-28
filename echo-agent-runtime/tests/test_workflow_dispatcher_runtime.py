"""The real dispatcher owns workflow attempts through the real AgentLoop."""

import asyncio
import re

import pytest

from echo_agent.agent.loop import AgentLoop
from echo_agent.bus.queue import MessageBus
from echo_agent.config.schema import Config
from echo_agent.models.provider import LLMProvider, LLMResponse, ToolCallRequest
from echo_agent.storage.sqlite import SQLiteBackend
from echo_agent.tasks.dispatcher import TaskDispatcher
from echo_agent.tasks.manager import TaskManager
from echo_agent.tasks.models import TaskStatus, WorkflowStatus
from echo_agent.tasks.workflow import WorkflowEngine


class _WorkflowProvider(LLMProvider):
    """Only the remote model is replaced; tool execution is the actual runtime."""

    def __init__(self, fail_first):
        super().__init__()
        self.fail_first = fail_first
        self.dispatched = []
        self.tool_results = []

    def get_default_model(self):
        return "controlled-workflow"

    async def chat(self, messages, tools=None, model=None, **kwargs):
        if messages[-1]["role"] == "tool":
            self.tool_results.append(messages[-1]["content"])
            return LLMResponse(content="Execution finished.", model=self.get_default_model())
        instruction = next(m["content"] for m in reversed(messages) if m["role"] == "user")
        task_id = re.search(r"任务ID: (t_[a-f0-9]+)", instruction).group(1)
        self.dispatched.append(task_id)
        params = {"task_id": task_id, "action": "fail" if self.fail_first else "complete"}
        params["error" if self.fail_first else "result"] = "controlled failure" if self.fail_first else "done"
        return LLMResponse(
            tool_calls=[ToolCallRequest(id=f"close-{task_id}", name="task", arguments=params)],
            finish_reason="tool_calls", model=self.get_default_model(),
        )


@pytest.mark.asyncio
@pytest.mark.parametrize("fail_first", [False, True])
async def test_workflow_auto_dispatch_executes_and_persists_terminal_state(tmp_path, fail_first):
    storage = SQLiteBackend(tmp_path / "runtime.db")
    await storage.initialize()
    manager = TaskManager(storage)
    engine = WorkflowEngine(storage, manager)
    bus = MessageBus()
    provider = _WorkflowProvider(fail_first)
    config = Config()
    config.memory.enabled = False
    config.knowledge.enabled = False
    config.evolution.enabled = False
    config.multi_agent.enabled = False
    config.planning.enabled = False
    config.tools.allow = ["task"]
    config.execution.network_policy = "deny"
    # This disposable trusted workspace explicitly permits unattended WRITE
    # work. EXEC/dangerous stay gated, and the production gate remains active.
    config.permissions.approval.unattended_policy = "allow_safe"
    agent = AgentLoop(bus, config, provider, tmp_path, storage=storage,
                      task_manager=manager, workflow_engine=engine)
    dispatcher = TaskDispatcher(bus, manager, owner_id="integration-runtime",
                                workflow_engine=engine, poll_interval_sec=0.01,
                                stop_grace_sec=1)
    manager.add_terminal_listener(dispatcher._on_task_terminal)
    expected = WorkflowStatus.FAILED if fail_first else WorkflowStatus.SUCCESS
    try:
        await agent.start()
        await bus.start()
        await dispatcher.start()
        workflow = await engine.create("real runtime workflow", [
            {"id": "first", "name": "first"},
            {"id": "second", "name": "second", "depends_on": ["first"]},
        ])
        await engine.start(workflow.id)

        async def wait_until_finished():
            while True:
                current = await engine.get(workflow.id)
                if current.status == expected and not bus._inflight_inbound and not dispatcher._inflight:
                    return current
                await asyncio.sleep(0.01)

        result = await asyncio.wait_for(wait_until_finished(), timeout=5)
        assert len(provider.dispatched) == (1 if fail_first else 2)
        assert len(provider.tool_results) == len(provider.dispatched)
        assert all("Error:" not in content for content in provider.tool_results)
        assert len(set(provider.dispatched)) == len(provider.dispatched)
        assert set(result.step_tasks) == ({"first"} if fail_first else {"first", "second"})
        for task_id in result.step_tasks.values():
            task = await manager.get(task_id)
            assert task.status == (TaskStatus.FAILED if fail_first else TaskStatus.SUCCESS)
            assert (task.error == "controlled failure") if fail_first else (task.result == "done")
            assert task.attempt_id and task.session_id == f"task:{task_id}"
            history = await agent.sessions.get(task.session_id)
            assert any(message["role"] == "tool" for message in history.messages)
            # Board tasks expose their result through persisted task/session/
            # turn APIs; they deliberately do not send to a synthetic channel.
            turn = await agent._turn_runs.get(task.metadata["_interrupt_event_id"])
            assert turn["status"] == "completed"
            assert turn["response_text"]
    finally:
        await dispatcher.stop()
        await bus.stop()
        await agent.stop()
        await storage.close()

    # Read from a fresh connection to establish persistence, rather than merely
    # asserting the previous manager's in-memory object graph.
    reopened = SQLiteBackend(tmp_path / "runtime.db")
    await reopened.initialize()
    try:
        restored = await WorkflowEngine(reopened, TaskManager(reopened)).get(workflow.id)
        assert restored.status == expected
        assert restored.step_tasks == result.step_tasks
    finally:
        await reopened.close()
