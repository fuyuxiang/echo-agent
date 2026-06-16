"""Application composition root — bootstraps all subsystems and manages their lifecycle.

This module is the single place that wires config → storage → providers → bus →
agent → plugins → evolution → channels → gateway, and the single place that
knows the correct start/stop ordering. Entry points (``__main__``, CLI
subcommands, tests) should import from here instead of from the script module.
"""

from __future__ import annotations

import asyncio
import signal
import sys
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, Callable, TYPE_CHECKING

if TYPE_CHECKING:
    from echo_agent.config.schema import Config

from loguru import logger


def configure_logging(level: str) -> None:
    logger.remove()
    logger.add(sys.stderr, level=level, format="<green>{time:HH:mm:ss}</green> | <level>{level:<7}</level> | {message}")


@dataclass
class BootstrapResult:
    config: Any = None
    workspace: Path = field(default_factory=lambda: Path("."))
    storage: Any = None
    bus: Any = None
    router: Any = None
    provider: Any = None
    agent: Any = None
    channels: Any = None
    scheduler: Any = None
    health: Any = None


async def bootstrap(
    config_path: str | None = None,
    overrides: dict[str, Any] | None = None,
    on_cli_exit: Callable[[], None] | None = None,
) -> BootstrapResult:
    """Shared bootstrap: config → storage → providers → bus → agent → channels."""
    from echo_agent.agent.loop import AgentLoop
    from echo_agent.bus.queue import MessageBus
    from echo_agent.channels.manager import ChannelManager
    from echo_agent.config.loader import load_config, resolve_config_file
    from echo_agent.models.provider import LLMProvider
    from echo_agent.models.providers import create_provider
    from echo_agent.models.router import ModelRouter
    from echo_agent.observability.monitor import HealthChecker
    from echo_agent.scheduler.delivery import build_scheduled_job_handler
    from echo_agent.storage.sqlite import SQLiteBackend

    config_file = resolve_config_file(config_path)
    config = load_config(config_path=config_file, overrides=overrides)
    configure_logging(config.observability.log_level)

    workspace_value = Path(config.workspace).expanduser()
    if not workspace_value.is_absolute():
        workspace_base = Path.cwd() if overrides and "workspace" in overrides else (config_file.parent if config_file else Path.cwd())
        workspace_value = workspace_base / workspace_value
    ws = workspace_value.resolve()
    ws.mkdir(parents=True, exist_ok=True)

    storage = SQLiteBackend(ws / config.storage.database_path)
    await storage.initialize()

    bus = MessageBus(
        max_queue_size=config.bus.max_queue_size,
        max_concurrency=config.bus.max_concurrency,
    )

    from echo_agent.bus.rate_limiter import SessionRateLimiter
    bus.set_rate_limiter(SessionRateLimiter(
        rpm=config.rate_limit.session_rpm,
        burst=config.rate_limit.session_burst,
    ))
    router = ModelRouter(config.models)
    provider: LLMProvider | None = None
    provider_errors: list[str] = []

    for pc in config.models.providers:
        try:
            p = create_provider(pc, default_model=config.models.default_model)
            router.register_provider(pc.name, p)
            if provider is None:
                provider = p
            logger.info("Registered provider: {}", pc.name)
        except Exception as e:
            provider_errors.append(f"{pc.name or '<unnamed>'}: {e}")
            logger.warning("Failed to create provider '{}': {}", pc.name, e)

    if provider is None:
        from echo_agent.models.stub import StubProvider

        if config.models.providers:
            details = "; ".join(provider_errors) or "all configured providers were skipped"
            stub_message = (
                "[No LLM provider could be initialized. Check provider SDK/API key. "
                f"Details: {details}]"
            )
            logger.error("No providers initialized — using stub: {}", details)
        else:
            stub_message = "[No LLM provider configured. Set up a provider in echo-agent.yaml]"
            logger.error("No providers configured — using stub")

        provider = StubProvider(stub_message)
        router.register_provider("stub", provider)

    from echo_agent.scheduler.service import Scheduler
    scheduler: Scheduler | None = None
    if config.scheduler.enabled:
        scheduler = Scheduler(
            store_path=ws / "data" / "scheduler.json",
            on_job=build_scheduled_job_handler(bus),
            max_concurrent=config.scheduler.max_concurrent_jobs,
        )

    from echo_agent.tasks.manager import TaskManager
    from echo_agent.tasks.workflow import WorkflowEngine
    task_manager = TaskManager(storage)
    workflow_engine = WorkflowEngine(storage, task_manager)

    agent = AgentLoop(
        bus=bus, config=config, provider=provider, workspace=ws,
        router=router,
        scheduler=scheduler, storage=storage,
        task_manager=task_manager, workflow_engine=workflow_engine,
    )

    # Plugin system — discover and activate plugins
    from echo_agent.plugins.manager import PluginManager

    plugin_manager = PluginManager(
        config=config,
        workspace=ws,
        bus=bus,
        tool_registry=agent.tools,
        provider=provider,
    )
    await plugin_manager.discover_and_load()
    agent.set_plugin_manager(plugin_manager)

    # Self-evolving skill harness
    if config.evolution.enabled:
        try:
            from echo_agent.evaluation.dataset import EvalDataset
            from echo_agent.evaluation.runner import EvalRunner
            from echo_agent.evolution.engine import EvolutionEngine

            dataset_path = ws / config.evolution.eval_dataset_path
            if not dataset_path.is_absolute():
                dataset_path = (ws / config.evolution.eval_dataset_path).resolve()

            def _load_eval_dataset() -> EvalDataset:
                return EvalDataset.from_path(dataset_path)

            def _make_eval_runner() -> EvalRunner:
                return EvalRunner(
                    agent,
                    parallel=config.evolution.eval_parallel,
                    timeout=config.evolution.eval_timeout_seconds,
                    provider=provider,
                )

            reflection_module = None
            try:
                from echo_agent.agent.planning.reflection import ReflectionModule
                reflection_module = ReflectionModule(provider.chat_with_retry)
            except Exception as e:
                logger.debug("Reflection module unavailable for evolution: {}", e)

            evolution_engine = EvolutionEngine(
                config=config.evolution,
                workspace=ws,
                storage=storage,
                provider=provider,
                skill_store=agent.skill_store,
                skill_manager=None,
                eval_runner_factory=_make_eval_runner,
                eval_dataset_loader=_load_eval_dataset,
                hooks=plugin_manager.hooks,
                reflection=reflection_module,
                router=router,
            )
            agent.set_evolution_engine(evolution_engine)
            logger.info("Evolution engine attached (trigger={})", config.evolution.trigger_mode)
        except Exception as e:
            logger.warning("Failed to attach evolution engine: {}", e)

    channels = ChannelManager(config.channels, bus, on_cli_exit=on_cli_exit)
    health = HealthChecker(check_interval=config.observability.health_check_interval_seconds)

    from echo_agent.observability.monitor import ComponentHealth as CH

    async def _check_bus() -> CH:
        return CH.HEALTHY if bus.pending_inbound < 900 else CH.DEGRADED

    async def _check_agent() -> CH:
        return CH.HEALTHY if agent.is_running else CH.UNHEALTHY

    async def _check_storage() -> CH:
        return CH.HEALTHY if storage.is_connected else CH.UNHEALTHY

    health.register_check("bus", _check_bus)
    health.register_check("agent", _check_agent)
    health.register_check("storage", _check_storage)

    async def _session_cleanup() -> CH:
        count = await agent.sessions.cleanup_expired()
        if count:
            logger.info("Cleaned up {} expired sessions", count)
        return CH.HEALTHY

    health.register_check("session_cleanup", _session_cleanup)

    return BootstrapResult(
        config=config, workspace=ws, storage=storage, bus=bus,
        router=router, provider=provider, agent=agent,
        channels=channels, scheduler=scheduler, health=health,
    )


