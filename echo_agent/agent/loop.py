"""Agent loop — the core processing engine.

Receives events → builds context → calls LLM → executes tools → sends responses.
Orchestrates pipeline stages: context building, inference, and response finalization.
"""

from __future__ import annotations

import asyncio
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
from echo_agent.agent.pipeline.response_stage import ResponseStage
from echo_agent.agent.tools.circuit_breaker import ToolCircuitBreaker
from echo_agent.agent.tools.registry import ToolRegistry
from echo_agent.bus.events import InboundEvent, OutboundEvent
from echo_agent.bus.queue import MessageBus
from echo_agent.config.schema import Config
from echo_agent.cost.budget import CostTracker
from echo_agent.memory.consolidator import MemoryConsolidator
from echo_agent.memory.store import MemoryStore
from echo_agent.models.inference import InferenceController
from echo_agent.models.provider import LLMProvider
from echo_agent.models.router import ModelRouter
from echo_agent.observability.monitor import TraceLogger
from echo_agent.permissions.manager import ApprovalManager, CredentialManager
from echo_agent.runtime_paths import bundled_skills_dir
from echo_agent.session.manager import Session, SessionManager
from echo_agent.skills.store import SkillStore
from echo_agent.agent.streaming import (
    ProcessResult as _ProcessResult,
    TokenStreamPublisher as _TokenStreamPublisher,
)
from echo_agent.agent.degraded_notice import (
    GENERIC_FALLBACK_TEXT,
    combine_notices,
    is_generic_fallback,
)


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
        self.config = config
        self.provider = provider
        self.router = router
        self.workspace = workspace
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
        )
        self.tools = ToolRegistry(
            audit_log_path=workspace / config.storage.logs_dir / "tool_audit.jsonl",
        )
        from echo_agent.gateway.media import MediaCache
        media_cache = MediaCache(
            cache_dir=workspace / config.gateway.media_cache_dir,
            max_size_mb=config.gateway.media_cache_max_mb,
        )
        self.context = ContextBuilder(
            workspace,
            media_cache=media_cache,
            doc_enabled=config.tools.inbound_document_enabled,
            doc_max_chars=config.tools.inbound_document_max_chars,
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
        self._episodic = None
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
        self._max_cached_sessions = 200
        self._background_tasks: set[asyncio.Task[Any]] = set()
        self._pending_consolidations: set[str] = set()
        self._state_lock = asyncio.Lock()
        self._plugin_manager: Any = None
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
            snapshot_enabled=self._snapshot_enabled,
            tool_definitions_fn=self.tools.get_definitions,
            episodic=self._episodic,
            plan_run_store=self._plan_run_store,
            bus=bus,
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
            working_memories=self._working_memories,
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
            memory_store=self.memory,
            task_manager=task_manager,
            workflow_engine=workflow_engine,
            knowledge_index=self.knowledge,
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
        from echo_agent.memory.retrieval import HybridRetriever

        forgetting = self.memory.forgetting_curve

        episodic = EpisodicManager(storage) if storage else None
        semantic = SemanticManager(self.memory)
        archival = ArchivalManager(storage, store=self.memory) if storage else None

        self._episodic = episodic
        self.consolidator.set_episodic_manager(episodic)
        self.consolidator.set_semantic_manager(semantic)
        self.consolidator.set_forgetting_curve(forgetting)
        self.consolidator.set_archival_manager(archival)

        vector_index = None
        embed_fn = None
        if config.memory.vector_enabled and storage:
            from echo_agent.memory.vectors import VectorIndex
            vector_index = VectorIndex(storage, dimensions=config.memory.vector_dimensions)
            self.memory.set_vector_index(vector_index)

            from echo_agent.models.provider import LLMProvider
            emb_model = config.memory.embedding_model or None
            # Prefer the main provider when it supports embeddings; otherwise
            # route to any registered embed-capable provider so an Anthropic
            # (or other non-embedding) main provider no longer silently
            # disables vector search / hybrid retrieval / contradiction scan.
            if hasattr(self.provider, "embed") and type(self.provider).embed is not LLMProvider.embed:
                embed_provider: Any = self.provider
            elif self.router is not None:
                embed_provider, routed_model = self.router.find_embed_provider(
                    emb_model or ""
                )
                if embed_provider is not None:
                    emb_model = routed_model or emb_model
                    logger.info(
                        "Main provider lacks embeddings; routing embedding via {}",
                        type(embed_provider).__name__,
                    )
            else:
                embed_provider = None

            if embed_provider is not None:
                async def _embed(text: str, _p=embed_provider, _model=emb_model) -> list[float]:
                    result = await _p.embed(text, model=_model)
                    return result or []
                embed_fn = _embed
            else:
                logger.warning(
                    "No embedding-capable provider registered; vector search "
                    "and hybrid retrieval will degrade to keyword mode"
                )

        self._vector_index = vector_index
        self._embed_fn = embed_fn
        self.memory.set_embed_fn(embed_fn)
        self.consolidator.set_embed_fn(embed_fn)

        if config.memory.contradiction_detection and storage:
            from echo_agent.memory.contradiction import ContradictionDetector
            detector = ContradictionDetector(storage, vector_index, store=self.memory)
            self.consolidator.set_contradiction_detector(detector)

        def entries_fn() -> list:
            return list(self.memory._entries.values())

        self._hybrid_retriever = HybridRetriever(
            entries_fn=entries_fn,
            vector_index=vector_index,
            forgetting=forgetting,
            embed_fn=embed_fn,
            embed_timeout=config.memory.embed_timeout_seconds,
        )
        self.memory.set_retriever(self._hybrid_retriever)

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

    async def start(self) -> None:
        self._running = True
        if self._vector_index is not None:
            await self._vector_index.initialize()
        await self._cost_tracker.load()
        self._spawn_background(self._start_mcp_background())
        self.bus.subscribe_inbound(self._on_inbound)
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
        async with self._state_lock:
            tasks = list(self._background_tasks)
        for task in tasks:
            task.cancel()
        if tasks:
            try:
                await asyncio.wait_for(
                    asyncio.gather(*tasks, return_exceptions=True),
                    timeout=10.0,
                )
            except (TimeoutError, asyncio.TimeoutError):
                logger.warning(
                    "{} background task(s) did not finish within 10s during shutdown; abandoning",
                    len(tasks),
                )
        async with self._state_lock:
            self._background_tasks.clear()
        logger.info("Agent loop stopped")

    def _spawn_background(self, coro: Any, *, session_key: str = "") -> None:
        task = asyncio.create_task(coro)
        if session_key:
            task._session_key = session_key  # type: ignore[attr-defined]
        self._background_tasks.add(task)
        task.add_done_callback(self._on_background_done)

    def _on_background_done(self, task: asyncio.Task) -> None:
        self._background_tasks.discard(task)
        if not task.cancelled() and task.exception():
            logger.warning("Background task failed: {}", task.exception())
            session_key = getattr(task, '_session_key', None)
            if session_key:
                self._pending_consolidations.add(session_key)

    async def _lru_put(self, cache: OrderedDict, key: str, value: Any) -> None:  # type: ignore[type-arg]
        async with self._state_lock:
            cache[key] = value
            cache.move_to_end(key)
            while len(cache) > self._max_cached_sessions:
                cache.popitem(last=False)

    async def _clear_memory_snapshot(self, session_key: str) -> None:
        async with self._state_lock:
            self._memory_snapshots.pop(session_key, None)

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

    async def _on_inbound(self, event: InboundEvent) -> None:
        """入站事件处理入口，负责追踪、错误处理和响应发布。"""
        if not self._running:
            return
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
        session_lock = await self.sessions.acquire(event.session_key)
        async with session_lock:
            trace_id = uuid.uuid4().hex[:12]
            span = self.tracer.start_span(trace_id, f"s_{trace_id}", "process_message", "input")
            try:
                result = await self._process_event(event, trace_id, publish_response=True)
                response_text = result.response_text
                notice = combine_notices(result.degraded_notices)

                # Convergence point: the turn MUST deliver a meaningful message.
                # 1) degraded event + no real answer  -> send the Chinese notice
                # 2) degraded event + real answer not yet sent -> answer + notice
                # 3) degraded event + real answer already streamed -> notice only
                # 4) no degraded event, empty/generic answer -> generic Chinese
                # 5) no degraded event, real answer -> unchanged behaviour
                final_text = ""
                if notice:
                    if result.outbound_sent:
                        final_text = notice
                    elif is_generic_fallback(response_text):
                        final_text = notice
                    else:
                        final_text = f"{response_text}\n\n{notice}"
                elif not result.outbound_sent:
                    if is_generic_fallback(response_text):
                        final_text = GENERIC_FALLBACK_TEXT
                    else:
                        final_text = response_text

                if final_text:
                    out = OutboundEvent.from_text_with_media(
                        channel=event.channel, chat_id=event.chat_id, text=final_text, reply_to_id=event.reply_to_id,
                    )
                    out.metadata = dict(event.metadata)
                    out.metadata["_inbound_event_id"] = event.event_id
                    await self.bus.publish_outbound(out)
                self.tracer.end_span(span, metadata={"response_len": len(response_text or "")})
            except Exception as e:
                logger.error("Processing failed for event {}: {}", event.event_id, e)
                self.tracer.end_span(span, error=str(e))
                error_out = OutboundEvent.text_reply(
                    channel=event.channel, chat_id=event.chat_id, text=GENERIC_FALLBACK_TEXT, reply_to_id=event.reply_to_id,
                )
                error_out.metadata = dict(event.metadata)
                error_out.metadata["_inbound_event_id"] = event.event_id
                await self.bus.publish_outbound(error_out)
            finally:
                self.tracer.flush_trace(trace_id)

    async def _process_event(self, event: InboundEvent, trace_id: str, *, publish_response: bool = False) -> _ProcessResult:
        """处理单个入站事件 — 委托给 pipeline stages。"""
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
        if recorder is not None:
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
            return f"Approval request not found: {request_id}"
        if not self._can_decide_approval(event.sender_id, req):
            return "You are not allowed to decide this approval request."

        if command == "/approve":
            ok = self.approval.approve(request_id, decided_by=event.sender_id)
            return f"Approval request {request_id} approved." if ok else f"Approval request not found: {request_id}"

        reason = parts[2] if len(parts) >= 3 else ""
        ok = self.approval.deny(request_id, reason=reason, decided_by=event.sender_id)
        return f"Approval request {request_id} denied." if ok else f"Approval request not found: {request_id}"

    def _is_approval_command(self, text: str) -> bool:
        stripped = text.strip()
        if not stripped.startswith("/"):
            return False
        command = stripped.split(maxsplit=1)[0].lower()
        return command in {"/approvals", "/approve", "/deny"}

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
