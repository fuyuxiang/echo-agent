"""Agent loop — the core processing engine.

Receives events → builds context → calls LLM → executes tools → sends responses.
Orchestrates pipeline stages: context building, inference, and response finalization.
"""

from __future__ import annotations

import asyncio
import time
import uuid
from collections import OrderedDict
from pathlib import Path
from typing import Any

from loguru import logger

from echo_agent.agent.approval_gate import ApprovalGate
from echo_agent.agent.consolidation import ConsolidationWorker
from echo_agent.agent.context import ContextBuilder
from echo_agent.permissions.allowlist import ApprovalAllowlist
from echo_agent.agent.compression import ConversationCompressor
from echo_agent.agent.pipeline.context_stage import ContextStage
from echo_agent.agent.pipeline.inference_stage import InferenceStage
from echo_agent.agent.pipeline.response_stage import ResponseStage, _is_ephemeral_session
from echo_agent.agent.tools.circuit_breaker import ToolCircuitBreaker
from echo_agent.agent.tools.registry import ToolRegistry
from echo_agent.bus.events import EventType, InboundEvent, OutboundEvent
from echo_agent.bus.queue import MessageBus
from echo_agent.config.schema import Config
from echo_agent.cost.budget import CostTracker
from echo_agent.memory.consolidator import MemoryConsolidator
from echo_agent.memory.service import MemoryService
from echo_agent.memory.store import MemoryStore
from echo_agent.models.inference import InferenceController
from echo_agent.models.provider import LLMProvider
from echo_agent.models.router import ModelRouter
from echo_agent.observability.monitor import TraceLogger
from echo_agent.permissions.manager import ApprovalManager, ApprovalStatus, CredentialManager
from echo_agent.runtime_paths import bundled_skills_dir
from echo_agent.session.manager import Session, SessionManager
from echo_agent.skills.store import SkillStore
from echo_agent.agent.streaming import (
    ProcessResult as _ProcessResult,
    TokenStreamPublisher as _TokenStreamPublisher,
)
from echo_agent.agent.progress_heartbeat import ProgressHeartbeat, SharedActivityState
from echo_agent.agent.degraded_notice import GENERIC_FALLBACK_TEXT


def _resolve_builtin_skills_dir(workspace: Path, configured_path: str) -> Path | None:
    raw_path = Path(configured_path).expanduser()
    candidates: list[Path] = []
    if raw_path.is_absolute():
        candidates.append(raw_path)
    else:
        candidates.append(workspace / raw_path)
        bundled = bundled_skills_dir()
        if bundled:
            candidates.append(bundled)
    for candidate in candidates:
        if candidate.exists():
            return candidate
    return None


_EMBED_CIRCUIT_THRESHOLD = 3


def _embed_model_identity(provider: Any, emb_model: str | None) -> str:
    """Stable embedding-model id: unwrap transport wrappers (rate-limit /
    credential-pool) to the concrete provider class, then append the model.

    The id is persisted in the vectors table and compared on startup to detect
    a genuine embedding-model change. Deriving it from the OUTER wrapper class
    (e.g. RateLimitedProvider) would flip the id whenever rate-limiting or
    credential pooling is toggled between runs, wrongly marking every stored
    vector stale and forcing a full re-embed even though the model never
    changed. Unwrapping to the real provider keeps the id tied to what actually
    determines the embedding space."""
    inner = provider
    # Wrappers expose the delegate as `_inner`; follow the chain to the bottom.
    for _ in range(8):  # bounded guard against accidental cycles
        nxt = getattr(inner, "_inner", None)
        if nxt is None or nxt is inner:
            break
        inner = nxt
    return f"{type(inner).__name__.lower()}:{emb_model or 'default'}"


class _ProviderEmbedFn:
    """provider embedding 入口的熔断包装。连续失败达阈值后停止调用底层、
    返回 []（对检索与 flush 两条路径都安全），并只告警一次。止损后由重启
    重新决策 backend（不做运行时热切换，避免维度污染）。"""

    def __init__(self, provider: Any, model: str | None):
        self._provider = provider
        self._model = model
        self._consecutive_failures = 0
        self.tripped = False

    async def __call__(self, text: str) -> list[float]:
        if self.tripped:
            return []
        try:
            result = await self._provider.embed(text, model=self._model)
        except Exception as e:
            logger.debug("Provider embedding raised: {}", e)
            result = None
        if result:
            self._consecutive_failures = 0
            return result
        self._consecutive_failures += 1
        if self._consecutive_failures >= _EMBED_CIRCUIT_THRESHOLD and not self.tripped:
            self.tripped = True
            logger.warning(
                "Provider embedding failed {} times consecutively; embedding backfill "
                "paused. Fix the endpoint or set memory.embedding_backend=local and "
                "restart. Vector search degrades to keyword-only until then.",
                _EMBED_CIRCUIT_THRESHOLD,
            )
        return []


def resolve_embed_fallback(
    embed_provider, emb_model, local_model_name, local_load_timeout=60.0, hf_endpoint="",
    cache_dir="", max_load_attempts=5, retry_backoff=30.0,
):
    """Resolve the embedding tier: provider-backed when available, else the
    local fastembed fallback (zero-config vector search), else nothing.

    Returns (embed_fn | None, embed_model_id, local_embedder | None)."""
    if embed_provider is not None:
        async def _embed(text: str, _p=embed_provider, _model=emb_model) -> list[float]:
            result = await _p.embed(text, model=_model)
            return result or []
        model_id = _embed_model_identity(embed_provider, emb_model)
        return _embed, model_id, None

    if local_model_name:
        from echo_agent.memory.local_embed import LocalEmbedder
        resolved_cache = str(Path(cache_dir).expanduser()) if cache_dir else ""
        local = LocalEmbedder(
            local_model_name, load_timeout_seconds=local_load_timeout,
            hf_endpoint=hf_endpoint, cache_dir=resolved_cache,
            max_load_attempts=max_load_attempts, retry_backoff_seconds=retry_backoff,
        )
        if local.available:
            logger.info(
                "No embed-capable provider; using local embedding fallback '{}'",
                local_model_name,
            )
            return local.embed, local.model_id, local
        logger.warning(
            "fastembed not importable; vector search degrades to keyword mode",
        )
        return None, "", None

    logger.warning(
        "No embedding-capable provider registered and local fallback disabled; "
        "vector search and hybrid retrieval will degrade to keyword mode"
    )
    return None, "", None


def pick_embed_candidate(backend, provider, router, emb_model):
    """挑候选 embed provider（不发网络）。local 模式恒不挑 provider。
    返回 (candidate_provider | None, resolved_model)。"""
    if backend == "local":
        return None, emb_model
    if provider.supports_embed():
        return provider, emb_model
    if router is not None:
        cand, routed_model = router.find_embed_provider(emb_model or "")
        if cand is not None:
            return cand, (routed_model or emb_model)
    return None, emb_model


async def probe_embed_provider(provider, model, timeout) -> int:
    """发一次探针 embedding。成功且非空返回维度(>0)，否则返回 0。"""
    try:
        vec = await asyncio.wait_for(provider.embed("ping", model=model), timeout=timeout)
    except Exception as e:
        logger.info("Embedding probe failed, will fall back to local: {}", e)
        return 0
    return len(vec) if vec else 0


def _should_publish_reply(event: InboundEvent, final_text: str) -> bool:
    """Reply convergence gate. Normal rounds always publish (behaviour unchanged).

    Inspection rounds (metadata _inspection=True) are silenced when the agent's
    final reply is empty or carries the INSPECT_OK sentinel — honouring the
    "no news, stay silent" contract. The sentinel check only applies to
    inspection rounds, so ordinary replies that happen to contain the literal
    "INSPECT_OK" are never suppressed.
    """
    if not event.metadata.get("_inspection"):
        return True
    from echo_agent.agent.inspection.policy import should_deliver

    return should_deliver(final_text)


