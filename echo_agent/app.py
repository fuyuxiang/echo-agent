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
    from echo_agent.observability.log_buffer import install_log_buffer

    logger.remove()
    logger.add(sys.stderr, level=level, format="<green>{time:HH:mm:ss}</green> | <level>{level:<7}</level> | {message}")
    # Buffer records in memory so the dashboard's /api/logs endpoint has history
    # to serve; the stderr sink alone keeps nothing queryable.
    install_log_buffer(level=level)


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
    instance_lock: Any = None


async def bootstrap(
    config_path: str | None = None,
    overrides: dict[str, Any] | None = None,
    on_cli_exit: Callable[[], None] | None = None,
    *,
    single_instance: bool = False,
    force: bool = False,
    role: str = "run",
) -> BootstrapResult:
    """Shared bootstrap: config → storage → providers → bus → agent → channels.

    When ``single_instance`` is set (the channel-consuming entrypoints), the
    workspace lock is acquired *before* opening SQLite or running migrations, so
    a second process against the same workspace bails out here instead of
    concurrently initializing the database. On conflict this raises
    :class:`InstanceLockError` before any resource is opened — nothing to leak.
    ``force`` / ``runtime.single_instance=false`` disable the guard."""
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

    # 不带 -c 时用 workspace 作查找目录,避免子命令回退到 ~/.echo-agent 的全局
    # 配置(与 run_gateway 的预解析、cost/config 命令行为保持一致)。
    search_dir = overrides.get("workspace") if overrides else None
    config_file = resolve_config_file(config_path, search_dir=search_dir)
    config = load_config(config_path=config_file, overrides=overrides)
    configure_logging(config.observability.log_level)

    workspace_value = Path(config.workspace).expanduser()
    if not workspace_value.is_absolute():
        workspace_base = Path.cwd() if overrides and "workspace" in overrides else (config_file.parent if config_file else Path.cwd())
        workspace_value = workspace_base / workspace_value
    ws = workspace_value.resolve()
    ws.mkdir(parents=True, exist_ok=True)

    # Acquire the single-instance lock before opening SQLite / running migrations
    # so a duplicate process bails out here rather than concurrently initializing
    # the database. Raising before any resource is opened means nothing to leak.
    instance_lock: Any = None
    if single_instance and config.runtime.single_instance and not force:
        from echo_agent.runtime_lock import acquire_instance_lock
        instance_lock = acquire_instance_lock(ws, role=role)

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

    from echo_agent.scheduler.service import Scheduler, ScheduledJob, TriggerKind
    scheduler: Scheduler | None = None
    if config.scheduler.enabled:
        inspection_runner = None
        insp_cfg = config.agent.inspection
        if insp_cfg.enabled:
            from echo_agent.agent.inspection.store import InspectStore
            from echo_agent.agent.inspection.tick import run_inspection_tick
            insp_store = InspectStore(
                ws / insp_cfg.inspect_file,
                ws / "data" / "inspect_state.json",
            )

            async def inspection_runner():
                await run_inspection_tick(insp_store, insp_cfg, bus)

        scheduler = Scheduler(
            store_path=ws / "data" / "scheduler.json",
            on_job=build_scheduled_job_handler(bus, inspection_runner=inspection_runner),
            max_concurrent=config.scheduler.max_concurrent_jobs,
        )
        if insp_cfg.enabled and not any(
            j.name == "__inspection_tick__" for j in scheduler.list_jobs()
        ):
            scheduler.add_job(ScheduledJob(
                name="__inspection_tick__",
                trigger=TriggerKind.INTERVAL,
                interval_ms=insp_cfg.tick_interval_sec * 1000,
                payload={"_inspection_tick": True},
            ))

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

    # Checkpoint safety net — snapshot workspace before write tools (fail-open)
    try:
        from echo_agent.checkpoint.hook import install_checkpoint
        install_checkpoint(config, ws, plugin_manager.hooks)
    except Exception as e:
        logger.debug("checkpoint install failed (fail-open): {}", e)

    # Post-write validation — lint the written file, feed errors back (fail-open)
    try:
        from echo_agent.validation.hook import install_validation
        install_validation(config, ws, plugin_manager.hooks)
    except Exception as e:
        logger.debug("validation install failed (fail-open): {}", e)

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
    # Wire the real heartbeat config so verbosity (key_milestones/every_tool/
    # silent) actually takes effect at runtime; the manager otherwise defaults.
    channels._heartbeat_cfg = config.agent.heartbeat
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
        instance_lock=instance_lock,
    )


