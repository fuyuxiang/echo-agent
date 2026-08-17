"""spill 配置项：默认值、camelCase 别名、以及装配后端到端生效。"""

from __future__ import annotations

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
