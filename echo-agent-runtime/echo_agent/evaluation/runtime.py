"""Disposable evaluation runtimes; no online sessions, files or tool clients.

Only workspace-confined file operations and skill metadata reads execute here.
Other tool schemas remain visible to measure attempted use, but their execution
is blocked and makes the measurement inconclusive. This is an evaluation
boundary, not an operating-system sandbox for arbitrary candidate code.
"""

from __future__ import annotations

import asyncio
import shutil
import tempfile
from pathlib import Path
from typing import Any

from echo_agent.config.schema import Config, StorageConfig
from echo_agent.evaluation.dataset import EvalCase
from echo_agent.evaluation.metrics import MetricResult
from echo_agent.evaluation.runner import CaseResult, EvalRunner
from echo_agent.models.provider import LLMProvider
from echo_agent.skills.store import SkillStore
from echo_agent.tools import Tool, ToolExecutionContext, ToolResult


_LOCAL_TOOLS = frozenset({
    "read_file", "write_file", "edit_file", "list_dir", "search_files", "patch",
    "read_spill", "skills_list", "skill_view",
})


def copy_skill_tree(source: Path, target: Path) -> None:
    """Copy skills without following links into an online/external filesystem."""
    if source.is_symlink() or any(p.is_symlink() for p in source.rglob("*")):
        raise ValueError(f"isolated evaluation cannot copy symlinked skills: {source}")
    shutil.copytree(source, target)


class _BlockedTool(Tool):
    def __init__(self, original: Tool, blocked: set[str]):
        self.name = original.name
        self.description = original.description
        self.parameters = original.parameters
        self.risk_level = original.risk_level
        self._blocked = blocked

    async def execute(self, params: dict[str, Any], ctx: ToolExecutionContext | None = None) -> ToolResult:
        self._blocked.add(self.name)
        return ToolResult(success=False, error="External/process tool execution is unavailable in isolated evaluation",
                          error_kind="business")