def install_signal_handler(shutdown: asyncio.Event) -> None:
    loop = asyncio.get_running_loop()
    for sig in (signal.SIGINT, signal.SIGTERM):
        try:
            loop.add_signal_handler(sig, shutdown.set)
        except NotImplementedError:
            pass


def _is_supervised() -> bool:
    """Best-effort: is this process managed by a supervisor that respawns it?

    systemd sets INVOCATION_ID; our launchd/systemd unit templates set
    _ECHO_AGENT_SUPERVISED=1. If neither is present we assume a foreground run,
    where a self-exit would leave the service dead — so the watchdog degrades to
    warn-only rather than exiting.
    """
    import os
    return bool(os.environ.get("INVOCATION_ID") or os.environ.get("_ECHO_AGENT_SUPERVISED"))


def build_loop_watchdog(ctx: "BootstrapResult") -> "Any | None":
    """Construct a LoopWatchdog from config, or None when disabled."""
    obs = ctx.config.observability
    if not getattr(obs, "loop_watchdog_enabled", True):
        return None
    from echo_agent.observability.loop_watchdog import LoopWatchdog
    from echo_agent.observability.restart_guard import RestartGuard

    guard = RestartGuard(
        ctx.workspace / "data" / "watchdog_restarts.json",
        max_restarts=obs.loop_watchdog_max_restarts_per_hour,
    )
    return LoopWatchdog(
        warn_seconds=obs.loop_watchdog_warn_seconds,
        kill_seconds=obs.loop_watchdog_kill_seconds,
        check_interval_seconds=obs.loop_watchdog_check_interval_seconds,
        restart_guard=guard,
        supervised=_is_supervised(),
        dump_path=ctx.workspace / ctx.config.storage.logs_dir / "loop_freeze.log",
    )


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
        self._instance_lock: Any = None

    @property
    def gateway(self) -> Any:
        return self._gateway

    async def start(self) -> bool:
        """Start all components. Returns False if there is nothing to serve
        (no active channels and gateway disabled), in which case the caller
        should call ``stop()`` and exit.

        The workspace single-instance lock is acquired in ``bootstrap`` (before
        SQLite is opened), not here — this runtime only owns releasing it on
        ``stop()`` via ``ctx.instance_lock``."""
        ctx = self._ctx
        self._instance_lock = ctx.instance_lock
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
        if self._instance_lock is not None:
            try:
                self._instance_lock.release()
            except Exception as e:
                logger.warning("Error releasing instance lock: {}", e)
            self._instance_lock = None

    @staticmethod
    async def _stop_step(name: str, coro: Any) -> None:
        try:
            await coro
        except asyncio.CancelledError:
            raise
        except Exception as e:
            logger.warning("Error stopping {}: {}", name, e)


async def run(config_path: str | None = None, workspace: str | None = None, force: bool = False) -> None:
    """Run the full agent (``echo-agent run``)."""
    if config_path is None and workspace:
        from echo_agent.config.loader import resolve_config_file
        config_path = str(resolve_config_file(search_dir=workspace) or "")
    overrides = {"workspace": workspace} if workspace else None
    shutdown = asyncio.Event()
    from echo_agent.runtime_lock import InstanceLockError
    try:
        ctx = await bootstrap(
            config_path=config_path, overrides=overrides, on_cli_exit=shutdown.set,
            single_instance=True, force=force, role="run",
        )
    except InstanceLockError as e:
        logger.error(e.message)
        return

    logger.info("Echo Agent starting — workspace: {}", ctx.workspace)

    install_signal_handler(shutdown)
    runtime = AppRuntime(ctx, shutdown_event=shutdown)
    watchdog = build_loop_watchdog(ctx)
    try:
        if not await runtime.start():
            return
        if watchdog is not None:
            watchdog.start()
        logger.info("Echo Agent ready — channels: {}", ctx.channels.active_channels)
        await shutdown.wait()
        logger.info("Shutting down...")
    finally:
        if watchdog is not None:
            await watchdog.stop()
        await runtime.stop()
    logger.info("Echo Agent stopped")


