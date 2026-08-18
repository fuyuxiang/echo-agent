"""spill 配置项：默认值、camelCase 别名、以及装配后端到端生效。"""

from __future__ import annotations

import pytest
from pydantic import ValidationError

from echo_agent.config.schema import Config


def test_defaults():
    c = Config()
    assert c.spill.enabled is True
    assert c.spill.max_inline_chars == 6000
    assert c.spill.retention_days == 7
    assert c.spill.max_total_mb == 512
    assert c.spill.sweep_interval_hours == 6
    assert c.storage.spill_dir == "data/spill"


def test_camel_case_alias_matches_yaml_convention():
    # _Base 的 alias_generator=to_camel(schema.py:12):用户在 YAML 里写驼峰
    c = Config.model_validate({"spill": {"maxInlineChars": 1234, "enabled": False}})
    assert c.spill.max_inline_chars == 1234
    assert c.spill.enabled is False


# ── 数值边界 ─────────────────────────────────────────────────────────────────
# 这四个字段每一个配成 0 或负数都有具体的破坏性后果,不是"不太合理"而已。

@pytest.mark.parametrize("value", [0, -1, -7])
def test_retention_days_rejects_non_positive(value):
    """负保留期会把 cutoff 推到未来,一次清扫删光全部产物——而取回路径已经
    发给模型了。"""
    with pytest.raises(ValidationError):
        Config.model_validate({"spill": {"retentionDays": value}})


@pytest.mark.parametrize("value", [0, -1])
def test_max_total_mb_rejects_non_positive(value):
    """预算为 0 时体积回收会删到一个不剩。"""
    with pytest.raises(ValidationError):
        Config.model_validate({"spill": {"maxTotalMb": value}})


@pytest.mark.parametrize("value", [0, -1, 1, 100])
def test_max_inline_chars_rejects_too_small(value):
    """cap 小于取回提示本身时,compose 一路返回 None:文件白写(孤儿)、模型
    照样收到超长原文。配小一点本以为更省 token,实际两头都亏。"""
    with pytest.raises(ValidationError):
        Config.model_validate({"spill": {"maxInlineChars": value}})


@pytest.mark.parametrize("value", [0, -3])
def test_sweep_interval_rejects_non_positive(value):
    """此前会被 max(1, ...) 静默改成 1 小时,配错的人得不到任何反馈。"""
    with pytest.raises(ValidationError):
        Config.model_validate({"spill": {"sweepIntervalHours": value}})


def test_valid_values_still_accepted():
    c = Config.model_validate({"spill": {
        "maxInlineChars": 500, "retentionDays": 1,
        "maxTotalMb": 1, "sweepIntervalHours": 1,
    }})
    assert c.spill.max_inline_chars == 500


# ── spillDir 必须是工作区内的专用子目录 ──────────────────────────────────────

@pytest.mark.parametrize("bad", [
    ".",                    # 工作区本身:清扫器指向源码树
    "./",
    "",
    "   ",
    "..",                   # 逃出工作区
    "../shared",
    "data/../..",
    "/var/lib/echo",        # 绝对路径:POSIX
    "C:\\data\\spill",      # 绝对路径:Windows
])
def test_spill_dir_rejects_dangerous_values(bad):
    """spillDir 决定清扫器在哪删文件,也决定 spill 闸门屏蔽哪片路径。

    配成 "." 时:清扫器扫源码树(虽只删自己认得的形状,仍是配置错误),而闸门
    会把整个工作区判为 spill 区域,read_file/search_files 全部静默失效。
    """
    with pytest.raises(ValidationError):
        Config.model_validate({"storage": {"spillDir": bad}})


@pytest.mark.parametrize("good", ["data/spill", "var/spill", "data/spill/v2", "spill"])
def test_spill_dir_accepts_dedicated_subdirs(good):
    c = Config.model_validate({"storage": {"spillDir": good}})
    assert c.storage.spill_dir == good
