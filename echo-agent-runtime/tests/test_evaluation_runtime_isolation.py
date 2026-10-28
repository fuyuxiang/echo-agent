"""Regression coverage for real session/SQLite/filesystem evaluation isolation."""

from __future__ import annotations

import asyncio
from types import SimpleNamespace

import pytest

from echo_agent.config.schema import Config
from echo_agent.evaluation.dataset import EvalCase, EvalDataset
from echo_agent.evaluation.runner import CaseResult, EvalReport, EvalRunner
from echo_agent.evaluation.runtime import IsolatedEvalRunner
from echo_agent.evolution.gate import PromotionGate
from echo_agent.evolution.store import TrajectoryStore
from echo_agent.evolution.types import SkillCandidate
from echo_agent.models.provider import LLMProvider, LLMResponse, ToolCallRequest
from echo_agent.session.manager import SessionManager
from echo_agent.skills.store import SkillStore
from echo_agent.storage.sqlite import SQLiteBackend


@pytest.mark.asyncio
async def test_repeated_case_id_does_not_reuse_history_or_tool_counts(tmp_path):
    storage = SQLiteBackend(tmp_path / "eval.db")
    await storage.initialize()
    sessions = SessionManager(tmp_path / "sessions", storage=storage)
    keys = []

    async def process(event, trace_id):
        keys.append(event.session_key)
        session = await sessions.get_or_create(event.session_key)
        session.add_message("user", "hi")
        if len(keys) == 1:
            session.add_message("tool", "first run only", name="read_file")
        session.add_message("assistant", "ok")
        await sessions.save(session)
        return SimpleNamespace(response_text="ok", task_incomplete=False)

    runner = EvalRunner(SimpleNamespace(sessions=sessions, _process_event=process))
    try:
        first = await runner.run_case(EvalCase(id="same", input="hi"))
        second = await runner.run_case(EvalCase(id="same", input="hi"))
        assert first.tools_used == ["read_file"]
        assert second.tools_used == []
        assert first.iterations == second.iterations == 1
        assert keys[0] != keys[1]
    finally:
        await storage.close()


class _FileProvider(LLMProvider):
    def __init__(self):
        super().__init__()
        self.tool_results = []

    def get_default_model(self):
        return "isolated-test"

    async def chat(self, messages, tools=None, **kwargs):
        if messages[-1]["role"] == "tool":
            self.tool_results.append(messages[-1]["content"])
            return LLMResponse(content="done")
        text = str(messages[-1]["content"])
        if isinstance(messages[-1]["content"], list):
            text = "\n".join(part.get("text", "") for part in messages[-1]["content"])
        text = text.rsplit("\n\n", 1)[-1].strip()
        if text == "write":
            call = ToolCallRequest("write1", "write_file", {"path": "case-output.txt", "content": "private-case"})
        elif text == "external":
            call = ToolCallRequest("process1", "exec", {"command": "echo isolated"})
        else:
            call = ToolCallRequest("read1", "read_file", {"path": "case-output.txt"})
        return LLMResponse(tool_calls=[call], finish_reason="tool_calls")


@pytest.mark.asyncio
async def test_isolated_runtime_uses_real_tools_without_online_or_cross_case_files(tmp_path, monkeypatch):
    from echo_agent.agent.loop import AgentLoop
    from echo_agent.agent.browser.session import manager as browser_manager
    from unittest.mock import AsyncMock

    online = tmp_path / "online"
    online.mkdir()
    skills = SkillStore(user_dir=online / "skills")
    config = Config()
    config.knowledge.enabled = False
    config.permissions.approval.mode = "off"
    config.permissions.approval.unattended_policy = "allow_safe"
    config.storage.memory_dir = str(online / "memory")
    config.storage.logs_dir = str(online / "logs")
    config.storage.sessions_dir = str(online / "sessions")
    provider = _FileProvider()
    workspaces = []
    original_init = AgentLoop.__init__

    def capture_init(self, *args, **kwargs):
        original_init(self, *args, **kwargs)
        workspaces.append(self.workspace)

    monkeypatch.setattr(AgentLoop, "__init__", capture_init)
    close_browser = AsyncMock()
    monkeypatch.setattr(browser_manager, "close_all", close_browser)
    runner = IsolatedEvalRunner(config=config, provider=provider, skill_store=skills, parallel=1)
    report = await runner.run_dataset(EvalDataset([
        EvalCase(id="same", input="write", expected_tools=["write_file"], expected_contains=["done"]),
        EvalCase(id="same", input="read", expected_tools=["read_file"], expected_contains=["done"]),
    ]))
    assert report.passed_cases == 2, [(r.error, r.response) for r in report.results]
    assert "Written" in str(provider.tool_results[0])
    assert "No such file" in str(provider.tool_results[1])
    assert len(set(workspaces)) == 2
    assert all(not path.exists() for path in workspaces)
    assert sorted(p.name for p in online.iterdir()) == ["skills"]
    close_browser.assert_not_awaited()


