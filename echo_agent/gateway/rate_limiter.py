"""Token-bucket rate limiter for gateway platforms."""

from __future__ import annotations

import asyncio
import time
from collections import defaultdict
from dataclasses import dataclass, field


@dataclass
class _Bucket:
    tokens: float = 0.0
    last_refill: float = field(default_factory=time.time)
    capacity: float = 30.0
    refill_rate: float = 0.5  # tokens per second


class RateLimiter:

    def __init__(self, default_rpm: int = 30):
        self._default_rpm = default_rpm
        self._platform_limits: dict[str, int] = {}
        self._buckets: dict[str, _Bucket] = defaultdict(
            lambda: self._make_bucket(self._default_rpm),
        )

    def configure(self, platform: str, rpm: int) -> None:
        self._platform_limits[platform] = rpm

    def acquire(self, platform: str, chat_id: str = "") -> bool:
        key = f"{platform}:{chat_id}" if chat_id else platform
        bucket = self._get_bucket(key, platform)
        self._refill(bucket)
        if bucket.tokens >= 1.0:
            bucket.tokens -= 1.0
            return True
        return False

    async def wait(self, platform: str, chat_id: str = "") -> None:
        while not self.acquire(platform, chat_id):
            await asyncio.sleep(0.5)

    def get_stats(self) -> dict[str, dict[str, float]]:
        stats: dict[str, dict[str, float]] = {}
        for key, bucket in self._buckets.items():
            self._refill(bucket)
            stats[key] = {
                "tokens_available": round(bucket.tokens, 1),
                "capacity": bucket.capacity,
            }
        return stats

    def _get_bucket(self, key: str, platform: str) -> _Bucket:
        if key not in self._buckets:
            rpm = self._platform_limits.get(platform, self._default_rpm)
            self._buckets[key] = self._make_bucket(rpm)
        return self._buckets[key]

    def _make_bucket(self, rpm: int) -> _Bucket:
        capacity = float(rpm)
        refill_rate = rpm / 60.0
        return _Bucket(tokens=capacity, capacity=capacity, refill_rate=refill_rate)

    def _refill(self, bucket: _Bucket) -> None:
        now = time.time()
        elapsed = now - bucket.last_refill
        bucket.tokens = min(
            bucket.capacity,
            bucket.tokens + elapsed * bucket.refill_rate,
        )
        bucket.last_refill = now
