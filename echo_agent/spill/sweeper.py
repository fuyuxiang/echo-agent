"""spill 产物清扫:按保留天数与总体积双限回收。

harness 自承 spill 文件靠外部清理——它是会话式 CLI,可以耍这个赖。
echo-agent 是 7x24 常驻服务,不做清扫就是在 workspace 里种定时炸弹。
"""

from __future__ import annotations

import asyncio
import time
from pathlib import Path

from loguru import logger


def sweep(root: Path, retention_days: int, max_total_mb: int) -> int:
    """先删过期,再按最旧优先删到总体积达标。返回删除的文件数。"""
    root = Path(root)
    if not root.is_dir():
        return 0

    entries: list[tuple[float, int, Path]] = []
    for p in root.rglob("*"):
        if not p.is_file():
            continue
        try:
            st = p.stat()
        except OSError:
            continue
        entries.append((st.st_mtime, st.st_size, p))

    deleted = 0
    cutoff = time.time() - retention_days * 86400
    survivors: list[tuple[float, int, Path]] = []
    for mtime, size, p in entries:
        if mtime < cutoff:
            if _unlink(p):
                deleted += 1
        else:
            survivors.append((mtime, size, p))

    budget = max_total_mb * 1024 * 1024
    total = sum(size for _, size, _ in survivors)
    for mtime, size, p in sorted(survivors):
        if total <= budget:
            break
        if _unlink(p):
            deleted += 1
            total -= size

    _prune_empty_dirs(root)
    return deleted


def _unlink(p: Path) -> bool:
    try:
        p.unlink()
        return True
    except OSError as e:
        logger.debug("spill 清扫删除失败 {}: {}", p, e)
        return False


def _prune_empty_dirs(root: Path) -> None:
    for d in sorted((d for d in root.rglob("*") if d.is_dir()), reverse=True):
        try:
            d.rmdir()
        except OSError:
            pass


async def sweep_forever(root: Path, retention_days: int, max_total_mb: int,
                        interval_hours: int) -> None:
    """启动时先扫一次,之后按间隔循环。"""
    while True:
        try:
            n = sweep(root, retention_days, max_total_mb)
            if n:
                logger.info("spill 清扫删除 {} 个产物", n)
        except Exception as e:
            logger.warning("spill 清扫异常: {}", e)
        await asyncio.sleep(max(1, interval_hours) * 3600)