class IsolatedEvalRunner(EvalRunner):
    """Construct a fresh AgentLoop and real SQLite database for *each* case."""

    def __init__(self, *, config: Config, provider: LLMProvider,
                 skill_store: SkillStore, parallel: int = 3, timeout: int = 120,
                 providers: dict[str, LLMProvider] | None = None):
        super().__init__(None, parallel=parallel, timeout=timeout, provider=provider)
        self._requires_runtime_plugins = bool(config.plugins.required or config._runtime_extension is not None)
        self._config = Config.model_validate(config.model_dump())
        self._skills = skill_store
        self._providers = dict(providers) if providers is not None else None

    async def run_case(self, case: EvalCase) -> CaseResult:
        from echo_agent.agent.loop import AgentLoop
        from echo_agent.bus.queue import MessageBus
        from echo_agent.storage.sqlite import SQLiteBackend, _settle_cleanup

        if getattr(self._provider, "is_stub", False) is True:
            return self._inconclusive(case, "evaluation requires a configured model provider; StubProvider is not a model")
        # Runtime-extension semantics cannot be reproduced by simply disabling
        # a required plugin; refuse to score that comparison as a success.
        if self._requires_runtime_plugins:
            return self._inconclusive(case, "required runtime plugins need an isolated evaluation adapter")
        if self._providers is None and len(self._config.models.providers) > 1:
            return self._inconclusive(case, "multi-provider evaluation requires the configured provider registry")
        with tempfile.TemporaryDirectory(prefix="echo-eval-case-") as directory:
            workspace = Path(directory)
            config = self._config.model_copy(deep=True)
            config.workspace = str(workspace)
            # Absolute deployment paths must not escape the disposable root.
            config.storage = StorageConfig()
            config.gateway.media_cache_dir = "data/media_cache"
            config.multi_agent.audit_path = "data/logs/multi_agent.jsonl"
            config.knowledge.docs_dir = "data/knowledge"
            config.knowledge.index_path = "data/knowledge_index.json"
            config.artifacts.root_dir = "data/artifacts"
            config.tools.restrict_to_workspace = True
            config.tools.safe_write_root = ""
            config.tools.mcp_servers = {}
            config.plugins.enabled = False
            config.evolution.enabled = False
            config.memory.vector_enabled = False
            config.memory.rerank_enabled = False
            config.agent.heartbeat.enabled = False
            # Evaluation must not register global exporters or send spans to the
            # live runtime's telemetry endpoint.
            config.observability.otel_enabled = False
            try:
                copy_skill_tree(self._skills.user_dir, workspace / "data" / "skills")
                config.skills.disabled = list(self._skills._disabled)
                builtin = self._skills._builtin_dir
                if builtin is not None:
                    copy_skill_tree(builtin, workspace / "builtin_skills")
                    config.skills.skills_dir = str(workspace / "builtin_skills")
                else:
                    # Prevent packaged skill auto-discovery from introducing a
                    # different skill library than the one the gate snapshotted.
                    config.skills.skills_dir = str(workspace / "no_builtin_skills")
                config.skills.external_dirs = []
                for index, source in enumerate(self._skills._external_dirs):
                    target = workspace / f"external_skills_{index}"
                    copy_skill_tree(source, target)
                    config.skills.external_dirs.append(str(target))
            except (OSError, ValueError) as error:
                return self._inconclusive(case, str(error))
            storage = SQLiteBackend(workspace / config.storage.database_path)
            await storage.initialize()
            agent = None
            blocked: set[str] = set()
            try:
                from echo_agent.models.router import ModelRouter

                router = ModelRouter(config.models, default_context_window=config.session.context_window_tokens)
                registrations = self._providers
                if registrations is None:
                    name = config.models.providers[0].name if config.models.providers else "default"
                    registrations = {name: self._provider}
                for name, provider in registrations.items():
                    router.register_provider(name, provider)
                agent = AgentLoop(MessageBus(), config, self._provider, workspace, storage=storage, router=router)
                for name in agent.tools.tool_names:
                    if name not in _LOCAL_TOOLS:
                        original = agent.tools.get(name)
                        agent.tools.register(_BlockedTool(original, blocked), replace=True)
                await agent.start()
                candidates = router.route_candidates("evaluation", case.input)
                judge_provider = candidates[0][1] if candidates else self._provider
                judge_model = candidates[0][2].model if candidates else config.models.default_model
                result = await EvalRunner(agent, timeout=self._timeout, provider=judge_provider,
                                          judge_model=judge_model or None).run_case(case)
                # Approval can reject a nonlocal call before the replacement
                # tool executes. Such an attempt still cannot establish that
                # the intended external operation works in this environment.
                blocked.update(name for name in result.tools_used if name not in _LOCAL_TOOLS)
                if blocked:
                    result.metrics.append(MetricResult(
                        name="isolated_tools", score=0.0, passed=False, inconclusive=True,
                        details={"reason": "unverified external/process tools", "tools": sorted(blocked)},
                    ))
                    result.passed = False
                return result
            finally:
                # Fully settle teardown before deleting files or closing SQLite,
                # even if timeout/shutdown cancels this evaluation again.
                cancellation: asyncio.CancelledError | None = None
                try:
                    if agent is not None:
                        cancellation = await _settle_cleanup(agent.stop(close_shared_browser_sessions=False))
                finally:
                    close_cancellation = await _settle_cleanup(storage.close())
                    cancellation = cancellation or close_cancellation
                if cancellation is not None:
                    raise cancellation

    @staticmethod
    def _inconclusive(case: EvalCase, detail: str) -> CaseResult:
        return CaseResult(case_id=case.id, category=case.category, error=detail,
                          metrics=[MetricResult(name="isolation", score=0.0, passed=False,
                                                inconclusive=True, details={"reason": detail})])
