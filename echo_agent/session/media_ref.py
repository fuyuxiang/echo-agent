"""Lightweight media reference for persisting image metadata across turns."""

from __future__ import annotations

import time
from dataclasses import dataclass, field
from typing import Any


@dataclass
class MediaRef:
    """A JSON-serializable pointer to a cached media file.

    Stored in session messages so that ``build_messages`` can re-attach images
    from previous turns without embedding base64 data in the session store."""

    cache_path: str
    original_url: str
    mime_type: str
    timestamp: float = field(default_factory=time.time)
    aes_key: str = ""

    def to_dict(self) -> dict[str, Any]:
        d: dict[str, Any] = {
            "cache_path": self.cache_path,
            "original_url": self.original_url,
            "mime_type": self.mime_type,
            "timestamp": self.timestamp,
        }
        if self.aes_key:
            d["aes_key"] = self.aes_key
        return d

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> MediaRef:
        return cls(
            cache_path=data.get("cache_path", ""),
            original_url=data.get("original_url", ""),
            mime_type=data.get("mime_type", ""),
            timestamp=data.get("timestamp", 0.0),
            aes_key=data.get("aes_key", ""),
        )
