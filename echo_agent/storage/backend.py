"""Storage layer — abstract backend with SQLite default implementation.

Separates: session, memory, task, file, log, and vector stores.
"""

from __future__ import annotations

import json
import sqlite3
from abc import ABC, abstractmethod
from datetime import datetime
from pathlib import Path
from typing import Any

from loguru import logger


class StorageBackend(ABC):
    """Abstract storage backend interface."""

    @abstractmethod
    async def initialize(self) -> None:
        """Set up storage (create tables, directories, etc.)."""

    @abstractmethod
    async def close(self) -> None:
        """Clean up resources."""

    @abstractmethod
    async def store_session(self, key: str, data: dict[str, Any]) -> None:
        pass

    @abstractmethod
    async def load_session(self, key: str) -> dict[str, Any] | None:
        pass

    @abstractmethod
    async def store_memory(self, entry_id: str, data: dict[str, Any]) -> None:
        pass

    @abstractmethod
    async def load_memories(self, mem_type: str | None = None) -> list[dict[str, Any]]:
        pass

    @abstractmethod
    async def store_task(self, task_id: str, data: dict[str, Any]) -> None:
        pass

    @abstractmethod
    async def load_task(self, task_id: str) -> dict[str, Any] | None:
        pass

    @abstractmethod
    async def store_log(self, trace_id: str, spans: list[dict[str, Any]]) -> None:
        pass

    @abstractmethod
    async def query_logs(self, filters: dict[str, Any] | None = None, limit: int = 100) -> list[dict[str, Any]]:
        pass
