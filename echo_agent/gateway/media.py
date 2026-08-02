"""Media cache — download, store, and manage media files for the gateway."""

from __future__ import annotations

import asyncio
import hashlib
from pathlib import Path

from loguru import logger

from echo_agent.security.net_guard import FetchLimits, GuardedFetchError, guarded_download


class MediaCache:
    """Content-addressed cache of downloaded media.

    Every URL reaching ``download`` is untrusted: it comes from a POST /message
    body or an inbound chat attachment, both attacker-controlled. Downloads
    therefore run through ``security.net_guard.guarded_download``, which applies
    the same SSRF policy as web_fetch (scheme allowlist, private/metadata
    address block, DNS pinning, per-hop redirect re-validation) plus a hard size
    ceiling enforced on the real byte stream.
    """

    def __init__(
        self,
        cache_dir: Path,
        max_size_mb: int = 500,
        *,
        max_file_mb: int = 25,
        concurrency: int = 4,
        allow_private: bool = False,
    ):
        self._cache_dir = cache_dir
        self._cache_dir.mkdir(parents=True, exist_ok=True)
        self._max_bytes = max_size_mb * 1024 * 1024
        self._max_file_bytes = max(1, max_file_mb) * 1024 * 1024
        self._allow_private = allow_private
        # Bounds parallel downloads across the whole process, not per call site:
        # callers gather() over a message's URLs, and several messages can be in
        # flight at once.
        self._slots = asyncio.Semaphore(max(1, concurrency))

    @property
    def cache_dir(self) -> Path:
        return self._cache_dir

    async def download(
        self,
        url: str,
        platform: str,
        headers: dict[str, str] | None = None,
    ) -> Path | None:
        """Fetch *url* into the cache, returning its path or None on failure.

        Returns None rather than raising: a rejected or oversized attachment
        must degrade to "no media" for that one block, never fail the whole
        turn. The reason is logged — an SSRF rejection at warning level, since
        it may be a probe worth noticing.
        """
        url_hash = hashlib.sha256(url.encode()).hexdigest()[:16]
        ext = self._guess_extension(url)
        platform_dir = self._cache_dir / platform
        platform_dir.mkdir(parents=True, exist_ok=True)
        target = platform_dir / f"{url_hash}{ext}"

        if target.exists():
            target.touch()
            return target

        # The served Content-Type names the extension when the URL does not.
        # Captured from the final hop's headers so a redirect chain's last
        # response decides, and applied by renaming after the bytes have landed.
        served_type: list[str] = []

        try:
            async with self._slots:
                await guarded_download(
                    url,
                    target,
                    headers=headers,
                    limits=FetchLimits(max_bytes=self._max_file_bytes),
                    allow_private=self._allow_private,
                    on_content_type=served_type.append,
                )
        except GuardedFetchError as e:
            level = logger.warning if e.blocked_by_policy else logger.info
            level("Media download refused for {}: {}", url[:80], e)
            return None
        except Exception as e:
            logger.error("Media download error for {}: {}", url[:80], e)
            return None

        if not ext and served_type:
            sniffed = self._ext_from_content_type(served_type[0])
            if sniffed:
                renamed = platform_dir / f"{url_hash}{sniffed}"
                try:
                    target.replace(renamed)
                    target = renamed
                except OSError as e:
                    logger.debug("Could not apply sniffed extension {}: {}", sniffed, e)

        logger.debug("Cached media: {} → {}", url[:80], target.name)
        return target

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
            "image/bmp": ".bmp",
            "image/heic": ".heic",
            "image/heif": ".heif",
            "image/avif": ".avif",
            "image/tiff": ".tiff",
            "image/svg+xml": ".svg",
            "audio/mpeg": ".mp3",
            "audio/mp4": ".m4a",
            "audio/aac": ".aac",
            "audio/ogg": ".ogg",
            "audio/wav": ".wav",
            "audio/x-wav": ".wav",
            "audio/flac": ".flac",
            "audio/amr": ".amr",
            "video/mp4": ".mp4",
            "video/quicktime": ".mov",
            "video/webm": ".webm",
            "video/x-matroska": ".mkv",
            "application/pdf": ".pdf",
        }
        base = ct.split(";")[0].strip().lower()
        return mapping.get(base, "")