def install_signal_handler(shutdown: asyncio.Event) -> None:
    loop = asyncio.get_running_loop()
    for sig in (signal.SIGINT, signal.SIGTERM):
        try:
            loop.add_signal_handler(sig, shutdown.set)
        except NotImplementedError:
            pass


class AppRuntime:
    """Owns the ordered start/stop lifecycle of all optional components.

    ``stop()`` guards every step so a failure in one component (e.g. a stop
    timeout in the agent loop) can never prevent later steps — in particular
    the storage close — from running.
    """

    def __init__(self, ctx: BootstrapResult, shutdown_event: asyncio.Event | None = None):
        self._ctx = ctx
        self._gateway: Any = None
        self._started = False
        self._shutdown_event = shutdown_event

    @property
    def gateway(self) -> Any:
        return self._gateway

    async def start(self) -> bool:
        """Start all components. Returns False if there is nothing to serve
        (no active channels and gateway disabled), in which case the caller
        should call ``stop()`` and exit."""
        ctx = self._ctx
        self._started = True
        await ctx.bus.start()
        await ctx.agent.start()
        await ctx.channels.start_all()

        if not ctx.channels.active_channels and not ctx.config.gateway.enabled:
            logger.error(
                "No active input channels. Run in an interactive terminal, enable gateway, "
                "or configure another channel."
            )
            return False

        if ctx.scheduler:
            await ctx.scheduler.start()
        await ctx.health.start()

        if ctx.config.gateway.enabled:
            from echo_agent.gateway.server import GatewayServer
            self._gateway = GatewayServer(
                config=ctx.config.gateway,
                bus=ctx.bus,
                channel_manager=ctx.channels,
                session_manager=ctx.agent.sessions,
                workspace=ctx.workspace,
                agent_loop=ctx.agent,
                a2a_config=ctx.config.a2a,
            )
            if self._shutdown_event:
                self._gateway.set_shutdown_event(self._shutdown_event)
            await self._gateway.start()
            logger.info("Gateway started on {}:{}", ctx.config.gateway.host, ctx.config.gateway.port)
        return True

    async def stop(self) -> None:
        if not self._started:
            return
        self._started = False
        ctx = self._ctx
        if self._gateway:
            await self._stop_step("gateway", self._gateway.stop())
            self._gateway = None
        await self._stop_step("health", ctx.health.stop())
        if ctx.scheduler:
            await self._stop_step("scheduler", ctx.scheduler.stop())
        await self._stop_step("channels", ctx.channels.stop_all())
        await self._stop_step("agent", ctx.agent.stop())
        await self._stop_step("bus", ctx.bus.stop())
        await self._stop_step("storage", ctx.storage.close())

    @staticmethod
    async def _stop_step(name: str, coro: Any) -> None:
        try:
            await coro
        except asyncio.CancelledError:
            raise
        except Exception as e:
            logger.warning("Error stopping {}: {}", name, e)


