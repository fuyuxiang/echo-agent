"""Tests for echo_agent/config/loader.py."""

from __future__ import annotations

from pathlib import Path

import yaml

from echo_agent.config.loader import (
    _deep_merge,
    _env_overrides,
    _find_config_file_in,
    _load_yaml_file,
    load_config,
    migrate_heartbeat_config,
    resolve_config_file,
    save_config,
)


# ---------------------------------------------------------------------------
# _find_config_file_in
# ---------------------------------------------------------------------------

class TestFindConfigFileIn:
    def test_finds_yaml(self, tmp_path: Path):
        (tmp_path / "echo-agent.yaml").write_text("key: val\n")
        assert _find_config_file_in(tmp_path) == tmp_path / "echo-agent.yaml"

    def test_finds_yml(self, tmp_path: Path):
        (tmp_path / "echo-agent.yml").write_text("key: val\n")
        assert _find_config_file_in(tmp_path) == tmp_path / "echo-agent.yml"

    def test_prefers_yaml_over_yml(self, tmp_path: Path):
        (tmp_path / "echo-agent.yaml").write_text("a: 1\n")
        (tmp_path / "echo-agent.yml").write_text("b: 2\n")
        result = _find_config_file_in(tmp_path)
        assert result.name == "echo-agent.yaml"

    def test_finds_config_yaml(self, tmp_path: Path):
        (tmp_path / "config.yaml").write_text("x: 1\n")
        assert _find_config_file_in(tmp_path) == tmp_path / "config.yaml"

    def test_returns_none_when_missing(self, tmp_path: Path):
        assert _find_config_file_in(tmp_path) is None


# ---------------------------------------------------------------------------
# _load_yaml_file
# ---------------------------------------------------------------------------

class TestLoadYamlFile:
    def test_valid_yaml(self, tmp_path: Path):
        p = tmp_path / "test.yaml"
        p.write_text("foo: bar\nnested:\n  a: 1\n")
        data = _load_yaml_file(p)
        assert data["foo"] == "bar"
        assert data["nested"]["a"] == 1

    def test_empty_file(self, tmp_path: Path):
        p = tmp_path / "empty.yaml"
        p.write_text("")
        assert _load_yaml_file(p) == {}

    def test_missing_file(self, tmp_path: Path):
        assert _load_yaml_file(tmp_path / "nope.yaml") == {}

    def test_none_path(self):
        assert _load_yaml_file(None) == {}


# ---------------------------------------------------------------------------
# _deep_merge
# ---------------------------------------------------------------------------

class TestDeepMerge:
    def test_nested_dicts(self):
        base = {"a": {"x": 1, "y": 2}, "b": 10}
        override = {"a": {"y": 99, "z": 3}}
        result = _deep_merge(base, override)
        assert result == {"a": {"x": 1, "y": 99, "z": 3}, "b": 10}

    def test_override_scalar(self):
        base = {"a": 1, "b": 2}
        override = {"a": 100}
        result = _deep_merge(base, override)
        assert result["a"] == 100
        assert result["b"] == 2

    def test_override_dict_with_scalar(self):
        base = {"a": {"nested": True}}
        override = {"a": "flat"}
        result = _deep_merge(base, override)
        assert result["a"] == "flat"

    def test_empty_override(self):
        base = {"a": 1}
        assert _deep_merge(base, {}) == {"a": 1}

    def test_empty_base(self):
        override = {"a": 1}
        assert _deep_merge({}, override) == {"a": 1}


# ---------------------------------------------------------------------------
# _env_overrides
# ---------------------------------------------------------------------------

class TestEnvOverrides:
    def test_simple_prefix(self, monkeypatch):
        monkeypatch.setenv("ECHO_AGENT_MODEL", "gpt-4")
        result = _env_overrides()
        assert result["model"] == "gpt-4"

    def test_nested_with_double_underscore(self, monkeypatch):
        monkeypatch.setenv("ECHO_AGENT_LLM__PROVIDER", "openai")
        result = _env_overrides()
        assert result["llm"]["provider"] == "openai"

    def test_ignores_unrelated_vars(self, monkeypatch):
        monkeypatch.setenv("OTHER_VAR", "nope")
        monkeypatch.delenv("ECHO_AGENT_MODEL", raising=False)
        monkeypatch.delenv("ECHO_AGENT_LLM__PROVIDER", raising=False)
        result = _env_overrides()
        assert "other_var" not in result


# ---------------------------------------------------------------------------
# save_config
# ---------------------------------------------------------------------------

class TestSaveConfig:
    def test_writes_yaml(self, tmp_path: Path):
        target = tmp_path / "out.yaml"
        data = {"model": "gpt-4", "nested": {"key": "val"}}
        result = save_config(data, path=target)
        assert result == target
        loaded = yaml.safe_load(target.read_text())
        assert loaded["model"] == "gpt-4"
        assert loaded["nested"]["key"] == "val"

    def test_creates_parent_dirs(self, tmp_path: Path):
        target = tmp_path / "sub" / "dir" / "config.yaml"
        save_config({"a": 1}, path=target)
        assert target.exists()


# ---------------------------------------------------------------------------
# resolve_config_file
# ---------------------------------------------------------------------------

