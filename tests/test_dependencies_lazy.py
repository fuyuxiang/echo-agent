"""Tests for echo_agent.dependencies — lazy_deps, cli, skill_require.

Covers spec safety validation, version checks, the public API, the install
engine (with subprocess fully mocked), the deps CLI dispatch, and the
skill_require convenience helpers.
"""

from __future__ import annotations

from unittest.mock import MagicMock, patch

import pytest

from echo_agent.dependencies import lazy_deps as ld


# ── _spec_is_safe ─────────────────────────────────────────────────────────────


@pytest.mark.parametrize(
    "spec",
    [
        "openpyxl",
        "openpyxl>=3.1",
        "duckduckgo_search>=7.0",
        "google-generativeai>=0.8",
        "uvicorn[standard]>=0.30",
        "pkg>=1.0,<2.0",
    ],
)
def test_spec_is_safe_accepts_valid(spec):
    assert ld._spec_is_safe(spec) is True


@pytest.mark.parametrize(
    "spec",
    [
        "",
        "x" * 201,                       # too long
        "pkg; rm -rf /",                 # shell metachar
        "pkg | cat",
        "pkg && evil",
        "pkg`whoami`",
        "pkg$VAR",
        "pkg\nmore",
        "pkg\\path",
        "-rrequirements.txt",            # starts with dash
        "/abs/path",                     # starts with slash
        "./rel/path",                    # starts with dot
        "git+https://example.com/x",     # URL
        "pkg @ file:///tmp/x",           # @ direct ref
        "https://example.com/x.whl",
    ],
)
def test_spec_is_safe_rejects_dangerous(spec):
    assert ld._spec_is_safe(spec) is False


# ── name / specifier extraction ──────────────────────────────────────────────


def test_pkg_name_from_spec():
    assert ld._pkg_name_from_spec("openpyxl>=3.1") == "openpyxl"
    assert ld._pkg_name_from_spec("google-generativeai>=0.8") == "google-generativeai"
    assert ld._pkg_name_from_spec("plain") == "plain"


def test_specifier_from_spec():
    assert ld._specifier_from_spec("openpyxl>=3.1") == ">=3.1"
    assert ld._specifier_from_spec("openpyxl") == ""
    assert ld._specifier_from_spec("uvicorn[standard]>=0.30") == ">=0.30"


# ── _is_satisfied / _is_present ──────────────────────────────────────────────


def test_is_satisfied_missing_package():
    with patch("importlib.metadata.version", side_effect=__import__("importlib.metadata", fromlist=["PackageNotFoundError"]).PackageNotFoundError):
        assert ld._is_satisfied("definitely-not-installed-xyz>=1.0") is False


def test_is_satisfied_no_specifier_present():
    with patch("importlib.metadata.version", return_value="1.2.3"):
        assert ld._is_satisfied("somepkg") is True


def test_is_satisfied_version_in_range():
    with patch("importlib.metadata.version", return_value="3.5.0"):
        assert ld._is_satisfied("openpyxl>=3.1") is True


def test_is_satisfied_version_out_of_range():
    with patch("importlib.metadata.version", return_value="2.0.0"):
        assert ld._is_satisfied("openpyxl>=3.1") is False


def test_is_satisfied_unparseable_version_is_lenient():
    # Non-PEP440 installed version → treated as satisfied rather than blocking.
    with patch("importlib.metadata.version", return_value="weird-version"):
        assert ld._is_satisfied("openpyxl>=3.1") is True


def test_is_present_true_and_false():
    with patch("importlib.metadata.version", return_value="1.0"):
        assert ld._is_present("anything") is True
    from importlib.metadata import PackageNotFoundError
    with patch("importlib.metadata.version", side_effect=PackageNotFoundError):
        assert ld._is_present("nope") is False


# ── feature_* public API ─────────────────────────────────────────────────────


def test_feature_specs_known_and_unknown():
    assert ld.feature_specs("skill.excel-author") == ("openpyxl>=3.1",)
    with pytest.raises(KeyError):
        ld.feature_specs("skill.does-not-exist")


def test_feature_install_command():
    cmd = ld.feature_install_command("skill.excel-author")
    assert cmd is not None and "uv pip install" in cmd and "openpyxl" in cmd
    # empty-spec feature and unknown feature both yield None
    assert ld.feature_install_command("skill.calculator") is None
    assert ld.feature_install_command("unknown") is None


def test_is_available_empty_specs_always_true():
    assert ld.is_available("skill.calculator") is True


def test_is_available_unknown_feature_false():
    assert ld.is_available("skill.unknown-xyz") is False


def test_is_available_reflects_missing(monkeypatch):
    monkeypatch.setattr(ld, "_is_satisfied", lambda spec: False)
    assert ld.is_available("skill.excel-author") is False
    monkeypatch.setattr(ld, "_is_satisfied", lambda spec: True)
    assert ld.is_available("skill.excel-author") is True


def test_feature_missing(monkeypatch):
    monkeypatch.setattr(ld, "_is_satisfied", lambda spec: False)
    assert ld.feature_missing("skill.excel-author") == ("openpyxl>=3.1",)


