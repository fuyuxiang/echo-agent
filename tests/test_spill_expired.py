"""清扫会删掉历史会话仍引用的路径。会话恢复后模型照着 notice 去 read_file,
文件已不存在——裸文件系统错误模型读不懂,可能反复重试或胡乱推断。

判据必须用解析后的绝对路径,不能按字符串前缀:否则
data/spill/../../../etc/passwd 会被误判为 spill 路径,让越权读取失败伪装成
"产物已过期",掩盖真实原因。
"""

from __future__ import annotations

from echo_agent.spill.expired import expired_notice


def test_missing_spill_file_gets_semantic_notice(tmp_path):
    spill_root = tmp_path / "spill"
    spill_root.mkdir()
    gone = spill_root / "session-abc" / "dead-exec.txt"
    msg = expired_notice(str(gone), str(tmp_path), spill_root)
    assert msg is not None
    assert "保留期" in msg


def test_existing_spill_file_gets_no_notice(tmp_path):
    spill_root = tmp_path / "spill"
    spill_root.mkdir()
    live = spill_root / "live.txt"
    live.write_text("here", encoding="utf-8")
    assert expired_notice(str(live), str(tmp_path), spill_root) is None


def test_non_spill_path_gets_no_notice(tmp_path):
    spill_root = tmp_path / "spill"
    spill_root.mkdir()
    assert expired_notice(str(tmp_path / "other.txt"), str(tmp_path), spill_root) is None


def test_traversal_out_of_spill_root_is_not_claimed(tmp_path):
    spill_root = tmp_path / "spill"
    spill_root.mkdir()
    evil = str(spill_root / ".." / ".." / "etc" / "passwd")
    assert expired_notice(evil, str(tmp_path), spill_root) is None


def test_no_spill_root_configured_is_none(tmp_path):
    assert expired_notice(str(tmp_path / "x.txt"), str(tmp_path), None) is None