async def run(config_path: str | None = None, workspace: str | None = None) -> None:
    """Run the full agent (``echo-agent run``)."""
    if config_path is None and workspace:
        from echo_agent.config.loader import resolve_config_file
        config_path = str(resolve_config_file(search_dir=workspace) or "")
    overrides = {"workspace": workspace} if workspace else None
    shutdown = asyncio.Event()
    ctx = await bootstrap(config_path=config_path, overrides=overrides, on_cli_exit=shutdown.set)

    logger.info("Echo Agent starting — workspace: {}", ctx.workspace)

    install_signal_handler(shutdown)
    runtime = AppRuntime(ctx, shutdown_event=shutdown)
    try:
        if not await runtime.start():
            return
        logger.info("Echo Agent ready — channels: {}", ctx.channels.active_channels)
        await shutdown.wait()
        logger.info("Shutting down...")
    finally:
        await runtime.stop()
    logger.info("Echo Agent stopped")


def _apply_gateway_profile_default(config: "Config", config_path: str | None) -> None:
    """Tighten the gateway entrypoint to ``public_gateway`` when the user did
    not explicitly choose a ``security.profile``. Explicit config is respected."""
    from echo_agent.config.loader import profile_explicitly_set

    if not profile_explicitly_set(config_path):
        config.security.profile = "public_gateway"
        logger.warning(
            "Gateway 入口未显式配置 security.profile，已默认切到 public_gateway 收紧档；"
            "如需放开请在配置中显式设置 security.profile"
        )


async def run_gateway(
    config_path: str | None = None,
    host: str | None = None,
    port: int | None = None,
    workspace: str | None = None,
) -> None:
    """Run in gateway mode (``echo-agent gateway``) — same lifecycle as ``run``
    with the gateway force-enabled, so health checks and the scheduler are
    started here too."""
    if config_path is None and workspace:
        from echo_agent.config.loader import resolve_config_file
        config_path = str(resolve_config_file(search_dir=workspace) or "")
    overrides: dict[str, Any] = {"workspace": workspace} if workspace else {}
    shutdown = asyncio.Event()
    ctx = await bootstrap(config_path=config_path, overrides=overrides or None, on_cli_exit=shutdown.set)
    ctx.config.gateway.enabled = True
    _apply_gateway_profile_default(ctx.config, config_path)
    if host:
        ctx.config.gateway.host = host
    if port:
        ctx.config.gateway.port = port

    install_signal_handler(shutdown)
    runtime = AppRuntime(ctx, shutdown_event=shutdown)
    try:
        if not await runtime.start():
            return
        logger.info("Gateway listening on {}:{}", ctx.config.gateway.host, ctx.config.gateway.port)
        await shutdown.wait()
    finally:
        await runtime.stop()