@pytest.mark.asyncio
async def test_isolated_runtime_marks_blocked_external_tool_inconclusive(tmp_path, monkeypatch):
    from echo_agent.agent.tools.shell import ShellTool

    async def must_not_execute(*args, **kwargs):
        raise AssertionError("a subprocess must never execute in candidate evaluation")

    monkeypatch.setattr(ShellTool, "execute", must_not_execute)
    config = Config()
    config.knowledge.enabled = False
    config.permissions.approval.auto_approve = ["exec"]
    provider = _FileProvider()
    runner = IsolatedEvalRunner(config=config, provider=provider, skill_store=SkillStore(user_dir=tmp_path / "skills"))
    result = await runner.run_case(EvalCase(id="external", input="external"))
    assert result.inconclusive
    assert not result.passed
    assert result.metrics[-1].details["tools"] == ["exec"]


@pytest.mark.asyncio
async def test_stub_provider_cannot_supply_evidence_for_skill_promotion(tmp_path):
    from echo_agent.models.stub import StubProvider

    runner = IsolatedEvalRunner(config=Config(), provider=StubProvider("no model configured"),
                                skill_store=SkillStore(user_dir=tmp_path / "skills"))
    result = await runner.run_case(EvalCase(id="safety", input="do no harm"))
    assert result.inconclusive
    assert not result.passed
    assert "StubProvider" in result.error


@pytest.mark.asyncio
async def test_promotion_does_not_overwrite_live_edits_during_evaluation(tmp_path):
    storage = SQLiteBackend(tmp_path / "evolution.db")
    await storage.initialize()
    store = TrajectoryStore(storage)
    await store.init_schema()
    live = SkillStore(user_dir=tmp_path / "skills")
    live.create_skill("candidate", "---\nname: candidate\ndescription: original\n---\nOriginal")
    calls = 0

    def factory(*, skill_store):
        async def run_dataset(dataset):
            nonlocal calls
            calls += 1
            if calls == 2:
                live.patch_skill("candidate", "Original", "Operator edit")
            return EvalReport(results=[CaseResult(case_id=str(i), passed=(i < calls)) for i in range(3)],
                              total_cases=3, passed_cases=calls)
        return SimpleNamespace(run_dataset=run_dataset)

    gate = PromotionGate(eval_runner_factory=factory, eval_dataset_loader=lambda: EvalDataset([EvalCase(id="one")]),
                         skill_store=live, store=store, require_strict_improvement=False)
    candidate = SkillCandidate(operation="patch", skill_name="candidate",
                               proposed_patch_old="Original", proposed_patch_new="Candidate edit")
    await store.save_candidate(candidate)
    try:
        decision = await gate.evaluate(candidate)
        assert not decision.promoted
        assert "changed during evaluation" in decision.reason
        assert "Operator edit" in live.read_skill("candidate")
    finally:
        await storage.close()


@pytest.mark.asyncio
async def test_candidate_evaluation_is_staged_and_cancel_cleanup_is_durable(tmp_path):
    storage = SQLiteBackend(tmp_path / "evolution.db")
    await storage.initialize()
    store = TrajectoryStore(storage)
    await store.init_schema()
    live = SkillStore(user_dir=tmp_path / "skills")
    entered = asyncio.Event()
    staged_paths = []
    calls = 0

    def factory(*, skill_store=None):
        skill_store = skill_store or live
        async def run_dataset(dataset):
            nonlocal calls
            calls += 1
            staged_paths.append(skill_store.user_dir)
            assert not (live.user_dir / "candidate").exists()
            if calls == 2:
                assert (skill_store.user_dir / "candidate" / "SKILL.md").is_file()
                entered.set()
                await asyncio.Event().wait()
            return EvalReport(results=[CaseResult(case_id="one", passed=True)], total_cases=1, passed_cases=1)
        return SimpleNamespace(run_dataset=run_dataset)

    gate = PromotionGate(eval_runner_factory=factory, eval_dataset_loader=lambda: EvalDataset([EvalCase(id="one")]),
                         skill_store=live, store=store)
    candidate = SkillCandidate(operation="create", skill_name="candidate",
                               proposed_content="---\nname: candidate\ndescription: test\n---\nHello")
    await store.save_candidate(candidate)
    task = asyncio.create_task(gate.evaluate(candidate))
    try:
        await asyncio.wait_for(entered.wait(), timeout=3)
        task.cancel()
        with pytest.raises(asyncio.CancelledError):
            await task
        assert not (live.user_dir / "candidate").exists()
        assert all(not path.exists() for path in staged_paths)
        persisted = await store.get_candidate(candidate.id)
        assert persisted.status == "rejected"
        assert "cancelled" in persisted.rejected_reason
    finally:
        task.cancel()
        await asyncio.gather(task, return_exceptions=True)
        await storage.close()
