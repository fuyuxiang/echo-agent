"""清扫:天数与总体积双限。

单靠天数不能约束磁盘(一天内跑几千次大输出就能到 GB),单靠体积不能约束陈旧。
"""

from __future__ import annotations

import os
import time

from echo_agent.spill.sweeper import sweep


def _artifact(root, name, size=1024, age_days=0.0):
    p = root / "session-x" / name
    p.parent.mkdir(parents=True, exist_ok=True)
    p.write_text("z" * size, encoding="utf-8")
    if age_days:
        old = time.time() - age_days * 86400
        os.utime(p, (old, old))
    return p


def test_deletes_expired_only(tmp_path):
    fresh = _artifact(tmp_path, "fresh.txt", age_days=1)
    stale = _artifact(tmp_path, "stale.txt", age_days=30)
    assert sweep(tmp_path, retention_days=7, max_total_mb=512) == 1
    assert fresh.exists()
    assert not stale.exists()


def test_keeps_everything_when_under_both_limits(tmp_path):
    a = _artifact(tmp_path, "a.txt", age_days=1)
    assert sweep(tmp_path, retention_days=7, max_total_mb=512) == 0
    assert a.exists()


def test_size_cap_deletes_oldest_first(tmp_path):
    old = _artifact(tmp_path, "old.txt", size=600_000, age_days=2)
    new = _artifact(tmp_path, "new.txt", size=600_000, age_days=1)
    sweep(tmp_path, retention_days=7, max_total_mb=1)
    assert not old.exists()
    assert new.exists()


def test_missing_root_is_noop(tmp_path):
    assert sweep(tmp_path / "nope", retention_days=7, max_total_mb=512) == 0