class TestResolveConfigFile:
    def test_explicit_path(self, tmp_path: Path):
        cfg = tmp_path / "my.yaml"
        cfg.write_text("x: 1\n")
        result = resolve_config_file(config_path=cfg)
        assert result == cfg.resolve()

    def test_explicit_path_nonexistent(self, tmp_path: Path):
        cfg = tmp_path / "missing.yaml"
        result = resolve_config_file(config_path=cfg)
        assert result == cfg

    def test_search_dir(self, tmp_path: Path):
        (tmp_path / "echo-agent.yaml").write_text("a: 1\n")
        result = resolve_config_file(search_dir=tmp_path)
        assert result is not None
        assert result.name == "echo-agent.yaml"


# ---------------------------------------------------------------------------
# load_config — end-to-end profile cognitive defaults
# ---------------------------------------------------------------------------

class TestLoadConfigProfileDefaults:
    """Lock the zero-config default: a CLI run with no user YAML must resolve
    to the lean personal_cli cognitive defaults (planning off). Regression
    guard for the bug where profile_defaults ran before pydantic injected the
    default profile, so planning stayed on for the most common path."""

    def test_zero_config_defaults_to_lean_cli(self, tmp_path: Path):
        # Point at a nonexistent file inside an empty dir so only the packaged
        # default.yaml is loaded — no user/home config bleeds in.
        cfg = load_config(config_path=tmp_path / "absent.yaml")
        assert cfg.security.profile == "personal_cli"
        assert cfg.planning.enabled is False
        assert cfg.memory.retrieval_on_miss == "degrade"

    def test_explicit_daemon_keeps_planning(self, tmp_path: Path):
        cfg_file = tmp_path / "echo-agent.yaml"
        cfg_file.write_text("security:\n  profile: daemon\n")
        cfg = load_config(config_path=cfg_file)
        assert cfg.security.profile == "daemon"
        assert cfg.planning.enabled is True
        assert cfg.memory.retrieval_on_miss == "sync"

    def test_explicit_user_planning_overrides_profile_default(self, tmp_path: Path):
        # personal_cli would turn planning off, but an explicit user value wins.
        cfg_file = tmp_path / "echo-agent.yaml"
        cfg_file.write_text("planning:\n  enabled: true\n")
        cfg = load_config(config_path=cfg_file)
        assert cfg.security.profile == "personal_cli"
        assert cfg.planning.enabled is True


def test_migrate_every_maps_to_every_tool():
    data = {"agent": {"heartbeat": {"on_uneditable": "every"}}}
    out = migrate_heartbeat_config(data)
    assert out["agent"]["heartbeat"]["verbosity"] == "every_tool"
    assert "on_uneditable" not in out["agent"]["heartbeat"]


def test_load_config_does_not_emit_info_log(tmp_path):
    """The per-command 'Loading config' line is now debug, so an INFO-level
    sink (loguru's default is INFO+) must not capture it — otherwise it leaks
    into clean command output like `status`/`cost`."""
    from loguru import logger

    cfg_file = tmp_path / "echo-agent.yaml"
    cfg_file.write_text("planning:\n  enabled: true\n")

    captured: list[str] = []
    sink_id = logger.add(lambda msg: captured.append(str(msg)), level="INFO")
    try:
        load_config(config_path=cfg_file)
    finally:
        logger.remove(sink_id)

    assert not any("Loading config" in line for line in captured)


def test_load_config_debug_log_still_available(tmp_path):
    """At DEBUG level the load line is still emitted (diagnostics preserved)."""
    from loguru import logger

    cfg_file = tmp_path / "echo-agent.yaml"
    cfg_file.write_text("planning:\n  enabled: true\n")

    captured: list[str] = []
    sink_id = logger.add(lambda msg: captured.append(str(msg)), level="DEBUG")
    try:
        load_config(config_path=cfg_file)
    finally:
        logger.remove(sink_id)

    assert any("Loading config" in line for line in captured)


def test_migrate_first_only_drops_without_setting_verbosity():
    data = {"agent": {"heartbeat": {"on_uneditable": "first_only"}}}
    out = migrate_heartbeat_config(data)
    assert "on_uneditable" not in out["agent"]["heartbeat"]
    # first_only/off carry no clean mapping -> leave verbosity to schema default
    assert "verbosity" not in out["agent"]["heartbeat"]


def test_migrate_renames_interval_sec():
    data = {"agent": {"heartbeat": {"interval_sec": 90}}}
    out = migrate_heartbeat_config(data)
    assert out["agent"]["heartbeat"]["min_interval_sec"] == 90
    assert "interval_sec" not in out["agent"]["heartbeat"]


def test_migrate_noop_when_no_heartbeat():
    data = {"agent": {}}
    assert migrate_heartbeat_config(data) == {"agent": {}}


def test_migrate_every_does_not_override_explicit_verbosity():
    # User already set verbosity explicitly -> legacy on_uneditable must not clobber it.
    data = {"agent": {"heartbeat": {"on_uneditable": "every", "verbosity": "silent"}}}
    out = migrate_heartbeat_config(data)
    assert out["agent"]["heartbeat"]["verbosity"] == "silent"
    assert "on_uneditable" not in out["agent"]["heartbeat"]


def test_migrate_drops_interval_sec_when_min_interval_present():
    # Both present -> keep min_interval_sec, drop legacy interval_sec.
    data = {"agent": {"heartbeat": {"interval_sec": 90, "min_interval_sec": 45}}}
    out = migrate_heartbeat_config(data)
    assert out["agent"]["heartbeat"]["min_interval_sec"] == 45
    assert "interval_sec" not in out["agent"]["heartbeat"]

