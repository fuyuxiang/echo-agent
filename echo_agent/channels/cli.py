"""CLI channel — interactive terminal input/output."""

from __future__ import annotations

import asyncio
import sys

from loguru import logger

from echo_agent.bus.events import OutboundEvent
from echo_agent.bus.queue import MessageBus
from echo_agent.channels.base import BaseChannel
from echo_agent.config.schema import CLIChannelConfig


class CLIChannel(BaseChannel):
    name = "cli"

    def __init__(self, config: CLIChannelConfig, bus: MessageBus):
        super().__init__(config, bus)
        self._task: asyncio.Task | None = None

    async def start(self) -> None:
        self._running = True
        self.bus.subscribe_outbound(self.name, self.send)
        self._task = asyncio.create_task(self._read_loop())
        logger.info("CLI channel started")

    async def stop(self) -> None:
        self._running = False
        if self._task:
            self._task.cancel()
            try:
                await self._task
            except asyncio.CancelledError:
                pass

    async def send(self, event: OutboundEvent) -> None:
        text = event.text
        if text:
            print(f"\n{text}\n")

    async def _read_loop(self) -> None:
        loop = asyncio.get_event_loop()
        while self._running:
            try:
                line = await loop.run_in_executor(None, self._read_line)
                if line is None:
                    break
                line = line.strip()
                if not line:
                    continue
                if line.lower() in ("exit", "quit", "/quit"):
                    self._running = False
                    break
                await self._handle_message(
                    sender_id="cli_user",
                    chat_id="cli",
                    text=line,
                    session_key="cli:cli",
                )
            except asyncio.CancelledError:
                break
            except Exception as e:
                logger.error("CLI read error: {}", e)

    @staticmethod
    def _read_line() -> str | None:
        try:
            return input("You> ")
        except (EOFError, KeyboardInterrupt):
            return None