# ── FeatureUnavailable formatting ────────────────────────────────────────────


def test_feature_unavailable_message_with_missing():
    err = ld.FeatureUnavailable("skill.x", ("pkg>=1.0",), "boom")
    text = str(err)
    assert "skill.x" in text and "boom" in text and "pkg>=1.0" in text
    assert err.feature == "skill.x" and err.reason == "boom"


def test_feature_unavailable_message_without_missing():
    err = ld.FeatureUnavailable("skill.x", (), "boom")
    assert "boom" in str(err)


# ── _allow_lazy_installs ─────────────────────────────────────────────────────


def test_allow_lazy_installs_env_disable(monkeypatch):
    monkeypatch.setenv("ECHO_AGENT_DISABLE_LAZY_INSTALLS", "1")
    assert ld._allow_lazy_installs() is False


def test_allow_lazy_installs_config_false(monkeypatch):
    monkeypatch.delenv("ECHO_AGENT_DISABLE_LAZY_INSTALLS", raising=False)
    fake_cfg = MagicMock()
    fake_cfg.skills.allow_lazy_installs = False
    with patch("echo_agent.config.loader.load_config", return_value=fake_cfg):
        assert ld._allow_lazy_installs() is False


def test_allow_lazy_installs_default_true(monkeypatch):
    monkeypatch.delenv("ECHO_AGENT_DISABLE_LAZY_INSTALLS", raising=False)
    with patch("echo_agent.config.loader.load_config", side_effect=RuntimeError):
        assert ld._allow_lazy_installs() is True


# ── ensure() decision paths ──────────────────────────────────────────────────


def test_ensure_unknown_feature_raises():
    with pytest.raises(ld.FeatureUnavailable):
        ld.ensure("skill.unknown-xyz", prompt=False)


def test_ensure_empty_specs_noop():
    # Should return without touching install machinery.
    ld.ensure("skill.calculator", prompt=False)


def test_ensure_already_satisfied_noop(monkeypatch):
    monkeypatch.setattr(ld, "_is_satisfied", lambda spec: True)
    ld.ensure("skill.excel-author", prompt=False)


def test_ensure_rejects_unsafe_spec(monkeypatch):
    monkeypatch.setattr(ld, "_is_satisfied", lambda spec: False)
    monkeypatch.setitem(ld.SKILL_DEPS, "skill._evil", ("pkg; rm -rf /",))
    try:
        with pytest.raises(ld.FeatureUnavailable, match="unsafe spec"):
            ld.ensure("skill._evil", prompt=False)
    finally:
        del ld.SKILL_DEPS["skill._evil"]


def test_ensure_disabled_raises(monkeypatch):
    monkeypatch.setattr(ld, "_is_satisfied", lambda spec: False)
    monkeypatch.setattr(ld, "_allow_lazy_installs", lambda: False)
    with pytest.raises(ld.FeatureUnavailable, match="disabled"):
        ld.ensure("skill.excel-author", prompt=False)


def test_ensure_install_success(monkeypatch):
    # First check: missing. After install: satisfied.
    calls = {"n": 0}

    def fake_satisfied(spec):
        calls["n"] += 1
        return calls["n"] > 1  # missing first, satisfied after install

    monkeypatch.setattr(ld, "_is_satisfied", fake_satisfied)
    monkeypatch.setattr(ld, "_allow_lazy_installs", lambda: True)
    monkeypatch.setattr(
        ld, "_venv_pip_install", lambda specs, **kw: ld._InstallResult(True, "ok", "")
    )
    ld.ensure("skill.excel-author", prompt=False)


def test_ensure_install_failure_raises(monkeypatch):
    monkeypatch.setattr(ld, "_is_satisfied", lambda spec: False)
    monkeypatch.setattr(ld, "_allow_lazy_installs", lambda: True)
    monkeypatch.setattr(
        ld, "_venv_pip_install",
        lambda specs, **kw: ld._InstallResult(False, "", "pip exploded"),
    )
    with pytest.raises(ld.FeatureUnavailable, match="install failed"):
        ld.ensure("skill.excel-author", prompt=False)


def test_ensure_install_succeeds_but_still_missing(monkeypatch):
    monkeypatch.setattr(ld, "_is_satisfied", lambda spec: False)
    monkeypatch.setattr(ld, "_allow_lazy_installs", lambda: True)
    monkeypatch.setattr(
        ld, "_venv_pip_install", lambda specs, **kw: ld._InstallResult(True, "ok", "")
    )
    with pytest.raises(ld.FeatureUnavailable, match="still not importable"):
        ld.ensure("skill.excel-author", prompt=False)


def test_ensure_prompt_decline(monkeypatch):
    monkeypatch.setattr(ld, "_is_satisfied", lambda spec: False)
    monkeypatch.setattr(ld, "_allow_lazy_installs", lambda: True)
    monkeypatch.setattr(ld.sys.stdin, "isatty", lambda: True, raising=False)
    monkeypatch.setattr(ld.sys.stdout, "isatty", lambda: True, raising=False)
    with patch("builtins.input", return_value="n"):
        with pytest.raises(ld.FeatureUnavailable, match="declined"):
            ld.ensure("skill.excel-author", prompt=True)


