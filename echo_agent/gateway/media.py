"""Media cache — download, store, and manage media files for the gateway."""

from __future__ import annotations

import hashlib
import time
from pathlib import Path
from typing import Any

import aiohttp
from loguru import logger


class MediaCache:

    def __init__(self, cache_dir: Path, max_size_mb: int = 500):
        self._cache_dir = cache_dir
        self._cache_dir.mkdir(parents=True, exist_ok=True)
        self._max_bytes = max_size_mb * 1024 * 1024

    async def download(
        self,
        url: str,
        platform: str,
        headers: dict[str, str] | None = None,
    ) -> Path | None:
        url_hash = hashlib.sha256(url.encode()).hexdigest()[:16]
        ext = self._guess_extension(url)
        platform_dir = self._cache_dir / platform
        platform_dir.mkdir(parents=True, exist_ok=True)
        target = platform_dir / f"{url_hash}{ext}"

        if target.exists():
            target.touch()
            return target

        try:
            async with aiohttp.ClientSession() as session:
                async with session.get(url, headers=headers, timeout=aiohttp.ClientTimeout(total=60)) as resp:
                    if resp.status != 200:
                        logger.warning("Media download failed ({}): {}", resp.status, url)
                        return None

                    content_type = resp.headers.get("Content-Type", "")
                    if not ext and content_type:
                        ext = self._ext_from_content_type(content_type)
                        target = platform_dir / f"{url_hash}{ext}"

                    data = await resp.read()
                    target.write_bytes(data)
                    logger.debug("Cached media: {} → {}", url[:80], target.name)
                    return target

        except Exception as e:
            logger.error("Media download error for {}: {}", url[:80], e)
            return None

    def get_cached(self, url: str) -> Path | None:
        url_hash = hashlib.sha256(url.encode()).hexdigest()[:16]
        for path in self._cache_dir.rglob(f"{url_hash}*"):
            if path.is_file():
                path.touch()
                return path
        return None

    async def cleanup(self) -> int:
        total_size = 0
        files: list[tuple[Path, float, int]] = []

        for path in self._cache_dir.rglob("*"):
            if not path.is_file():
                continue
            stat = path.stat()
            total_size += stat.st_size
            files.append((path, stat.st_mtime, stat.st_size))

        if total_size <= self._max_bytes:
            return 0

        files.sort(key=lambda x: x[1])
        removed = 0
        for path, _, size in files:
            if total_size <= self._max_bytes:
                break
            try:
                path.unlink()
                total_size -= size
                removed += 1
            except OSError:
                pass

        if removed:
            logger.info("Media cache cleanup: removed {} files", removed)
        return removed

    def get_size_mb(self) -> float:
        total = sum(
            p.stat().st_size for p in self._cache_dir.rglob("*") if p.is_file()
        )
        return total / (1024 * 1024)

    def _guess_extension(self, url: str) -> str:
        path = url.split("?")[0].split("#")[0]
        if "." in path.split("/")[-1]:
            ext = "." + path.split("/")[-1].rsplit(".", 1)[-1].lower()
            if len(ext) <= 5:
                return ext
        return ""

    def _ext_from_content_type(self, ct: str) -> str:
        mapping = {
            "image/jpeg": ".jpg",
            "image/png": ".png",
            "image/gif": ".gif",
            "image/webp": ".webp",
            "audio/mpeg": ".mp3",
            "audio/ogg": ".ogg",
            "audio/wav": ".wav",
            "video/mp4": ".mp4",
            "application/pdf": ".pdf",
        }
        base = ct.split(";")[0].strip().lower()
        return mapping.get(base, "")
