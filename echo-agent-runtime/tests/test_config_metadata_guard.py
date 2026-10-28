# tests/test_config_metadata_guard.py
"""Guard tests: every Config field must declare valid metadata.

This is the anti-regression mechanism. A new field with no status, or an
effective field whose ref points at a non-existent file, fails CI.
"""
from __future__ import annotations

from pathlib import Path

from echo_agent.config.metadata import iter_fields
from echo_agent.config.schema import Config

_ECHO_ROOT = Path(__file__).resolve().parent.parent / "echo_agent"


def test_every_field_declares_status():
    missing = [f.path for f in iter_fields(Config)
               if f.extra.get("status") not in ("effective", "dead")]
    assert not missing, f"字段缺少 status 元数据: {missing}"


def test_effective_fields_have_desc_and_ref():
    bad = []
    for f in iter_fields(Config):
        if f.extra.get("status") != "effective":
            continue
        if not (f.extra.get("desc_zh") and f.extra.get("desc_en") and f.extra.get("ref")):
            bad.append(f.path)
    assert not bad, f"effective 字段缺少 desc_zh/desc_en/ref: {bad}"


def test_dead_fields_have_reason_and_disposition():
    bad = []
    for f in iter_fields(Config):
        if f.extra.get("status") != "dead":
            continue
        if not f.extra.get("reason") or f.extra.get("disposition") not in ("fix", "remove", "keep"):
            bad.append(f.path)
    assert not bad, f"dead 字段缺少 reason 或合法 disposition: {bad}"


def test_effective_ref_files_exist():
    bad = []
    for f in iter_fields(Config):
        if f.extra.get("status") != "effective":
            continue
        ref = f.extra.get("ref", "")
        rel_path = ref.split(":")[0]
        if not (_ECHO_ROOT / rel_path).exists():
            bad.append((f.path, ref))
    assert not bad, f"effective 字段 ref 指向不存在的文件: {bad}"
