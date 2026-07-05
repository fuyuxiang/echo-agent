from __future__ import annotations

from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, Protocol, runtime_checkable


@dataclass
class UnderstandResult:
    text: str                                  # understood text; "" means nothing to inject
    kind: str                                  # "transcript" | "caption" | ...
    metadata: dict[str, str] = field(default_factory=dict)


@runtime_checkable
class MediaUnderstanding(Protocol):
    def can_handle(self, block: Any) -> bool: ...
    async def understand(self, path: Path, block: Any) -> UnderstandResult: ...