class AgentLoop:
    """Core processing engine that ties all subsystems together."""

    _MAX_TOOL_RESULT_CHARS = 16000

    def __init__(
        self,
        bus: MessageBus,
        config: Config,
        provider: LLMProvider,
        workspace: Path,
        router: ModelRouter | None = None,
        scheduler: Any = None,
        storage: Any = None,
        task_manager: Any = None,
        workflow_engine: Any = None,
    ):
        self.bus = bus
        from echo_agent.agent.cognitive_emitter import CognitiveEmitter
        self.cognitive_emitter = CognitiveEmitter(bus)
        self.config = config
        self.provider = provider
        self.router = router
        self.workspace = workspace
        # 阶段 B(start 内 _resolve_embed_and_index)需要 storage 才能定案 backend
        # 并构造 VectorIndex/依赖它的消费者,故在此固化引用。
        self._storage = storage
        try:
            provider_default_model = provider.get_default_model()
        except Exception as e:
            logger.debug("Failed to get default model from provider: {}", e)
            provider_default_model = ""
        self._default_model = config.models.default_model or provider_default_model or ""

        self.sessions = SessionManager(
            sessions_dir=workspace / config.storage.sessions_dir,
            expiry_hours=config.session.expiry_hours,
            storage=storage,
        )
        self.memory = MemoryStore(
            memory_dir=workspace / config.storage.memory_dir,
            max_user=config.memory.max_user_memories,
            max_env=config.memory.max_env_memories,
            decay_half_life_days=config.memory.importance_decay_days,
            storage=storage,
            scope_policy=config.memory.scope_policy,
            contradiction_scan_on_store=config.memory.contradiction_scan_on_store,
            archival_threshold=config.memory.archival_threshold,
            forget_threshold=config.memory.forget_threshold,
            lineage_max_versions=config.memory.lineage_max_versions,
            lineage_retention_days=config.memory.lineage_retention_days,
            # R1 Task8:唯一写口。所有写者经 self._memory_service 单例(下方构造)
            # 走八步写序,故 store 置 service_only,外部绕过 service 直写即软告警。
            service_only=config.memory.enabled,
        )
        # R1 Task8:统一装配的 MemoryService 单例——所有写者(工具/reviewer/REST/
        # promotion/reflection/detector/归档)共享此实例,失效/flush/审计集中一处,
        # 审计统一落 logs_dir/memory_audit.jsonl(复用 tool_audit.jsonl 同目录)。
        # 收口前各入口就近 new 独立 service,失效/审计各自为政;此处收敛为单例。
        self._memory_service = MemoryService(
            self.memory,
            invalidate_fn=self._invalidate_memory_caches,
            flush_fn=self.memory.flush_pending_embeds,
            audit_path=workspace / config.storage.logs_dir / "memory_audit.jsonl",
            allow_env_writes=config.memory.allow_model_environment_writes,
        )
        self.tools = ToolRegistry(
            audit_log_path=workspace / config.storage.logs_dir / "tool_audit.jsonl",
            config=config,
        )
        from echo_agent.gateway.media import MediaCache
        media_cache = MediaCache(
            cache_dir=workspace / config.gateway.media_cache_dir,
            max_size_mb=config.gateway.media_cache_max_mb,
        )
        from echo_agent.agent.media.understanding import default_understanders
        understanders = default_understanders(
            config.media_understanding,
            transcription_api_key=config.channels.transcription_api_key,
            vision_provider=provider,
        )
        self.context = ContextBuilder(
            workspace,
            media_cache=media_cache,
            doc_enabled=config.tools.inbound_document_enabled,
            doc_max_chars=config.tools.inbound_document_max_chars,
            understanders=understanders,
        )
        self.compressor = ConversationCompressor(
            config=config.compression,
            context_window_tokens=config.session.context_window_tokens,
            provider=provider,
            default_model=self._default_model,
            storage=storage,
            router=router,
        )
        try:
            from echo_agent.models.tokenizer import TokenCounter
            provider_name = getattr(config.models, "default_provider", "") or ""
            if not provider_name and config.models.providers:
                provider_name = config.models.providers[0].name
            if self._default_model:
                tc = TokenCounter.for_model(provider_name, self._default_model)
                if hasattr(self.compressor, 'set_token_counter'):
                    self.compressor.set_token_counter(tc)
        except Exception as e:
            logger.debug("Tokenizer initialization skipped: {}", e)
        self.approval = ApprovalManager(
            require_approval=config.permissions.approval.require_approval,
            auto_approve=config.permissions.approval.auto_approve,
            auto_deny=config.permissions.approval.auto_deny,
            default_policy=config.permissions.approval.default_policy,
            store_path=workspace / "data" / "approvals.json",
        )
        from echo_agent.agent.clarify_manager import ClarifyManager
        self.clarify = ClarifyManager()
        from echo_agent.agent.interrupt_manager import InterruptManager
        self.interrupt = InterruptManager()
        self.inference = InferenceController()
        if config.permissions.approval.require_approval:
            from echo_agent.models.inference import InferenceConstraints
            self.inference.set_constraints(InferenceConstraints(
                require_confirmation_for=list(config.permissions.approval.require_approval),
                blocked_tools=list(config.permissions.approval.auto_deny),
            ))
        self.approval_gate = ApprovalGate(
            config=config,
            approval=self.approval,
            inference=self.inference,
            bus=bus,
            provider=provider,
            registry=self.tools,
            router=router,
            cognitive_emitter=self.cognitive_emitter,
            allowlist=ApprovalAllowlist(
                store_path=self.workspace / "data" / "approval_allowlist.json",
            ),
        )
        self.credentials = CredentialManager(
            store_path=workspace / "data" / "credentials.json",
            encryption_key_env=config.credentials.encryption_key_env,
            require_encryption=config.credentials.require_encryption,
            key_path=workspace / ".credential_key",
        )
        self.tracer = TraceLogger(
            logs_dir=workspace / config.storage.logs_dir,
            enabled=config.observability.trace_enabled,
            max_trace_files=config.observability.max_trace_files,
        )
        self.consolidator = MemoryConsolidator(
            memory_store=self.memory,
            llm_call=self.provider.chat_with_retry,
            context_window_tokens=config.session.context_window_tokens,
            consolidation_threshold=config.memory.consolidation_threshold,
        )

        self._working_memories: OrderedDict[str, Any] = OrderedDict()
        self._hybrid_retriever = None
        self._vector_index = None
        self._embed_fn = None
        self._local_embedder = None
        self._embed_model_id = ""
        self._episodic = None
        # 两阶段接线的默认值：memory 关闭时 _init_advanced_memory 不跑，
        # 但 start() 仍会调 _resolve_embed_and_index，这些属性须先就位。
        self._embed_backend = config.memory.embedding_backend
        self._embed_candidate: tuple[Any | None, str | None] = (None, None)
        self._contradiction_detector = None
        if config.memory.enabled:
            self._init_advanced_memory(config, storage)

        self.planner = None
        self._plan_run_store = None
        if config.planning.enabled:
            from echo_agent.agent.planning import AgentPlanner
            self.planner = AgentPlanner(
                llm_call=self.provider.chat_with_retry,
                default_strategy=config.planning.default_strategy,
                max_tree_depth=config.planning.max_tree_depth,
                reflection_enabled=config.planning.reflection_enabled,
                max_branches=config.planning.max_branches,
            )
            if storage is not None:
                from echo_agent.agent.planning.plan_run_store import PlanRunStore
                self._plan_run_store = PlanRunStore(storage)

        self._telemetry = None
        if config.observability.otel_enabled:
            from echo_agent.observability.telemetry import TelemetryManager
            self._telemetry = TelemetryManager(
                service_name=config.observability.otel_service_name,
                otel_endpoint=config.observability.otel_endpoint,
                export_interval_ms=config.observability.otel_export_interval_ms,
            )
            self._telemetry.setup()
            if self._telemetry.available:
                self.tracer.set_otel_tracer(self._telemetry.get_tracer())
        self.mcp_manager: Any = None
        self.knowledge: Any = None
        self.evolution: Any = None
        if config.knowledge.enabled:
            from echo_agent.knowledge import KnowledgeIndex
            self.knowledge = KnowledgeIndex(
                workspace=workspace,
                docs_dir=config.knowledge.docs_dir,
                index_path=config.knowledge.index_path,
                chunk_size=config.knowledge.chunk_size,
                chunk_overlap=config.knowledge.chunk_overlap,
                allowed_extensions=config.knowledge.allowed_extensions,
            )
            self.knowledge.ensure_ready(auto_index=config.knowledge.auto_index)

        skills_dir = _resolve_builtin_skills_dir(workspace, config.skills.skills_dir)
        self.skill_store = SkillStore(
            user_dir=workspace / "data" / "skills",
            builtin_dir=skills_dir,
            external_dirs=[Path(d) for d in config.skills.external_dirs],
            disabled=config.skills.disabled,
        )

        self._running = False
        self._max_iterations = config.agent.max_iterations
        self._nudge_interval = config.skills.creation_nudge_interval
        self._memory_nudge_interval = config.memory.memory_nudge_interval
        self._tool_iters_since_skill_check = 0
        self._tool_iters_since_memory_check = 0
        self._snapshot_enabled = config.memory.snapshot_enabled
        self._memory_snapshots: OrderedDict[str, str] = OrderedDict()
        self._memory_snapshot_ids: "OrderedDict[str, frozenset[str]]" = OrderedDict()
        self._memory_snapshot_meta: dict[str, tuple[str, int]] = {}
        self._scope_versions: dict[str, int] = {}
        self._retrieval_cache: OrderedDict[str, Any] = OrderedDict()
        self._max_cached_sessions = 200
        from echo_agent.agent.background import BackgroundScheduler
        self._bg_scheduler = BackgroundScheduler(config.execution.max_background_tasks)
        self._state_lock = asyncio.Lock()
        self._plugin_manager: Any = None
        # Kept so a finished CRON turn can write its real outcome back to the job
        # (see _on_inbound); the scheduler otherwise only ever sees "queued".
        self._scheduler = scheduler
        # Retained for the dashboard task API (gateway/api/tasks.py); previously
        # only forwarded into tool discovery and never held on the instance.
        self._task_manager = task_manager
        # Retained so the REST transition endpoint can advance workflows after
        # a terminal task transition — the same closing-the-loop hook TaskTool got.
        self._workflow_engine = workflow_engine
        self._register_tools(scheduler=scheduler, task_manager=task_manager, workflow_engine=workflow_engine)
        self._setup_delegation()

        # Pipeline stages
        self._circuit_breaker = ToolCircuitBreaker(
            failure_threshold=config.circuit_breaker.failure_threshold,
            recovery_seconds=config.circuit_breaker.recovery_seconds,
            half_open_max=config.circuit_breaker.half_open_max,
        )
        self._consolidation_worker = ConsolidationWorker(
            sessions=self.sessions,
            consolidator=self.consolidator,
            sleep_consolidation=config.memory.sleep_consolidation,
        )
        self._context_stage = ContextStage(
            config=config,
            sessions=self.sessions,
            memory=self.memory,
            compressor=self.compressor,
            context_builder=self.context,
            skill_store=self.skill_store,
            knowledge=self.knowledge,
            hybrid_retriever=getattr(self, '_hybrid_retriever', None),
            planner=self.planner,
            inference=self.inference,
            working_memories=self._working_memories,
            memory_snapshots=self._memory_snapshots,
            memory_snapshot_ids=self._memory_snapshot_ids,
            put_snapshot=self.put_memory_snapshot,
            memory_snapshot_meta=self._memory_snapshot_meta,
            scope_version_fn=self._scope_version,
            snapshot_enabled=self._snapshot_enabled,
            memory_enabled=config.memory.enabled,
            tool_definitions_fn=self.tools.get_definitions,
            episodic=self._episodic,
            narrative_episode_count=config.memory.narrative_episode_count,
            plan_run_store=self._plan_run_store,
            bus=bus,
            retrieval_cache_get=self._get_retrieval_cache,
            retrieval_on_miss=config.memory.retrieval_on_miss,
            retrieval_miss_timeout=config.memory.retrieval_miss_timeout_seconds,
            cache_ttl=config.memory.cache_ttl_seconds,
            cache_jaccard_min=config.memory.cache_jaccard_min,
            cognitive_emitter=self.cognitive_emitter,
        )
        self._cost_tracker = CostTracker(
            storage=storage,
            enabled=config.cost.enabled,
            daily_budget_usd=config.cost.daily_budget_usd,
            soft_ratio=config.cost.soft_threshold_ratio,
            pricing_overrides=config.cost.pricing_overrides,
        )
        self._inference_stage = InferenceStage(
            config=config,
            bus=bus,
            provider=provider,
            router=self.router,
            tools=self.tools,
            approval_gate=self.approval_gate,
            credentials=self.credentials,
            tracer=self.tracer,
            telemetry=self._telemetry,
            inference=self.inference,
            circuit_breaker=self._circuit_breaker,
            default_model=self._default_model,
            max_iterations=self._max_iterations,
            planner=self.planner,
            plan_run_store=self._plan_run_store,
            cost_tracker=self._cost_tracker,
            cognitive_emitter=self.cognitive_emitter,
            compressor=self.compressor,
            memory_store=self.memory,
            clarify_manager=self.clarify,
            interrupt_manager=self.interrupt,
        )
        # Retrieval prefetcher: after each reply ResponseStage fires this on the
        # DISCARDABLE tier to warm the next turn's cache. Needs _hybrid_retriever
        # (set by _init_advanced_memory above) and _put_retrieval_cache, both
        # already initialized at this point.
        from echo_agent.memory.prefetch import RetrievalPrefetcher

        def _knowledge_fetch(query: str, user_id: str) -> str:
            # SYNC CPU-bound scan; the prefetcher runs this in an executor.
            results = self.knowledge.search(
                query, limit=config.knowledge.max_results, user_id=user_id
            )
            return self.knowledge.format_results(results)

        self._prefetcher = (
            RetrievalPrefetcher(
                # limit=8 matches the inline sync path (5 memory + 3 episode)
                # now that episodes ride the same retrieve() call.
                self._hybrid_retriever, self._put_retrieval_cache, limit=8,
                knowledge_fetch=_knowledge_fetch if self.knowledge else None,
            )
            if self._hybrid_retriever
            else None
        )
        # Skill admission gate — always-on, independent of evolution.enabled.
        # Uses its own TrajectoryStore over the shared storage backend so skill
        # distillation governance works even when the evolution engine is off.
        # Schema init is deferred to start() (init_schema is async; doing it as a
        # fire-and-forget in __init__ would race the first skill review).
        self._skill_admission = None
        self._skill_candidate_store = None
        if storage is not None:
            from echo_agent.evolution.store import TrajectoryStore
            from echo_agent.skills.admission import SkillAdmission
            self._skill_candidate_store = TrajectoryStore(storage)
            self._skill_admission = SkillAdmission(
                skill_store=self.skill_store,
                candidate_store=self._skill_candidate_store,
                policy=config.skills.admission_policy,
                auto_write_risk=config.skills.auto_write_risk,
            )
        self._response_stage = ResponseStage(
            config=config,
            sessions=self.sessions,
            memory=self.memory,
            provider=provider,
            consolidation_worker=self._consolidation_worker,
            default_model=self._default_model,
            spawn_fn=self._spawn_background,
            clear_memory_snapshot_fn=self._clear_memory_snapshot,
            skill_store=self.skill_store,
            skill_admission=self._skill_admission,
            working_memories=self._working_memories,
            prefetcher=self._prefetcher,
            scope_version_fn=self._scope_version,
            invalidate_memory_caches_fn=self._invalidate_memory_caches,
            memory_enabled=config.memory.enabled,
            # R1 Task8:Reviewer 经 ResponseStage 后台 review 注入 loop 单例 service。
            memory_service=self._memory_service,
        )

    def _register_tools(self, scheduler: Any = None, task_manager: Any = None, workflow_engine: Any = None) -> None:
        from echo_agent.agent.tools import discover_tools
        all_tools = discover_tools(
            config=self.config,
            workspace=self.workspace,
            bus=self.bus,
            provider=self.provider,
            scheduler=scheduler,
            session_manager=self.sessions,
            skill_store=self.skill_store,
            # memory.enabled 是总开关：关闭时连 memory_store 都不传，
            # discover_tools 的 `if memory_store:` 门控即不注册 memory 工具；
            # 配套的矛盾检测/失效回调在关闭时一并传 None，避免半开状态。
            memory_store=self.memory if self.config.memory.enabled else None,
            contradiction_detector=(
                getattr(self, "_contradiction_detector", None)
                if self.config.memory.enabled else None
            ),
            task_manager=task_manager,
            workflow_engine=workflow_engine,
            knowledge_index=self.knowledge,
            approval=self.approval,
            clarify_manager=self.clarify,
            memory_invalidate_fn=(
                self._invalidate_memory_caches if self.config.memory.enabled else None
            ),
            # R1 Task8:MemoryTool 注入 loop 单例 service,不再就近 new。
            memory_service=(
                self._memory_service if self.config.memory.enabled else None
            ),
        )
        for tool in all_tools:
            self.tools.register(tool)

        # Startup diagnostics: report tool readiness
        report = self.tools.get_readiness_report()
        not_ready = [(name, reason) for name, ready, reason in report if not ready]
        if not_ready:
            logger.warning("Tools not ready: {}", ", ".join(f"{n} ({r})" for n, r in not_ready))
        else:
            logger.info("All {} registered tools are ready", len(report))

    def _init_advanced_memory(self, config: Config, storage: Any) -> None:
        """初始化高级记忆子系统：分层记忆、向量索引、混合检索、矛盾检测。"""
        from echo_agent.memory.tiers import EpisodicManager, SemanticManager, ArchivalManager

        forgetting = self.memory.forgetting_curve

        episodic = EpisodicManager(storage) if storage else None
        # R1 Task8:晋升(SemanticManager)与归档/遗忘删除(ArchivalManager)均注入
        # loop 的 _memory_service 单例,失效/flush/审计统一收敛。
        semantic = SemanticManager(self._memory_service)
        archival = ArchivalManager(storage, service=self._memory_service) if storage else None

        self._episodic = episodic
        self.consolidator.set_episodic_manager(episodic)
        self.consolidator.set_semantic_manager(semantic)
        self.consolidator.set_forgetting_curve(forgetting)
        self.consolidator.set_archival_manager(archival)

        # 阶段 A：只挑候选 provider（不发网络、不构造索引、不构造依赖向量的消费者）。
        # 最终 backend 由 start() 内的探针在 _resolve_embed_and_index 定案，
        # VectorIndex / embed_fn / 矛盾检测 / HybridRetriever / reflection 一并在那时构造。
        # 阶段 A 后至 start() 前，_vector_index / _embed_fn 保持 None 是安全的
        # （下游消费者与 retrieval.py 均有 None 判断），生产环境处理事件必经 start()。
        vector_index = None
        embed_fn = None
        self._local_embedder = None
        self._embed_model_id = ""
        self._embed_backend = config.memory.embedding_backend
        self._embed_candidate = (None, None)
        # 依赖向量的消费者推迟到阶段 B 构造；阶段 A 先把属性初始化好，
        # 使 __init__ 后续（_register_tools / _context_stage / prefetcher）拿到确定的初值。
        self._contradiction_detector = None
        self._hybrid_retriever = None
        if config.memory.vector_enabled and storage:
            emb_model = config.memory.embedding_model or None
            self._embed_candidate = pick_embed_candidate(
                self._embed_backend, self.provider, self.router, emb_model,
            )
        self._vector_index = vector_index
        self._embed_fn = embed_fn

    def _setup_delegation(self) -> None:
        if not self.config.multi_agent.enabled:
            return
        from echo_agent.agent.multi_agent.registry import WorkerRegistry
        from echo_agent.agent.tools.delegate import DelegateTool

        worker_registry = WorkerRegistry.from_config(self.config.multi_agent)
        audit_path = Path(self.config.multi_agent.audit_path).expanduser()
        if not audit_path.is_absolute():
            audit_path = self.workspace / audit_path

        delegate_tool = DelegateTool(
            provider=self.provider,
            model_router=self.router,
            tool_registry=self.tools,
            worker_registry=worker_registry,
            approval_gate=self.approval_gate,
            credentials=self.credentials,
            audit_path=audit_path,
            max_depth=self.config.multi_agent.max_depth,
            max_parallel_workers=self.config.multi_agent.max_parallel_workers,
            max_worker_iterations=self.config.multi_agent.max_iterations,
            default_model=self._default_model,
        )
        self.tools.register(delegate_tool)

        # spawn_task shares delegate's execution engine so its background worker
        # runs real tool calls (exec/write_file/cronjob) through the approval
        # flow, instead of being a tool-less completion that only "plans".
        from echo_agent.agent.tools.delegate import SpawnTool
        spawn_tool = SpawnTool(
            provider=self.provider,
            bus=self.bus,
            tool_registry=self.tools,
            approval_gate=self.approval_gate,
            credentials=self.credentials,
            model_router=self.router,
            default_model=self._default_model,
            max_iterations=self.config.multi_agent.max_iterations,
        )
        self.tools.register(spawn_tool)
        logger.info("Delegation enabled with {} worker templates", len(worker_registry.list()))

    def set_plugin_manager(self, manager: Any) -> None:
        """Attach the plugin manager after bootstrap. Passes hook_registry to InferenceStage."""
        self._plugin_manager = manager
        self._inference_stage.set_hook_registry(manager.hooks)

    def set_evolution_engine(self, engine: Any) -> None:
        """Attach the evolution engine after bootstrap. Registers its tools and shares hooks."""
        self.evolution = engine
        if engine is None:
            return
        # Register agent-facing evolution tools so the LLM can introspect / trigger.
        try:
            from echo_agent.evolution.tools import build_evolution_tools
            for tool in build_evolution_tools(engine):
                self.tools.register(tool)
        except Exception as e:
            logger.warning("Failed to register evolution tools: {}", e)

    @property
    def is_running(self) -> bool:
        return self._running

    # ── Public accessors for the dashboard/gateway API ───────────────────────
    # These back the read paths in gateway/api/{analytics,cron_api,tasks,logs}.
    # The underlying state is held privately; exposing it via properties keeps a
    # stable public contract without leaking mutation access to internals.

    @property
    def cost_tracker(self) -> Any:
        """CostTracker backing the analytics API (daily/skill/channel usage)."""
        return self._cost_tracker

    @property
    def scheduler(self) -> Any:
        """Cron scheduler backing the cron API; None when scheduling is off."""
        return self._scheduler

    @property
    def task_manager(self) -> Any:
        """TaskManager backing the tasks API; None when no manager was wired."""
        return self._task_manager

    @property
    def workflow_engine(self) -> Any:
        """WorkflowEngine for DAG advance on task completion; None when unwired."""
        return self._workflow_engine

    @property
    def log_buffer(self) -> Any:
        """Recent structured log records backing the logs API.

        Sourced from the process-global buffer, so it reflects all logging
        regardless of which subsystem emitted it.
        """
        from echo_agent.observability.log_buffer import get_log_buffer

        return get_log_buffer()

    def _resolved_vector_dimensions(self) -> int:
        """Dimension for knowledge attach: explicit config wins, then the live
        index, then the local model's known dim, then the legacy default."""
        if self.config.memory.vector_dimensions:
            return self.config.memory.vector_dimensions
        if self._vector_index is not None and self._vector_index.dimensions:
            return self._vector_index.dimensions
        if self._local_embedder is not None and self._local_embedder.dimensions:
            return self._local_embedder.dimensions
        return 1536

    async def _resolve_embed_and_index(self, storage: Any) -> None:
        """阶段 B：探针定案 backend，构造 VectorIndex 与依赖它的消费者。

        由 start() 在向量初始化前调用。provider 模式探针失败抛 RuntimeError（不回退），
        auto 模式探针失败静默回退 fastembed，local 模式（候选恒为 None）直接走本地兜底。
        """
        config = self.config
        # memory.enabled 关闭时,阶段 A 的 _init_advanced_memory 整段被跳过,
        # 原实现下不存在任何向量索引/消费者,这里同样短路以保持行为一致。
        if not config.memory.enabled:
            return
        # vector_enabled=False 或无 storage 时,探针+建索引整段跳过,但消费者仍需
        # 按原语义接线(关键词模式):改造前 HybridRetriever/矛盾检测/reflection/预取
        # 的构造在向量块之外,只受各自开关+storage 控制。此处以 vector_index=None、
        # embed_fn=None 调用 _wire_vector_consumers,HybridRetriever 退化为 BM25。
        if not (config.memory.vector_enabled and storage):
            self._wire_vector_consumers(None, None)
            return
        candidate, emb_model = self._embed_candidate
        embed_fn = None
        self._embed_model_id = ""
        self._local_embedder = None
        probe_dim = 0

        use_provider = False
        if candidate is not None:
            dim = await probe_embed_provider(
                candidate, emb_model, config.memory.embed_timeout_seconds,
            )
            if dim > 0:
                use_provider = True
                probe_dim = dim
            elif self._embed_backend == "provider":
                raise RuntimeError(
                    "memory.embedding_backend=provider but the embedding probe "
                    "failed; fix the endpoint or switch to auto/local."
                )
            else:
                use_provider = False  # auto：静默回退 fastembed
        elif self._embed_backend == "provider":
            # provider 模式却没挑到任何 embed 候选（主 provider 无 embed 能力且无路由）：
            # 契约要求强制 provider、不回退，这里同样报错而非静默降级。
            raise RuntimeError(
                "memory.embedding_backend=provider but no embed-capable provider "
                "is available; register one or switch to auto/local."
            )

        if use_provider:
            # _ProviderEmbedFn 失败返回 []（非 None），且连续失败熔断，止损后靠重启重决策。
            embed_fn = _ProviderEmbedFn(candidate, emb_model)
            self._embed_model_id = _embed_model_identity(candidate, emb_model)
        else:
            embed_fn, self._embed_model_id, self._local_embedder = resolve_embed_fallback(
                None, emb_model, config.memory.local_embedding_model,
                local_load_timeout=config.memory.embed_load_timeout_seconds,
                hf_endpoint=config.memory.hf_embedding_endpoint,
                cache_dir=config.memory.local_embedding_cache_dir,
                max_load_attempts=config.memory.local_embedding_max_load_attempts,
                retry_backoff=config.memory.local_embedding_retry_backoff_seconds,
            )

        from echo_agent.memory.vectors import VectorIndex
        # 维度优先用探针实测值（config.vector_dimensions 默认 0=自动跟随），
        # 让索引在首个向量入库前就知道正确维度。
        vector_index = VectorIndex(
            storage,
            dimensions=probe_dim or config.memory.vector_dimensions,
            model_id=self._embed_model_id,
        )
        self._vector_index = vector_index
        self._embed_fn = embed_fn
        self.memory.set_vector_index(vector_index)
        self.memory.set_embed_fn(embed_fn)
        self.consolidator.set_embed_fn(embed_fn)
        if self._episodic is not None and embed_fn is not None:
            self._episodic.attach_embedding(embed_fn, vector_index)
        self._wire_vector_consumers(vector_index, embed_fn)

    def _wire_vector_consumers(self, vector_index: Any, embed_fn: Any) -> None:
        """构造依赖最终 vector_index/embed_fn 的消费者（矛盾检测/reflection/混合检索），
        并把 __init__ 阶段以 None 占位的持有者（context_stage / memory 工具 / prefetcher）
        重新指向最终对象——这些持有者在 __init__ 里按值捕获引用，不重指会永久停在 None。"""
        config = self.config
        storage = self._storage
        forgetting = self.memory.forgetting_curve

        if config.memory.contradiction_detection and storage:
            from echo_agent.memory.contradiction import ContradictionDetector
            # R1 Task8:裁决 mark_superseded 走 loop 单例 service 的 maintenance
            # 通道(统一失效+审计)。矛盾镜像跟踪(unresolved 标记/清除)仍直接落 store。
            detector = ContradictionDetector(
                storage,
                vector_index,
                store=self.memory,
                service=self._memory_service,
            )
            self._contradiction_detector = detector
            self.consolidator.set_contradiction_detector(detector)
            self.consolidator.set_auto_resolve_contradictions(
                config.memory.auto_resolve_contradictions
            )
            # memory 工具在 _register_tools 时以 None 建成，这里补上检测器引用。
            mem_tool = self.tools.get("memory")
            if mem_tool is not None and hasattr(mem_tool, "_contradiction_detector"):
                mem_tool._contradiction_detector = detector

        if config.memory.reflection_enabled:
            from echo_agent.memory.reflection import ReflectionEngine
            # R1 Task8:reflection 的写(蒸馏 add/清 tag/裁决 mark_superseded)注入
            # loop 单例 service,统一走 maintenance 通道失效+审计。收口前就近 new 的
            # reflection service 无 audit_path,审计 no-op——收敛后一并落统一审计。
            self.consolidator.set_reflection(ReflectionEngine(
                self._memory_service,
                llm_call=self.provider.chat_with_retry,
                contradiction_detector=self._contradiction_detector,
            ))

        from echo_agent.memory.retrieval import HybridRetriever

        def entries_fn() -> list:
            return list(self.memory._entries.values())

        # Episode candidates by relevance (semantic + LIKE), assembled inside
        # retrieve() so they ride the same call the prefetcher warms — this is
        # what keeps episodic recall alive on the CLI degrade-on-miss path (a
        # cache hit now carries episodes). None-safe: no episodic manager ⇒ no
        # episode candidates, retrieval stays memory-only.
        episodic_mgr = self._episodic

        async def _episode_search(query: str, session_key: str, limit: int) -> list:
            if episodic_mgr is None:
                return []
            return await episodic_mgr.search_episodes(
                query, session_key=session_key or None, limit=limit
            )

        self._hybrid_retriever = HybridRetriever(
            entries_fn=entries_fn,
            vector_index=vector_index,
            forgetting=forgetting,
            embed_fn=embed_fn,
            embed_timeout=config.memory.embed_timeout_seconds,
            visibility_fn=self.memory.is_visible_in_session,
            episode_search_fn=_episode_search if episodic_mgr is not None else None,
            is_unresolved_fn=self.memory.is_unresolved,
            min_similarity=config.memory.rrf_min_similarity,
        )
        self.memory.set_retriever(self._hybrid_retriever)
        # context_stage 在 __init__ 里按值持有了 None，这里重指最终检索器。
        self._context_stage._hybrid_retriever = self._hybrid_retriever
        # prefetcher 只在存在检索器时才有意义；__init__ 时检索器为 None 故未建，
        # 这里补建并重指 response_stage，使回复后预取重新生效。
        from echo_agent.memory.prefetch import RetrievalPrefetcher

        def _knowledge_fetch(query: str, user_id: str) -> str:
            results = self.knowledge.search(
                query, limit=config.knowledge.max_results, user_id=user_id
            )
            return self.knowledge.format_results(results)

        self._prefetcher = RetrievalPrefetcher(
            # limit=8 matches the inline sync path (5 memory + 3 episode) now
            # that episodes ride the same retrieve() call.
            self._hybrid_retriever, self._put_retrieval_cache, limit=8,
            knowledge_fetch=_knowledge_fetch if self.knowledge else None,
        )
        self._response_stage._prefetcher = self._prefetcher

    async def start(self) -> None:
        await self._resolve_embed_and_index(self._storage)
        if self._vector_index is not None:
            # Order matters: purge orphan rows BEFORE the index loads (so they
            # never enter the matrix), then queue re-embeds for entries whose
            # vector is missing or was produced by a different model.
            try:
                await self.memory.scan_orphan_vectors()
            except Exception as e:
                logger.warning("Orphan vector scan failed: {}", e)
            await self._vector_index.initialize()
            queued = self.memory.queue_missing_embeds(self._vector_index.stale_source_ids)
            if queued:
                self._spawn_background(self.memory.flush_pending_embeds())
            if self._episodic is not None:
                try:
                    await self._episodic.requeue_stale(self._vector_index.stale_source_ids)
                except Exception as e:
                    logger.warning("Episodic stale re-embed failed: {}", e)
        # Knowledge vectors reuse the same embed_fn but stay in their own sidecar
        # store. Inject here in start() — both self.knowledge and self._embed_fn
        # are assigned during __init__, but _init_advanced_memory runs before
        # knowledge is constructed, so injection cannot live there.
        if self.knowledge is not None and self._embed_fn is not None:
            self.knowledge.attach_embedding(
                self._embed_fn, self._resolved_vector_dimensions(),
                embed_timeout=self.config.memory.embed_timeout_seconds,
            )
        if self.knowledge is not None and self.knowledge.needs_vector_backfill():
            self._spawn_background(self.knowledge.rebuild_async())
        await self._cost_tracker.load()
        if self._contradiction_detector is not None:
            try:
                self.memory.reset_unresolved()
                # unresolved 镜像本身是全局索引,启动重建必须扫全库——这是
                # get_unresolved 唯一合法的不带 memory_scope(全库)调用方。
                for c in await self._contradiction_detector.get_unresolved(limit=10000):
                    self.memory.mark_contradiction_unresolved(
                        c.id, c.memory_id_a, c.memory_id_b
                    )
            except Exception as e:
                logger.warning("Unresolved-contradiction rebuild failed: {}", e)
        self._spawn_background(self._start_mcp_background())
        # Skill admission candidate store: ensure schema exists before the first
        # background skill review can stage a candidate. Must run BEFORE
        # subscribe_inbound to close the startup race where an inbound event
        # triggers a review against a not-yet-created table. Independent of evolution.
        if self._skill_candidate_store is not None:
            try:
                await self._skill_candidate_store.init_schema()
            except Exception as e:
                logger.warning("Skill candidate store schema init failed: {}", e)
        self.bus.subscribe_inbound(self._on_inbound)
        # 置位必须在 subscribe_inbound 之后、且在所有可抛异常的初始化完成之后:
        # 之前 _running=True 在 start() 首行,embedding 探针/索引初始化中途抛错时
        # 健康检查(app.py 以 is_running 为 HEALTHY 判据)会对一个收不了消息的
        # 半启动实例误报健康。
        self._running = True
        if self._plugin_manager:
            await self._plugin_manager.hooks.dispatch("on_agent_start")
        if self.evolution is not None:
            try:
                await self.evolution.start()
            except Exception as e:
                logger.warning("Evolution engine failed to start: {}", e)
        logger.info("Agent loop started")

    async def stop(self) -> None:
        self._running = False
        if self.evolution is not None:
            try:
                await self.evolution.stop()
            except Exception as e:
                logger.debug("Evolution engine stop raised: {}", e)
        if self._plugin_manager:
            await self._plugin_manager.hooks.dispatch("on_agent_stop")
            await self._plugin_manager.shutdown()
        if self.mcp_manager:
            await self.mcp_manager.stop_all()
        try:
            from echo_agent.agent.browser.session import manager as _browser_manager
            await _browser_manager.close_all()
        except Exception as e:
            logger.debug("browser manager close_all raised (ignored): {}", e)
        # All background work is spawned via ``_spawn_background`` and owned by
        # the scheduler; ``aclose`` cancels discardable tasks and flushes durable
        # ones. This is the single shutdown path for background work.
        await self._bg_scheduler.aclose(timeout=10.0)
        # 调度器 aclose 后再排空 store 的在途镜像任务:DURABLE 任务可能刚产生镜像写,
        # 顺序不能反。消除关闭时 aiosqlite 向已关闭事件循环回调的资源警告。
        if getattr(self, "memory", None) is not None:
            try:
                await self.memory.aclose()
            except Exception as e:
                logger.debug("Memory store aclose raised (ignored): {}", e)
        # Release the local embedder's dedicated thread pool, if one was built.
        if self._local_embedder is not None:
            try:
                self._local_embedder.close()
            except Exception as e:
                logger.debug("Local embedder close raised (ignored): {}", e)
        logger.info("Agent loop stopped")

    def _spawn_background(self, coro: Any, *, tier: Any = None) -> None:
        from echo_agent.agent.background import Tier
        self._bg_scheduler.spawn(coro, tier=tier or Tier.DISCARDABLE)

    async def _lru_put(self, cache: OrderedDict, key: str, value: Any) -> None:  # type: ignore[type-arg]
        async with self._state_lock:
            cache[key] = value
            cache.move_to_end(key)
            while len(cache) > self._max_cached_sessions:
                cache.popitem(last=False)

    async def put_memory_snapshot(
        self, key: str, value: str, ids: "frozenset[str] | None" = None,
        scope: str = "", version: int = 0,
    ) -> None:
        """快照缓存的唯一写入入口:经统一 LRU 管控。同时写入进入快照的 entry.id 集,
        供动态召回去重。并记录构建时的 (scope, version),读侧据此按 scope 版本校验:
        某 scope 被写后 bump 版本,挂在任意 session_key 上的旧快照都因版本不符失效。"""
        await self._lru_put(self._memory_snapshots, key, value)
        await self._lru_put(self._memory_snapshot_ids, key, ids or frozenset())
        async with self._state_lock:
            self._memory_snapshot_meta[key] = (scope, version)
            # meta 不走 _lru_put,快照被 LRU 逐出后其 meta 会残留。按当前快照键集
            # 剪除孤儿 meta,保证 meta 不超出快照上限、不无界增长(读侧已先 gate
            # session_key in _memory_snapshots,孤儿 meta 不会误命中,但须防泄漏)。
            if len(self._memory_snapshot_meta) > len(self._memory_snapshots):
                live = set(self._memory_snapshots)
                for k in [mk for mk in self._memory_snapshot_meta if mk not in live]:
                    del self._memory_snapshot_meta[k]

    def _scope_version(self, scope: str) -> int:
        return self._scope_versions.get(scope, 0)

    async def _clear_memory_snapshot(self, session_key: str) -> None:
        async with self._state_lock:
            self._memory_snapshots.pop(session_key, None)
            self._memory_snapshot_ids.pop(session_key, None)
            self._memory_snapshot_meta.pop(session_key, None)

    async def _invalidate_memory_caches(self, scope: str, global_scope: bool = False) -> None:
        """记忆写操作后的缓存失效。per-scope 用版本号:bump 该 scope 的版本,
        使所有在旧版本下构建、共享该 memory_scope 的快照/检索缓存(可能挂在
        不同 session_key 上)读取时因版本不符而失效——根治按单 session_key
        pop 清不掉跨通道共享 scope 的问题。environment/矛盾裁决影响所有会话,
        仍全局 clear。"""
        async with self._state_lock:
            if global_scope:
                self._memory_snapshots.clear()
                self._memory_snapshot_ids.clear()
                self._memory_snapshot_meta.clear()
                self._retrieval_cache.clear()
            else:
                self._scope_versions[scope] = self._scope_versions.get(scope, 0) + 1

    async def _put_retrieval_cache(self, session_key: str, entry: Any) -> None:
        """检索预取缓存的唯一写入入口:复用统一 LRU(锁 + 上限),
        与 snapshot 缓存共用上限策略,避免无界增长。"""
        await self._lru_put(self._retrieval_cache, session_key, entry)

    def _get_retrieval_cache(self, session_key: str) -> Any:
        return self._retrieval_cache.get(session_key)

    async def _start_mcp_background(self) -> None:
        try:
            await self._start_mcp()
        except Exception as e:
            logger.error("MCP initialization failed (agent continues without MCP tools): {}", e)

    async def _start_mcp(self) -> None:
        mcp_servers = self._filter_mcp_servers(self.config.tools.mcp_servers)
        if not mcp_servers:
            return
        from echo_agent.mcp.manager import MCPManager
        self.mcp_manager = MCPManager(
            workspace=self.workspace,
            security_policy=self.config.tools.mcp_security_policy,
        )
        await self.mcp_manager.start_all(mcp_servers)
        await self.mcp_manager.discover_tools(self.tools)
        self._apply_runtime_tool_policy()

    def _apply_runtime_tool_policy(self) -> None:
        from echo_agent.security.tool_policy import is_tool_allowed
        for name in list(self.tools.tool_names):
            if name.startswith("mcp_") and not is_tool_allowed(self.config, name):
                self.tools.unregister(name)
                logger.info("Tool policy skipped MCP tool '{}'", name)

    def _filter_mcp_servers(self, servers: dict[str, Any]) -> dict[str, Any]:
        filtered: dict[str, Any] = {}
        for name, cfg in servers.items():
            if cfg.url and self.config.execution.network_policy == "deny":
                logger.warning("Skipping MCP server '{}' because networkPolicy is deny", name)
                continue
            if cfg.command and self.config.security.profile == "public_gateway" and not self.config.permissions.elevated.enabled:
                logger.warning("Skipping stdio MCP server '{}' under public_gateway profile without elevated access", name)
                continue
            filtered[name] = cfg
        return filtered

    async def _record_cron_outcome(self, event: InboundEvent, status: str, error: str = "") -> None:
        """For a fired CRON job, write the turn's terminal outcome back to the
        scheduler so last_status reflects reality instead of staying "queued".

        "completed" means the agent turn finished and its reply was published to
        the bus — NOT a confirmed channel delivery receipt (that would need the
        SendResult to propagate back from the channel, which is out of scope
        here). This is still strictly more truthful than the enqueue-time
        "queued". No-op for non-cron events or when no scheduler is wired."""
        # getattr guard: some code paths (and tests) build AgentLoop via
        # __new__, bypassing __init__ where _scheduler is set.
        scheduler = getattr(self, "_scheduler", None)
        if scheduler is None or event.event_type != EventType.CRON:
            return
        job_id = str(event.metadata.get("job_id") or "")
        if not job_id:
            return
        try:
            await scheduler.record_run_outcome(job_id, status, error)
        except Exception as e:
            logger.debug("Cron outcome writeback failed for job {}: {}", job_id, e)

    async def _on_inbound(self, event: InboundEvent) -> None:
        """入站事件处理入口，负责追踪、错误处理和响应发布。"""
        if not self._running:
            return
        # 群聊会话作用域解析：把按策略解析出的隔离键固化到 override，
        # 使下游全部 session_key 读取(锁/working memory/快照/可见性/source_session)统一按此键隔离。
        scope = self.config.session.group_session_scope
        if not event.session_key_override:
            event.session_key_override = event.scoped_session_key(scope)
        # 记忆作用域(memory_scope)的冻结统一下沉到 _process_event,使 _on_inbound
        # 与 process_direct(A2A/CLI)两条入站路径共享同一处逻辑,避免新增入口漏设。
        # Approval decisions (/approve, /deny, /approvals) are handled BEFORE
        # acquiring the session lock — by design. A turn that is blocked waiting
        # for approval holds the session lock while parked in wait_for_decision;
        # routing the decision through a separate lock-free path (not _process_event)
        # is what lets it wake the waiter without deadlocking on that same lock.
        # Do not move this below sessions.acquire().
        if self._is_approval_command(event.text):
            response_text = await self._handle_approval_command(event)
            if response_text is not None:
                out = OutboundEvent.from_text_with_media(
                    channel=event.channel,
                    chat_id=event.chat_id,
                    text=response_text,
                    reply_to_id=event.reply_to_id,
                )
                out.metadata = dict(event.metadata)
                out.metadata["_inbound_event_id"] = event.event_id
                await self.bus.publish_outbound(out)
                return
        # Clarify answers, like approval decisions, are handled BEFORE acquiring
        # the session lock — the blocked agent holds that lock while parked in
        # wait_for_answer, so resolving must run on a lock-free path. Do not move
        # this below sessions.acquire().
        if self._is_clarify_command(event.text):
            response_text = await self._handle_clarify_command(event)
            if response_text is not None:
                out = OutboundEvent.from_text_with_media(
                    channel=event.channel,
                    chat_id=event.chat_id,
                    text=response_text,
                    reply_to_id=event.reply_to_id,
                )
                out.metadata = dict(event.metadata)
                out.metadata["_inbound_event_id"] = event.event_id
                await self.bus.publish_outbound(out)
                return
        # Session-interrupt escape valve, handled BEFORE the session lock for the
        # same reason as clarify answers: the agent blocked in wait_for_answer
        # holds the lock, so the wake must run on a lock-free path. Synthesized by
        # the gateway on ws disconnect; internal control command, no reply.
        if self._is_clarify_cancel_command(event.text):
            await self._handle_clarify_cancel(event)
            return
        # Turn-interrupt escape valve. Handled BEFORE the session lock for the
        # same reason as clarify-cancel: the running turn holds the lock, so the
        # cooperative-stop signal must be delivered on a lock-free path — the
        # inference loop polls the flag at its next checkpoint and stops cleanly.
        # Synthesized by the gateway from a Ctrl+C interrupt frame; internal
        # control command, no reply.
        if self._is_interrupt_command(event.text):
            await self._handle_interrupt(event)
            return
        # IM follow-up continuation. On IM channels a clarify tool call cannot
        # block the turn, so the agent's question was remembered per session
        # (InferenceStage._prepare_clarify → register_im_pending). If this
        # session has an unanswered, unexpired follow-up, bind this message to it
        # so the model sees WHAT is being answered — otherwise a bare "A" reads
        # as an isolated, ambiguous message. This runs on IM channels only; CLI
        # uses the blocking /clarify path and never registers an IM pending.
        self._maybe_bind_im_clarify_answer(event)
        session_lock = await self.sessions.acquire(event.session_key)
        async with session_lock:
            trace_id = uuid.uuid4().hex[:12]
            span = self.tracer.start_span(trace_id, f"s_{trace_id}", "process_message", "input")
            heartbeat = ProgressHeartbeat(
                self.bus, event, self.config.agent.heartbeat,
                cognitive_emitter=self.cognitive_emitter,
            )
            activity = SharedActivityState(started_at=time.monotonic())
            # Register this turn so a lock-free /__interrupt__ can flag it for a
            # cooperative stop. request() always starts un-interrupted, so a
            # stale flag from a prior turn cannot leak into this one.
            self.interrupt.request(event.session_key, event.event_id)
            try:
                await heartbeat.start(activity)
                result = await self._process_event(event, trace_id, publish_response=True, activity=activity)
                response_text = result.response_text

                # Delivery point. Text convergence (degraded notices, English
                # filler → Chinese fallback) already happened in
                # ResponseStage.finalize BEFORE the session was persisted, so
                # history, stream, and this publish all carry the same text.
                # Here we only decide whether an outbound message is still
                # needed: streamed turns already delivered it.
                final_text = "" if result.outbound_sent else response_text

                if final_text and _should_publish_reply(event, final_text):
                    out = OutboundEvent.from_text_with_media(
                        channel=event.channel, chat_id=event.chat_id, text=final_text, reply_to_id=event.reply_to_id,
                    )
                    out.metadata = dict(event.metadata)
                    out.metadata["_inbound_event_id"] = event.event_id
                    await self.bus.publish_outbound(out)
                self.tracer.end_span(span, metadata={"response_len": len(response_text or "")})
                await self._record_cron_outcome(event, "completed")
            except Exception as e:
                logger.error("Processing failed for event {}: {}", event.event_id, e)
                self.tracer.end_span(span, error=str(e))
                error_out = OutboundEvent.text_reply(
                    channel=event.channel, chat_id=event.chat_id, text=GENERIC_FALLBACK_TEXT, reply_to_id=event.reply_to_id,
                )
                error_out.metadata = dict(event.metadata)
                error_out.metadata["_inbound_event_id"] = event.event_id
                await self.bus.publish_outbound(error_out)
                await self._record_cron_outcome(event, "error", str(e))
            finally:
                # Deregister the turn so a finished turn leaves no residue for
                # the next one to trip over (mirrors request() above).
                self.interrupt.clear(event.session_key)
                await heartbeat.stop()
                self.tracer.flush_trace(trace_id)

    async def _process_event(self, event: InboundEvent, trace_id: str, *, publish_response: bool = False, activity: Any = None) -> _ProcessResult:
        """处理单个入站事件 — 委托给 pipeline stages。"""
        # 记忆作用域键:与 session_key 解耦(后者承载会话锁/历史/投递路由,不能按人
        # 归一)。此处是所有入站路径(_on_inbound、A2A/CLI 的 process_direct)的唯一
        # 咽喉点,统一冻结可确保任何入口都拿到正确作用域。单主体下 1:1 私聊(任意通道,
        # 含 A2A/CLI)归一到 owner 键实现跨通道记忆互通,群聊保持 per_user 隔离;
        # 开关关闭时退回按会话键(旧行为)。
        if not event.memory_scope:
            if self.config.memory.cross_channel_owner:
                scope = self.config.session.group_session_scope
                event.memory_scope = event.memory_scope_key(
                    scope,
                    self.config.memory.owner_key,
                    frozenset(self.config.memory.principal_bindings),
                )
            else:
                event.memory_scope = event.session_key
        session = await self.sessions.get_or_create(event.session_key)
        if event.session_key not in self._working_memories:
            from echo_agent.memory.tiers import WorkingMemory
            await self._lru_put(self._working_memories, event.session_key, WorkingMemory(
                max_entries=self.config.memory.max_working_memory
            ))
        command_response = await self._handle_approval_command(event)
        if command_response is not None:
            session.add_message("user", event.text)
            session.add_message("assistant", command_response)
            await self.sessions.save(session)
            return _ProcessResult(response_text=command_response)

        recorder = self.evolution.recorder if self.evolution is not None else None
        if recorder is not None and not _is_ephemeral_session(event.session_key, event.channel):
            try:
                await recorder.begin_turn(
                    session_key=event.session_key,
                    chat_id=event.chat_id,
                    channel=event.channel,
                    task_input=event.text or "",
                    model_used=self._default_model,
                )
            except Exception as e:
                logger.debug("Recorder begin_turn failed: {}", e)

        should_introduce = self._should_introduce(session)
        intro_text = self._build_introduction(event) if should_introduce else ""
        stream_publisher = _TokenStreamPublisher(
            self.bus,
            event,
            enabled=publish_response and self._should_stream_channel(event.channel),
            flush_chars=self.config.channels.stream_flush_chars,
            flush_interval_ms=self.config.channels.stream_flush_interval_ms,
            paragraph_mode=self.config.channels.stream_paragraph_mode,
            intro_text=intro_text,
        )
        if publish_response:
            await stream_publisher.start()

        ctx = None
        inference_result = None
        result = None
        process_error: Exception | None = None
        try:
            # Stage 1: Context building
            ctx = await self._context_stage.build(
                event, session,
                publish_response=publish_response,
                trace_id=trace_id,
                stream_publisher=stream_publisher,
                intro_text=intro_text,
            )
            if activity is not None:
                ctx.activity = activity

            # Stage 2: Inference (LLM + tool execution loop)
            inference_result = await self._inference_stage.run(ctx)

            # Stage 3: Response finalization
            result = await self._response_stage.finalize(ctx, inference_result)
        except Exception as e:
            process_error = e
            raise
        finally:
            if recorder is not None:
                try:
                    if process_error is not None:
                        await recorder.end_turn(
                            session_key=event.session_key,
                            error=f"{type(process_error).__name__}: {process_error}",
                            outcome="failure",
                            task_type=getattr(ctx, "task_type", "") if ctx is not None else "",
                            spawn_fn=self._spawn_background,
                        )
                    else:
                        await recorder.end_turn(
                            session_key=event.session_key,
                            response_text=result.response_text if result else "",
                            iteration_count=getattr(inference_result, "total_tool_calls", 0) or 0,
                            task_type=getattr(ctx, "task_type", "") if ctx is not None else "",
                            spawn_fn=self._spawn_background,
                        )
                except Exception as e:
                    logger.debug("Recorder end_turn failed: {}", e)

        return _ProcessResult(
            response_text=result.response_text,
            outbound_sent=result.outbound_sent,
            degraded_notices=result.degraded_notices,
        )

    async def _handle_approval_command(self, event: InboundEvent) -> str | None:
        text = event.text.strip()
        if not self._is_approval_command(text):
            return None
        parts = text.split(maxsplit=2)
        command = parts[0].lower()

        if command == "/approvals":
            pending = self.approval.get_pending()
            visible = [req for req in pending if self._can_decide_approval(event.sender_id, req)]
            if not visible:
                return "No pending approval requests."
            lines = ["Pending approval requests:"]
            for req in visible:
                lines.append(f"- {req.id}: {req.tool_name or req.action} requested by {req.user_id}")
            return "\n".join(lines)

        if len(parts) < 2:
            return f"Usage: `{command} <request_id>`"
        request_id = parts[1]
        req = self.approval.get(request_id)
        if not req:
            # Not pending — distinguish "already decided / expired" from "never existed"
            # so users don't see a misleading "not found" for a request they just acted on.
            return self._describe_inactive_approval(request_id)
        if not self._can_decide_approval(event.sender_id, req):
            return "You are not allowed to decide this approval request."

        if command == "/approve":
            level = parts[2] if len(parts) >= 3 else ""
            ok = self.approval.approve(request_id, level=level, decided_by=event.sender_id)
            # `ok` is True on the happy path: get()/approve() both read _pending and
            # no await separates the check above from this act, so today nothing can
            # decide the request in between. The `else` is a check-then-act (TOCTOU)
            # guard: if a future change introduces an await in that window, a
            # concurrent decision could pop the request first — then approve()
            # returns False and we describe its now-historic state instead of
            # silently dropping the user's command. See the redeny test for the
            # forced-False path.
            return f"Approval request {request_id} approved." if ok else self._describe_inactive_approval(request_id)

        reason = parts[2] if len(parts) >= 3 else ""
        ok = self.approval.deny(request_id, reason=reason, decided_by=event.sender_id)
        # Same check-then-act guard as /approve above.
        return f"Approval request {request_id} denied." if ok else self._describe_inactive_approval(request_id)

    def _describe_inactive_approval(self, request_id: str) -> str:
        """Explain why a non-pending request can't be acted on, based on its history.

        A request leaves `_pending` once it is approved, denied, or times out. The
        command layer only looks at `_pending`, so without this lookup all three
        cases collapse into a misleading "not found".
        """
        historic = self.approval._find_history(request_id)
        if historic is None:
            return f"Approval request not found: {request_id}"
        status = historic.status
        if status == ApprovalStatus.APPROVED:
            when = historic.decided_at or "earlier"
            return f"Approval request {request_id} was already approved ({when}); no action needed."
        if status == ApprovalStatus.DENIED:
            suffix = f": {historic.reason}" if historic.reason else ""
            return f"Approval request {request_id} was already denied{suffix}."
        if status == ApprovalStatus.EXPIRED:
            return (
                f"Approval request {request_id} expired before it was approved; "
                "the action did not run. Please re-trigger it to get a fresh request."
            )
        return f"Approval request not found: {request_id}"

    def _is_approval_command(self, text: str) -> bool:
        stripped = text.strip()
        if not stripped.startswith("/"):
            return False
        command = stripped.split(maxsplit=1)[0].lower()
        return command in {"/approvals", "/approve", "/deny"}

    def _is_clarify_command(self, text: str) -> bool:
        return text.strip().split(maxsplit=1)[0].lower() == "/clarify" if text.strip() else False

    def _maybe_bind_im_clarify_answer(self, event: InboundEvent) -> None:
        """Bind an IM message to a pending follow-up question on its session.

        The agent asked a question on an IM channel last turn; that question was
        remembered (register_im_pending). We surface it to the model by reusing
        the reply-quote injection path: setting reply_to_text makes
        build_user_message_with_reply prepend the question (and any options) to
        the history copy, so the model sees the user is answering it — without
        rewriting event.text (retrieval/history keep the raw reply). We do NOT
        force-map "A" → option ourselves; the model resolves the choice from the
        full quoted context, which also handles free-text answers uniformly.

        No-ops when: the event is not a real user message (cron / unattended /
        internal control events must not consume pending — they reuse the source
        session key and would otherwise mis-bind to the last question); the loop
        has no clarify manager wired (lightweight __new__ construction paths); the
        session has no pending; the pending expired (TTL); or the message already
        carries its own quote. In the quoted case the explicit user reference wins
        over the implicit follow-up binding, but the stale pending is still cleared
        so it cannot bind to a later unrelated message.
        """
        # 仅真实用户的常规消息才能消费待答状态。定时任务(cron/unattended)与内部
        # 控制事件复用同一 source_session_key 时,不应把 TTL 内的下一次调度误当成
        # “回答上次问题”而绑定到 pending。
        if event.event_type != EventType.MESSAGE or event.unattended or event.is_control:
            return
        # clarify 兜底:__new__ 绕过 __init__ 的构造路径(如部分轻量测试)不接线
        # self.clarify,此处安全 no-op 而非抛 AttributeError。
        clarify = getattr(self, "clarify", None)
        if clarify is None:
            return
        session_key = event.session_key
        if not session_key:
            return
        # 用户显式引用了某条消息:引用意图优先于隐式追问绑定,不再改写 reply_to_text。
        # 但仍要清理本 session 的待答状态,否则旧问题会残留 —— 引用回答成功后,下一条
        # 无关消息会被误绑到已被回答过的旧问题上。
        if event.reply_to_text:
            clarify.clear_im_pending(session_key)
            return
        ttl = float(self.config.session.im_clarify_pending_ttl_seconds)
        req = clarify.take_im_pending(session_key, ttl)
        if req is None:
            return
        question = (req.question or "").strip()
        if not question:
            return
        if req.options:
            choices = "；".join(f"{chr(65 + i)}. {opt}" for i, opt in enumerate(req.options))
            quoted = f"{question}\n可选项：{choices}"
        else:
            quoted = question
        event.reply_to_text = quoted
        event.reply_to_is_own = True
        event.reply_to_sender = None

    _CLARIFY_CANCEL_CMD = "/__clarify_cancel__"

    def _is_clarify_cancel_command(self, text: str) -> bool:
        return text.strip() == self._CLARIFY_CANCEL_CMD

    async def _handle_clarify_cancel(self, event: InboundEvent) -> None:
        # Wake any clarify blocked on this session with the interrupt sentinel,
        # so a disconnected/quit CLI does not leave the agent parked in
        # wait_for_answer until the 24h registry backstop. Internal control
        # command — no user-facing reply.
        self.clarify.cancel_session(event.session_key)

    _INTERRUPT_CMD = "/__interrupt__"

    def _is_interrupt_command(self, text: str) -> bool:
        return text.strip() == self._INTERRUPT_CMD

    async def _handle_interrupt(self, event: InboundEvent) -> None:
        # Flag the session's running turn for a cooperative stop. Also cancel any
        # clarify parked on this session: a Ctrl+C while the agent waits for an
        # answer should unblock it (mirrors the disconnect escape valve), not
        # sit idle. Internal control command — no user-facing reply; the turn
        # itself emits the "stopped" text when it converges at the checkpoint.
        # The gateway stamps the turn the user meant to stop into metadata; pass
        # it through so a delayed stop frame can't land on a later turn. Empty
        # means "stop whatever is running" (older clients that don't track IDs).
        target_event_id = str(event.metadata.get("_interrupt_target_event_id", ""))
        self.interrupt.interrupt(event.session_key, target_event_id)
        self.clarify.cancel_session(event.session_key)

    async def _handle_clarify_command(self, event: InboundEvent) -> str | None:
        # Format: /clarify <clarify_id> <answer...>  (answer may contain spaces)
        parts = event.text.strip().split(maxsplit=2)
        if len(parts) < 2:
            return "用法:`/clarify <id> <答案>`"
        clarify_id = parts[1]
        answer = parts[2] if len(parts) >= 3 else ""
        ok = self.clarify.resolve(clarify_id, answer)
        if ok:
            return f"已回复澄清请求 {clarify_id}。"
        return f"澄清请求未找到或已处理:{clarify_id}"

    def _can_decide_approval(self, user_id: str, request: Any) -> bool:
        if user_id in (self.config.permissions.admin_users or []):
            return True
        if not self.config.permissions.admin_users:
            return not request.user_id or request.user_id == user_id
        return False

    def _should_introduce(self, session: Session) -> bool:
        if not self.config.session.introduction_enabled:
            return False
        return not any(msg.get("role") == "assistant" for msg in session.messages)

    def _build_introduction(self, event: InboundEvent) -> str:
        template = self.config.session.introduction_template.strip()
        if not template:
            if event.channel in {"wecom", "weixin"}:
                template = "你好，我是 {agent_name}，很高兴为你服务。"
            else:
                template = "Hello, I'm {agent_name}. How can I help?"

        values = {
            "agent_name": self.context.agent_name,
            "channel": event.channel,
            "chat_id": event.chat_id,
            "session_key": event.session_key,
        }
        try:
            return template.format(**values).strip()
        except Exception:
            logger.warning("Invalid session introduction template, using raw text")
            return template

    def _should_stream_channel(self, channel: str) -> bool:
        channels = set(self.config.channels.stream_channels)
        if channel in channels:
            return True
        for pattern in channels:
            if pattern.endswith(":*") and channel.startswith(pattern[:-1]):
                return True
        return False

    async def process_direct(self, content: str, session_key: str = "cli:direct", channel: str = "cli") -> str:
        """Process a message directly (for CLI or testing)."""
        event = InboundEvent.text_message(
            channel=channel, sender_id="user", chat_id="direct", text=content,
            session_key_override=session_key,
        )
        # Hold the same per-session lock the inbound dispatcher uses so two
        # concurrent CLI calls on the same session_key serialize their writes
        # to the message history.
        session_lock = await self.sessions.acquire(event.session_key)
        async with session_lock:
            result = await self._process_event(event, uuid.uuid4().hex[:12], publish_response=False)
        return result.response_text or ""
