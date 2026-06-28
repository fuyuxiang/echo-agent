"""Dispatch tests for echo_agent.cli.evolution_cmd.run_evolution_command.

The per-action async helpers are covered in tests/test_evolution_cli.py. Here
we only exercise the synchronous dispatcher: asyncio.run is mocked so no event
loop is started, and we assert the right helper is selected plus the argument
guards (missing id / skill) and the unknown-action exit.
"""

from __future__ import annotations

from unittest.mock import MagicMock, patch

import pytest

from echo_agent.cli import evolution_cmd

_T = "echo_agent.cli.evolution_cmd"


# Each action dispatches to an async helper via asyncio.run. We patch both the
# helper (so evaluating helper(...) yields a plain value, not a live coroutine
# that would later warn "never awaited") and asyncio.run (so no event loop
# starts). This keeps the dispatcher test synchronous and warning-clean.
@pytest.mark.parametrize("action,helper", [
    ("status", "_status"),
    ("run", "_run_once"),
    ("list-candidates", "_list_candidates"),
])
def test_dispatch_no_arg_actions_run_async(action, helper):
    with patch(f"{_T}.{helper}", MagicMock(return_value=None)), \
         patch(f"{_T}.asyncio.run") as run:
        evolution_cmd.run_evolution_command(action)
    run.assert_called_once()


def test_dispatch_show_candidate_requires_id(capsys):
    with pytest.raises(SystemExit) as exc:
        evolution_cmd.run_evolution_command("show-candidate", candidate_id="")
    assert exc.value.code == 1
    assert "Usage" in capsys.readouterr().out


def test_dispatch_show_candidate_with_id():
    with patch(f"{_T}._show_candidate", MagicMock(return_value=None)), \
         patch(f"{_T}.asyncio.run") as run:
        evolution_cmd.run_evolution_command("show-candidate", candidate_id="c1")
    run.assert_called_once()


def test_dispatch_promote_requires_id(capsys):
    with pytest.raises(SystemExit):
        evolution_cmd.run_evolution_command("promote", candidate_id="")
    assert "Usage" in capsys.readouterr().out


def test_dispatch_promote_with_id():
    with patch(f"{_T}._promote_candidate", MagicMock(return_value=None)), \
         patch(f"{_T}.asyncio.run") as run:
        evolution_cmd.run_evolution_command("promote", candidate_id="c1")
    run.assert_called_once()


def test_dispatch_rollback_requires_skill(capsys):
    with pytest.raises(SystemExit):
        evolution_cmd.run_evolution_command("rollback", skill="")
    assert "Usage" in capsys.readouterr().out


def test_dispatch_rollback_with_skill():
    with patch(f"{_T}._rollback", MagicMock(return_value=None)), \
         patch(f"{_T}.asyncio.run") as run:
        evolution_cmd.run_evolution_command("rollback", skill="my-skill")
    run.assert_called_once()


def test_dispatch_init_dataset_calls_sync_helper():
    with patch(f"{_T}._init_dataset") as fn:
        evolution_cmd.run_evolution_command("init-dataset")
    fn.assert_called_once()


def test_dispatch_unknown_action_exits(capsys):
    with pytest.raises(SystemExit) as exc:
        evolution_cmd.run_evolution_command("frobnicate")
    assert exc.value.code == 1
    out = capsys.readouterr().out
    assert "Unknown evolution action" in out


# ── _format_run pure helper ──────────────────────────────────────────────────

def test_format_run_none():
    assert "no runs yet" in evolution_cmd._format_run(None)


def test_init_dataset_writes_and_skips(tmp_path, capsys):
    from types import SimpleNamespace

    cfg = SimpleNamespace(
        workspace=str(tmp_path),
        evolution=SimpleNamespace(eval_dataset_path="data/eval/baseline.yaml"),
    )
    with patch("echo_agent.config.loader.resolve_config_file", return_value=None), \
         patch("echo_agent.config.loader.load_config", return_value=cfg):
        evolution_cmd._init_dataset(None, None)
    written = tmp_path / "data/eval/baseline.yaml"
    assert written.exists()
    assert "cases:" in written.read_text(encoding="utf-8")
    assert "Wrote baseline dataset" in capsys.readouterr().out

    # Second call must skip without overwriting.
    with patch("echo_agent.config.loader.resolve_config_file", return_value=None), \
         patch("echo_agent.config.loader.load_config", return_value=cfg):
        evolution_cmd._init_dataset(None, None)
    assert "already exists" in capsys.readouterr().out
