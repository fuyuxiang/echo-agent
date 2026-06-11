"""Tests for plugin manifest parsing."""

import pytest

from echo_agent.plugins.manifest import (
    PluginManifest,
    PluginRecord,
    check_required_env,
    parse_manifest,
    parse_manifest_from_dict,
)


@pytest.fixture
def sample_manifest_path(tmp_path):
    content = """
name: my-plugin
version: "1.2.3"
description: "A test plugin"
author: "Test Author"
requires_echo_agent: ">=0.1.0"
requires_env:
  - MY_API_KEY
provides:
  tools:
    - my_tool
  hooks:
    - pre_tool_call
kind: integration
config_key: my_plugin
depends_on:
  - other-plugin
"""
    p = tmp_path / "plugin.yaml"
    p.write_text(content)
    return p


def test_parse_manifest(sample_manifest_path):
    m = parse_manifest(sample_manifest_path)
    assert m.name == "my-plugin"
    assert m.version == "1.2.3"
    assert m.description == "A test plugin"
    assert m.author == "Test Author"
    assert m.requires_echo_agent == ">=0.1.0"
    assert m.requires_env == ["MY_API_KEY"]
    assert m.provides.tools == ["my_tool"]
    assert m.provides.hooks == ["pre_tool_call"]
    assert m.kind == "integration"
    assert m.config_key == "my_plugin"
    assert m.depends_on == ["other-plugin"]


def test_parse_manifest_minimal(tmp_path):
    p = tmp_path / "plugin.yaml"
    p.write_text("name: minimal\n")
    m = parse_manifest(p)
    assert m.name == "minimal"
    assert m.version == "0.0.1"
    assert m.provides.tools == []
    assert m.kind == "integration"


def test_parse_manifest_from_dict():
    m = parse_manifest_from_dict({"name": "dict-plugin", "version": "2.0.0"})
    assert m.name == "dict-plugin"
    assert m.version == "2.0.0"


def test_check_required_env_missing(monkeypatch):
    monkeypatch.delenv("NONEXISTENT_VAR_XYZ", raising=False)
    m = PluginManifest(name="test", requires_env=["NONEXISTENT_VAR_XYZ"])
    missing = check_required_env(m)
    assert "NONEXISTENT_VAR_XYZ" in missing


def test_check_required_env_present(monkeypatch):
    monkeypatch.setenv("TEST_PLUGIN_KEY", "value")
    m = PluginManifest(name="test", requires_env=["TEST_PLUGIN_KEY"])
    missing = check_required_env(m)
    assert missing == []


def test_plugin_record_defaults():
    m = PluginManifest(name="rec-test")
    r = PluginRecord(manifest=m, source="entrypoint")
    assert r.status == "discovered"
    assert r.error == ""
    assert r.tools_registered == []
    assert r.hooks_registered == []


# ── Error classes ──────────────────────────────────────────────────────────

def test_plugin_error_format():
    from echo_agent.plugins.errors import PluginError
    e = PluginError("my-plugin", "something went wrong")
    assert str(e) == "[my-plugin] something went wrong"
    assert e.plugin_name == "my-plugin"


def test_plugin_load_error():
    from echo_agent.plugins.errors import PluginLoadError
    e = PluginLoadError("bad-plugin", "import failed")
    assert "bad-plugin" in str(e)
    assert "import failed" in str(e)
    assert isinstance(e, Exception)


def test_plugin_activation_error():
    from echo_agent.plugins.errors import PluginActivationError
    e = PluginActivationError("crash-plugin", "activate() raised")
    assert "crash-plugin" in str(e)


def test_plugin_manifest_error():
    from echo_agent.plugins.errors import PluginManifestError
    e = PluginManifestError("invalid-plugin", "missing name field")
    assert "invalid-plugin" in str(e)


def test_error_inheritance():
    from echo_agent.plugins.errors import (
        PluginError, PluginLoadError, PluginActivationError, PluginManifestError,
    )
    assert issubclass(PluginLoadError, PluginError)
    assert issubclass(PluginActivationError, PluginError)
    assert issubclass(PluginManifestError, PluginError)
    assert issubclass(PluginError, Exception)