def _apply_gateway_profile_default(config: "Config", config_path: str | None) -> None:
    """Tighten the gateway entrypoint to ``public_gateway`` when the user did
    not explicitly choose a ``security.profile``. Explicit config is respected.

    NOTE: profile tightening must be injected into ``bootstrap`` overrides
    *before* the agent loop registers its tools — see ``run_gateway``. This
    helper only re-asserts the field on the resolved config as a guard; on its
    own it does not re-filter an already-built tool registry."""
    from echo_agent.config.loader import profile_explicitly_set

    if not profile_explicitly_set(config_path):
        config.security.profile = "public_gateway"
        logger.warning(
            "Gateway 入口未显式配置 security.profile，已默认切到 public_gateway 收紧档；"
            "如需放开请在配置中显式设置 security.profile"
        )


def _gateway_profile_override(config_path: str | None) -> dict[str, Any]:
    """Build the bootstrap override that tightens the gateway security profile
    when the user did not set one explicitly. Returning an override (rather than
    mutating config post-bootstrap) is what makes registration-time tool
    filtering see ``public_gateway`` — otherwise high-risk tools (exec,
    write_file, patch, workflow, ...) would already be registered and would
    remain callable, since native tools have no per-call profile gate."""
    from echo_agent.config.loader import (
        _load_yaml_file,
        profile_explicitly_set,
        resolve_config_file,
    )

    if profile_explicitly_set(config_path):
        return {}

    # If the user asked for a broad tool profile (full/coding) but left
    # security.profile implicit, the public_gateway downgrade will silently strip
    # exec/write_file/execute_code/patch — the tools that profile was meant to
    # grant. Surface that conflict loudly rather than as a soft "已收紧" note, and
    # point at the exact fix. (This is the failure that made a document-generation
    # task come back empty with no clue why.)
    tools_profile = ""
    try:
        path = resolve_config_file(config_path)
        user_yaml = _load_yaml_file(path if path and path.exists() else None)
        tools_section = user_yaml.get("tools")
        if isinstance(tools_section, dict):
            tools_profile = str(tools_section.get("profile") or "")
    except Exception as e:  # best-effort: never let the warning path break boot
        logger.debug("Could not read tools.profile for gateway conflict check: {}", e)

    if tools_profile in ("full", "coding"):
        logger.warning(
            "配置冲突：tools.profile={} 想启用全部/编码类工具，但 Gateway 入口未显式配置 "
            "security.profile，已默认切到 public_gateway 收紧档，会关闭 "
            "exec/execute_code/write_file/edit_file/patch/process 等高危工具——"
            "full/coding 的相应能力将失效。如需恢复：在配置中显式设置 "
            "security.profile: personal_cli（私人自用，全工具生效），或保留 "
            "public_gateway 并在 tools.also_allow 里按名单单独放行所需工具。",
            tools_profile,
        )
    else:
        logger.warning(
            "Gateway 入口未显式配置 security.profile，已默认切到 public_gateway 收紧档；"
            "如需放开请在配置中显式设置 security.profile"
        )
    return {"security": {"profile": "public_gateway"}}


