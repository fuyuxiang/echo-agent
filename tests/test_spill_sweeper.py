"""清扫:天数与总体积双限。

单靠天数不能约束磁盘(一天内跑几千次大输出就能到 GB),单靠体积不能约束陈旧。
"""

from __future__ import annotations

import os
import time

from echo_agent.spill.sweeper import sweep


def _aged(p, age_days: float):
    if age_days:
        old = time.time() - age_days * 86400
        os.utime(p, (old, old))
    return p


def _artifact(root, name, size=1024, age_days=0.0, session="session-abc123def456"):
    """按 store 真实写出的形状造产物:session-<12hex>/<8hex>-<tool>.txt。

    清扫器只删这个形状,所以测试必须用同一个形状——否则测的是"清扫器不删
    不认识的文件",而不是"清扫器会删过期产物"。
    """
    p = root / session / name
    p.parent.mkdir(parents=True, exist_ok=True)
    p.write_text("z" * size, encoding="utf-8")
    return _aged(p, age_days)


def _foreign(root, relpath, size=1024, age_days=0.0):
    """造一个不属于 spill 的旧文件,用于验证误配目录时不会被删。"""
    p = root / relpath
    p.parent.mkdir(parents=True, exist_ok=True)
    p.write_text("z" * size, encoding="utf-8")
    return _aged(p, age_days)


def test_deletes_expired_only(tmp_path):
    fresh = _artifact(tmp_path, "aaaaaaaa-exec.txt", age_days=1)
    stale = _artifact(tmp_path, "bbbbbbbb-exec.txt", age_days=30)
    assert sweep(tmp_path, retention_days=7, max_total_mb=512) == 1
    assert fresh.exists()
    assert not stale.exists()


def test_keeps_everything_when_under_both_limits(tmp_path):
    a = _artifact(tmp_path, "aaaaaaaa-exec.txt", age_days=1)
    assert sweep(tmp_path, retention_days=7, max_total_mb=512) == 0
    assert a.exists()


def test_size_cap_deletes_oldest_first(tmp_path):
    old = _artifact(tmp_path, "aaaaaaaa-exec.txt", size=600_000, age_days=2)
    new = _artifact(tmp_path, "bbbbbbbb-exec.txt", size=600_000, age_days=1)
    sweep(tmp_path, retention_days=7, max_total_mb=1)
    assert not old.exists()
    assert new.exists()


def test_missing_root_is_noop(tmp_path):
    assert sweep(tmp_path / "nope", retention_days=7, max_total_mb=512) == 0


def test_never_touches_files_outside_session_dirs(tmp_path):
    """spillDir 误配到工作区时,不得删除无关文件。

    这是本清扫器最危险的失效模式:它拿到的是一个来自配置的目录,按 mtime 删
    文件。配成 "." 就指向源码树,而那里到处是超过保留期的旧文件。
    """
    src = _foreign(tmp_path, "echo_agent/agent/loop.py", age_days=400)
    db = _foreign(tmp_path, "data/sessions/history.db", age_days=400)
    top = _foreign(tmp_path, "README.md", age_days=400)
    assert sweep(tmp_path, retention_days=7, max_total_mb=512) == 0
    assert src.exists()
    assert db.exists()
    assert top.exists()


def test_ignores_foreign_files_inside_a_session_dir(tmp_path):
    """会话目录里名字不合形状的文件也不删——只有自己写的才算自己的。"""
    stale = _artifact(tmp_path, "aaaaaaaa-exec.txt", age_days=30)
    foreign = _foreign(tmp_path, "session-abc123def456/notes.txt", age_days=400)
    assert sweep(tmp_path, retention_days=7, max_total_mb=512) == 1
    assert not stale.exists()
    assert foreign.exists()


def test_size_cap_ignores_foreign_bulk(tmp_path):
    """总体积只按自己的产物算,否则无关大文件会把真产物挤掉。"""
    _foreign(tmp_path, "big.bin", size=2_000_000, age_days=1)
    art = _artifact(tmp_path, "aaaaaaaa-exec.txt", size=1024, age_days=1)
    assert sweep(tmp_path, retention_days=7, max_total_mb=1) == 0
    assert art.exists()


def test_prunes_only_empty_session_dirs(tmp_path):
    empty_foreign = tmp_path / "logs"
    empty_foreign.mkdir(parents=True)
    session = _artifact(tmp_path, "aaaaaaaa-exec.txt", age_days=30).parent
    sweep(tmp_path, retention_days=7, max_total_mb=512)
    assert not session.exists()
    assert empty_foreign.exists()
