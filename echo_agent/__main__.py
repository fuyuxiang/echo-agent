"""Echo Agent entry point — bootstraps all subsystems and runs the agent."""

from __future__ import annotations

import argparse
import asyncio
import signal
import sys
from pathlib import Path

from loguru import logger


def _configure_logging(level: str) -> None:
    logger.remove()
    logger.add(sys.stderr, level=level, format="<green>{time:HH:mm:ss}</green> | <level>{level:<7}</level> | {message}")


async def _run(config_path: str | None = None, workspace: str | None = None) -> None:
    from echo_agent.agent.loop import AgentLoop
    from echo_agent.bus.queue import MessageBus
    from echo_agent.channels.manager import ChannelManager
    from echo_agent.config.loader import load_config
    from echo_agent.observability.monitor import HealthChecker
    from echo_agent.scheduler.service import Scheduler
    from echo_agent.storage.sqlite import SQLiteBackend

    overrides = {}
    if workspace:
        overrides["workspace"] = workspace

    config = load_config(config_path=config_path, overrides=overrides if overrides else None)
    ws = Path(config.workspace).resolve()
    ws.mkdir(parents=True, exist_ok=True)

    _configure_logging(config.observability.log_level)
    logger.info("Echo Agent starting — workspace: {}", ws)

    storage = SQLiteBackend(ws / config.storage.database_path)
    await storage.initialize()

    bus = MessageBus()

    # Build LLM provider from config
    from echo_agent.models.provider import LLMProvider, LLMResponse
    from echo_agent.models.providers import create_provider
    from echo_agent.models.router import ModelRouter

    provider: LLMProvider | None = None
    router = ModelRouter(config.models)

    for pc in config.models.providers:
        try:
            p = create_provider(pc)
            router.register_provider(pc.name, p)
            if provider is None:
                provider = p
            logger.info("Registered provider: {}", pc.name)
        except Exception as e:
            logger.warning("Failed to create provider '{}': {}", pc.name, e)

    if provider is None:
        class _StubProvider(LLMProvider):
            async def chat(self, messages, tools=None, model=None, tool_choice=None, **kw):
                return LLMResponse(content="[No LLM provider configured. Set up a provider in echo-agent.yaml]")
            def get_default_model(self):
                return "stub"
        provider = _StubProvider()
        logger.warning("No providers configured — using stub")

    scheduler: Scheduler | None = None
    if config.scheduler.enabled:
        scheduler = Scheduler(
            store_path=ws / "data" / "scheduler.json",
            max_concurrent=config.scheduler.max_concurrent_jobs,
        )

    agent = AgentLoop(bus=bus, config=config, provider=provider, workspace=ws, scheduler=scheduler)
    channels = ChannelManager(config.channels, bus)

    health = HealthChecker(check_interval=config.observability.health_check_interval_seconds)

    shutdown = asyncio.Event()

    def _signal_handler() -> None:
        logger.info("Shutdown signal received")
        shutdown.set()

    loop = asyncio.get_event_loop()
    for sig in (signal.SIGINT, signal.SIGTERM):
        try:
            loop.add_signal_handler(sig, _signal_handler)
        except NotImplementedError:
            pass

    await bus.start()
    await agent.start()
    await channels.start_all()
    if scheduler:
        await scheduler.start()
    await health.start()

    gateway = None
    if config.gateway.enabled:
        from echo_agent.gateway.server import GatewayServer
        gateway = GatewayServer(
            config=config.gateway,
            bus=bus,
            channel_manager=channels,
            session_manager=agent.sessions,
            workspace=ws,
        )
        await gateway.start()
        logger.info("Gateway started on {}:{}", config.gateway.host, config.gateway.port)

    logger.info("Echo Agent ready — channels: {}", channels.active_channels)

    await shutdown.wait()

    logger.info("Shutting down...")
    if gateway:
        await gateway.stop()
    await health.stop()
    if scheduler:
        await scheduler.stop()
    await channels.stop_all()
    await agent.stop()
    await bus.stop()
    await storage.close()
    logger.info("Echo Agent stopped")


def main() -> None:
    parser = argparse.ArgumentParser(description="Echo Agent")
    subparsers = parser.add_subparsers(dest="command")

    # setup subcommand
    setup_parser = subparsers.add_parser("setup", help="Run interactive setup wizard")
    setup_parser.add_argument("section", nargs="?", default=None,
                              help="Setup section: model, channel, advanced")

    # default run arguments (also accepted at top level)
    parser.add_argument("-c", "--config", help="Path to config file")
    parser.add_argument("-w", "--workspace", help="Workspace directory")
    args = parser.parse_args()

    if args.command == "setup":
        from echo_agent.cli.setup import run_setup_wizard
        run_setup_wizard(section=args.section)
        return

    # First-run detection: prompt setup if no config exists
    from echo_agent.cli.setup import prompt_first_run_setup
    if prompt_first_run_setup():
        return

    try:
        asyncio.run(_run(config_path=args.config, workspace=args.workspace))
    except KeyboardInterrupt:
        pass


if __name__ == "__main__":
    main()