def _gateway_port_in_use(host: str, port: int) -> str | None:
    """尽力（best-effort）探测 ``host:port`` 是否已被占用；占用时返回一条
    面向用户的友好提示，否则 None。

    这是一个尽力预检，非权威判定：权威判定在 ``GatewayServer.start()`` 的
    EADDRINUSE 包装（server.py），它对真实 bind 失败给出同样的提示。本函数
    对 ``0.0.0.0`` / ``::`` 仅探测 ``127.0.0.1``，因此可能漏报 IPv6 或指定
    网卡地址的占用——那种情况仍会走到 start() 的 EADDRINUSE 兜底。用一个
    throwaway socket（SO_REUSEADDR off）探测，尽量贴近 aiohttp 的 bind 行为。
    Port 0 是 ephemeral 哨兵——永不「占用」，跳过探测。"""
    if not port:
        return None
    import socket

    probe_host = "127.0.0.1" if host in ("", "0.0.0.0", "::") else host
    family = socket.AF_INET6 if ":" in probe_host else socket.AF_INET
    with socket.socket(family, socket.SOCK_STREAM) as sock:
        try:
            sock.bind((probe_host, port))
        except OSError:
            return (
                f"网关端口 {host}:{port} 已被占用，可能本机已有一个常驻 echo-agent 在运行。"
                "若要接入它请用 `echo-agent cli`；若要另起实例请用 `--port` 指定其它端口。"
            )
    return None


async def run_gateway(
    config_path: str | None = None,
    host: str | None = None,
    port: int | None = None,
    workspace: str | None = None,
    force: bool = False,
) -> None:
    """Run in gateway mode (``echo-agent gateway``) — same lifecycle as ``run``
    with the gateway force-enabled, so health checks and the scheduler are
    started here too."""
    if config_path is None and workspace:
        from echo_agent.config.loader import resolve_config_file
        config_path = str(resolve_config_file(search_dir=workspace) or "")

    # Preflight the listen port BEFORE bootstrap(), so an occupied port (a
    # resident gateway already running) exits with a friendly hint and never
    # leaves a half-built storage handle open (bootstrap creates SQLiteBackend;
    # AppRuntime.stop() is a no-op before start(), so a post-bootstrap bail
    # would leak the connection). Resolve host/port from args first, else config.
    from echo_agent.config.loader import load_config, resolve_config_file
    _cfg_file = resolve_config_file(config_path)
    _cfg = load_config(config_path=_cfg_file)
    pre_host = host or _cfg.gateway.host
    # port=0 is the "pick an ephemeral port" sentinel and must survive: use the
    # config port only when --port was omitted (None), not when it is 0.
    pre_port = port if port is not None else _cfg.gateway.port
    bind_err = _gateway_port_in_use(pre_host, pre_port)
    if bind_err:
        logger.error(bind_err)
        return

    # Mark this process as the gateway so `echo-agent gateway stop/restart`
    # issued from inside it (agent exec tool) can refuse — with the service
    # manager's KeepAlive/Restart=always that would be a kill/respawn loop.
    import os as _os
    _os.environ["_ECHO_AGENT_GATEWAY"] = "1"

    overrides: dict[str, Any] = {"workspace": workspace} if workspace else {}
    # Force gateway on and tighten the security profile *before* bootstrap so the
    # agent loop registers tools under the effective gateway policy. Applying
    # these after bootstrap would leave already-registered high-risk tools in the
    # registry (see _gateway_profile_override).
    overrides.setdefault("gateway", {})["enabled"] = True
    profile_override = _gateway_profile_override(config_path)
    if profile_override:
        overrides["security"] = {**overrides.get("security", {}), **profile_override["security"]}
    shutdown = asyncio.Event()
    from echo_agent.runtime_lock import InstanceLockError
    try:
        ctx = await bootstrap(
            config_path=config_path, overrides=overrides or None, on_cli_exit=shutdown.set,
            single_instance=True, force=force, role="gateway",
        )
    except InstanceLockError as e:
        logger.error(e.message)
        return
    ctx.config.gateway.enabled = True
    if host:
        ctx.config.gateway.host = host
    # Apply --port when provided, including 0 (ephemeral). Only None means
    # "not passed"; `if port` would drop the dynamic-port request.
    if port is not None:
        ctx.config.gateway.port = port

    install_signal_handler(shutdown)
    runtime = AppRuntime(ctx, shutdown_event=shutdown)
    watchdog = build_loop_watchdog(ctx)

    try:
        if not await runtime.start():
            return
        if watchdog is not None:
            watchdog.start()
        logger.info("Gateway listening on {}:{}", ctx.config.gateway.host, ctx.config.gateway.port)
        await shutdown.wait()
    finally:
        if watchdog is not None:
            await watchdog.stop()
        await runtime.stop()
