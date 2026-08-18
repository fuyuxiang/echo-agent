"""spill 产物清扫:按保留天数与总体积双限回收。

harness 自承 spill 文件靠外部清理——它是会话式 CLI,可以耍这个赖。
echo-agent 是 7x24 常驻服务,不做清扫就是在 workspace 里种定时炸弹。
"""

from __future__ import annotations

import asyncio
import time
from pathlib import Path

from loguru import logger

from echo_agent.spill.layout import SESSION_DIR_GLOB, is_artifact, is_session_dir


def sweep(root: Path, retention_days: int, max_total_mb: int) -> int:
    """先删过期,再按最旧优先删到总体积达标。返回删除的文件数。

    只认自己写出的形状:``session-<12hex>/<8hex>-<tool>.txt``。清扫器拿到的是
    一个来自配置的目录,配错了(``spillDir: "."``)就会指向源码树——遍历全部
    子孙节点、按 mtime 删除,会把仓库里的旧文件当产物删掉。宁可漏删不认识的
    文件,不可误删不属于自己的文件。
    """
    root = Path(root)
    if not root.is_dir():
        return 0

    entries: list[tuple[float, int, Path]] = []
    for session_dir in root.glob(SESSION_DIR_GLOB):
        if not is_session_dir(session_dir) or not session_dir.is_dir():
            continue
        # 只看直接子项:产物是平铺的,不下钻就不会碰到误配目录下的深层内容。
        for p in session_dir.iterdir():
            if not is_artifact(p) or not p.is_file():
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
    """只剪空的会话目录。rglob 全量剪枝会删掉误配目录下无关的空目录。"""
    for d in root.glob(SESSION_DIR_GLOB):
        if not is_session_dir(d) or not d.is_dir():
            continue
        try:
            d.rmdir()
        except OSError:
            pass


async def sweep_forever(root: Path, retention_days: int, max_total_mb: int,
                        interval_hours: int) -> None:
    """启动时先扫一次,之后按间隔循环。

    ``sweep`` 是同步的递归遍历 + stat + unlink。放在事件循环里跑,产物多时会
    冻住通道轮询、健康检查和其他会话,故整个扫描下放到工作线程。
    """
    while True:
        try:
            n = await asyncio.to_thread(sweep, root, retention_days, max_total_mb)
            if n:
                logger.info("spill 清扫删除 {} 个产物", n)
        except asyncio.CancelledError:
            raise
        except Exception as e:
            logger.warning("spill 清扫异常: {}", e)
        await asyncio.sleep(max(1, interval_hours) * 3600)