# ── _venv_pip_install engine ─────────────────────────────────────────────────


def test_venv_pip_install_empty_specs():
    r = ld._venv_pip_install(())
    assert r.success is True


def test_venv_pip_install_uv_success(monkeypatch):
    monkeypatch.setattr(ld.shutil, "which", lambda name: "/usr/bin/uv")
    proc = MagicMock(returncode=0, stdout="installed", stderr="")
    with patch.object(ld.subprocess, "run", return_value=proc) as run:
        r = ld._venv_pip_install(("openpyxl>=3.1",))
    assert r.success is True
    # uv path should be the first call
    assert run.call_args_list[0].args[0][0] == "/usr/bin/uv"


def test_venv_pip_install_falls_back_to_pip(monkeypatch):
    monkeypatch.setattr(ld.shutil, "which", lambda name: None)  # no uv

    def fake_run(cmd, **kw):
        if "--version" in cmd:
            return MagicMock(returncode=0, stdout="pip 24", stderr="")
        return MagicMock(returncode=0, stdout="done", stderr="")

    with patch.object(ld.subprocess, "run", side_effect=fake_run):
        r = ld._venv_pip_install(("openpyxl>=3.1",))
    assert r.success is True


def test_venv_pip_install_pip_failure(monkeypatch):
    monkeypatch.setattr(ld.shutil, "which", lambda name: None)

    def fake_run(cmd, **kw):
        if "--version" in cmd:
            return MagicMock(returncode=0, stdout="pip", stderr="")
        return MagicMock(returncode=1, stdout="", stderr="resolution impossible")

    with patch.object(ld.subprocess, "run", side_effect=fake_run):
        r = ld._venv_pip_install(("openpyxl>=3.1",))
    assert r.success is False
    assert "resolution impossible" in r.stderr


def test_venv_pip_install_timeout(monkeypatch):
    monkeypatch.setattr(ld.shutil, "which", lambda name: None)

    def fake_run(cmd, **kw):
        if "--version" in cmd:
            return MagicMock(returncode=0, stdout="pip", stderr="")
        raise ld.subprocess.TimeoutExpired(cmd, 1)

    with patch.object(ld.subprocess, "run", side_effect=fake_run):
        r = ld._venv_pip_install(("openpyxl>=3.1",))
    assert r.success is False
    assert "timed out" in r.stderr


# ── active_features / refresh / check_all ────────────────────────────────────


def test_active_features(monkeypatch):
    monkeypatch.setattr(ld, "_is_present", lambda spec: "openpyxl" in spec)
    active = ld.active_features()
    assert "skill.excel-author" in active
    assert "skill.calculator" not in active  # empty specs are skipped


def test_check_all_features_shape(monkeypatch):
    monkeypatch.setattr(ld, "_is_satisfied", lambda spec: False)
    report = ld.check_all_features()
    # empty-spec feature is always available with no command
    assert report["skill.calculator"]["available"] is True
    assert report["skill.calculator"]["command"] is None
    # excel needs openpyxl which we forced missing
    assert report["skill.excel-author"]["available"] is False
    assert report["skill.excel-author"]["missing"] == ["openpyxl>=3.1"]
    assert report["skill.excel-author"]["command"] is not None


def test_refresh_active_features(monkeypatch):
    monkeypatch.setattr(ld, "active_features", lambda: ["skill.excel-author", "skill.tts-voice"])

    def fake_missing(feature):
        return () if feature == "skill.excel-author" else ("edge-tts>=7.0",)

    monkeypatch.setattr(ld, "feature_missing", fake_missing)
    monkeypatch.setattr(ld, "ensure", lambda feature, prompt=False: None)
    results = ld.refresh_active_features(prompt=False)
    assert results["skill.excel-author"] == "current"
    assert results["skill.tts-voice"] == "refreshed"


def test_refresh_active_features_failure(monkeypatch):
    monkeypatch.setattr(ld, "active_features", lambda: ["skill.tts-voice"])
    monkeypatch.setattr(ld, "feature_missing", lambda f: ("edge-tts>=7.0",))

    def boom(feature, prompt=False):
        raise ld.FeatureUnavailable(feature, ("edge-tts>=7.0",), "install failed: x")

    monkeypatch.setattr(ld, "ensure", boom)
    results = ld.refresh_active_features(prompt=False)
    assert results["skill.tts-voice"].startswith("failed")


def test_refresh_active_features_skipped(monkeypatch):
    monkeypatch.setattr(ld, "active_features", lambda: ["skill.tts-voice"])
    monkeypatch.setattr(ld, "feature_missing", lambda f: ("edge-tts>=7.0",))

    def declined(feature, prompt=False):
        raise ld.FeatureUnavailable(feature, ("edge-tts>=7.0",), "user declined install")

    monkeypatch.setattr(ld, "ensure", declined)
    results = ld.refresh_active_features(prompt=False)
    assert results["skill.tts-voice"].startswith("skipped")
