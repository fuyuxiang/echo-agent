"""Setup artifacts must use the same workspace as the runtime."""

import pytest
import yaml

from echo_agent.cli import setup
from echo_agent.cli.evolution_cmd import run_evolution_command
from echo_agent.cli.status import _gather_status
from echo_agent.cli.i18n import get_locale, set_locale
from echo_agent.permissions.credential_key import KEY_FILENAME


@pytest.fixture(autouse=True)
def restore_locale():
    saved = get_locale()
    try:
        yield
    finally:
        set_locale(saved)


def test_quickstart_creates_credential_key_in_explicit_workspace(tmp_path, monkeypatch):
    cwd = tmp_path / "shell"
    cwd.mkdir()
    workspace = tmp_path / "runtime"
    monkeypatch.chdir(cwd)
    monkeypatch.delenv("ECHO_AGENT_CREDENTIAL_KEY", raising=False)
    monkeypatch.setattr(setup, "is_interactive", lambda: True)
    for name in ("setup_language", "setup_model", "setup_permissions", "setup_security",
                 "setup_doctor", "_print_summary", "_offer_gateway_start", "_print_banner"):
        monkeypatch.setattr(setup, name, lambda *a, **k: None)

    assert setup.run_setup_wizard(workspace=str(workspace), flow="quickstart") == 0
    assert (workspace / KEY_FILENAME).is_file()
    assert not (cwd / KEY_FILENAME).exists()
    assert yaml.safe_load((workspace / "echo-agent.yaml").read_text())["workspace"] == "."


def test_evolution_setup_seeds_dataset_beside_config_relative_workspace(tmp_path, monkeypatch):
    cwd = tmp_path / "shell"
    cwd.mkdir()
    config_dir = tmp_path / "config"
    config_dir.mkdir()
    target = config_dir / "echo-agent.yaml"
    target.write_text("workspace: runtime\nevolution:\n  enabled: true\n")
    monkeypatch.chdir(cwd)
    monkeypatch.setattr(setup, "is_interactive", lambda: True)
    monkeypatch.setattr(setup, "_offer_gateway_start", lambda *a, **k: None)
    monkeypatch.setattr(setup.ui, "confirm", lambda _q, default=True: default)
    monkeypatch.setattr(setup.ui, "text", lambda _q, default="": default)
    monkeypatch.setattr(setup.ui, "select", lambda _q, _choices, default="": default)

    assert setup.run_setup_wizard(section="evolution", config_path=str(target)) == 0
    assert (config_dir / "runtime/data/eval/baseline.yaml").is_file()
    assert not (cwd / "runtime/data/eval/baseline.yaml").exists()


def test_status_health_probes_the_resolved_workspace(tmp_path, monkeypatch):
    cwd = tmp_path / "shell"
    cwd.mkdir()
    (cwd / "runtime").write_text("this is not the configured workspace")
    config_dir = tmp_path / "config"
    config_dir.mkdir()
    runtime = config_dir / "runtime"
    runtime.mkdir()
    target = config_dir / "echo-agent.yaml"
    target.write_text("workspace: runtime\n")
    monkeypatch.chdir(cwd)
    monkeypatch.setattr("echo_agent.cli.runtime_probe._detect_backend", lambda: None)
    status = _gather_status(str(target), None)
    assert status["workspace"] == str(runtime)
    assert any(p["status"] == "ok" and str(runtime) in p["detail"] for p in status["health"])
    assert not any(str(cwd / "runtime") in p["detail"] for p in status["health"])
    assert yaml.safe_load(target.read_text())["workspace"] == "runtime"


def test_init_dataset_uses_config_relative_workspace(tmp_path, monkeypatch):
    cwd = tmp_path / "shell"
    cwd.mkdir()
    target = tmp_path / "config/echo-agent.yaml"
    target.parent.mkdir()
    target.write_text("workspace: runtime\n")
    monkeypatch.chdir(cwd)
    assert run_evolution_command("init-dataset", config_path=str(target)) == 0
    assert (target.parent / "runtime/data/eval/baseline.yaml").is_file()
    assert not (cwd / "runtime/data/eval/baseline.yaml").exists()
