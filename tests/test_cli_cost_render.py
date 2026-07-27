"""Tests for echo_agent.cli.cost rendering helpers and the show_cost driver.

Complements tests/test_cli_cost.py (which covers the SQL aggregation helpers
against a real SQLite backend). Here we cover the pure render functions and the
missing-DB degrade path with everything mocked.
"""

from __future__ import annotations

from types import SimpleNamespace
from unittest.mock import patch

from echo_agent.cli import cost as cost_mod

_T = "echo_agent.cli.cost"


def test_fmt_int_thousands():
    assert cost_mod._fmt_int(1234567) == "1,234,567"
    assert cost_mod._fmt_int(0) == "0"


def test_render_today_missing_table_warns(capsys):
    cost_mod._render_today(None, 0.0, "2026-06-19")
    out = capsys.readouterr().out
    assert "无成本归因表" in out


def test_render_today_empty_rows_warns(capsys):
    cost_mod._render_today([], 0.0, "2026-06-19")
    assert "暂无成本数据" in capsys.readouterr().out


def test_render_today_with_rows(capsys):
    rows = [
        {"channel": "telegram", "model": "gpt-4o", "spent": 0.4,
         "input_tokens": 12400, "output_tokens": 3100},
        {"channel": None, "model": "gpt-4o-mini", "spent": 0.08,
         "input_tokens": 8200, "output_tokens": 1900},
    ]
    cost_mod._render_today(rows, 0.48, "2026-06-19")
    out = capsys.readouterr().out
    assert "telegram" in out
    assert "12,400" in out
    assert "—" in out  # None channel rendered as dash
    assert "合计 $0.4800" in out


def test_render_trend_empty_noop(capsys):
    cost_mod._render_trend([])
    assert capsys.readouterr().out == ""


def test_render_trend_bars(capsys):
    rows = [
        {"window_date": "2026-06-17", "total": 1.0},
        {"window_date": "2026-06-18", "total": 0.5},
    ]
    cost_mod._render_trend(rows)
    out = capsys.readouterr().out
    assert "2026-06-17" in out
    assert "近期每日趋势" in out
    assert "█" in out


def test_show_cost_no_db_degrades(tmp_path, capsys):
    cfg = SimpleNamespace(
        workspace=str(tmp_path),
        storage=SimpleNamespace(database_path="db.sqlite"),
    )
    with patch(f"{_T}.resolve_config_file", return_value=None), \
         patch(f"{_T}.load_config", return_value=cfg), \
         patch(f"{_T}.resolve_effective_workspace", return_value=tmp_path):
        # No db file present -> renders missing-table warning, no crash.
        cost_mod.show_cost(workspace=str(tmp_path))
    assert "无成本归因表" in capsys.readouterr().out
