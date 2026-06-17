"""CLI cost report: aggregation, empty data, missing table."""

from __future__ import annotations

import pytest

from echo_agent.cli.cost import _today_report, _trend_report
from echo_agent.storage.sqlite import SQLiteBackend


def _date_offset(days: int) -> str:
    import datetime

    return (datetime.date.today() - datetime.timedelta(days=days)).isoformat()


async def _seed(tmp_path, rows):
    storage = SQLiteBackend(tmp_path / "db.sqlite")
    await storage.initialize()
    for r in rows:
        await storage.execute_sql(
            "INSERT INTO cost_ledger_dim (window_date, provider, model, channel, "
            "spent_usd, input_tokens, output_tokens, cache_read_tokens, "
            "cache_write_tokens, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            r,
        )
    return storage


@pytest.mark.asyncio
async def test_today_report_groups_and_totals(tmp_path):
    today = __import__("datetime").date.today().isoformat()
    storage = await _seed(tmp_path, [
        (today, "openai", "gpt-4o", "telegram", 0.40, 12400, 3100, 0, 0, "x"),
        (today, "openai", "gpt-4o-mini", "discord", 0.08, 8200, 1900, 0, 0, "x"),
    ])
    rows, total = await _today_report(storage, today)
    assert len(rows) == 2
    assert abs(total - 0.48) < 1e-9
    await storage.close()


@pytest.mark.asyncio
async def test_today_report_empty(tmp_path):
    today = __import__("datetime").date.today().isoformat()
    storage = await _seed(tmp_path, [])
    rows, total = await _today_report(storage, today)
    assert rows == []
    assert total == 0.0
    await storage.close()


@pytest.mark.asyncio
async def test_trend_report_groups_by_day(tmp_path):
    storage = await _seed(tmp_path, [
        ("2026-06-15", "openai", "gpt-4o", "telegram", 0.30, 1, 1, 0, 0, "x"),
        ("2026-06-16", "openai", "gpt-4o", "telegram", 0.50, 1, 1, 0, 0, "x"),
        ("2026-06-16", "openai", "gpt-4o", "discord", 0.20, 1, 1, 0, 0, "x"),
    ])
    rows = await _trend_report(storage, "2026-06-10")
    by_date = {r["window_date"]: r["total"] for r in rows}
    assert abs(by_date["2026-06-15"] - 0.30) < 1e-9
    assert abs(by_date["2026-06-16"] - 0.70) < 1e-9
    await storage.close()


class _NoTableStorage:
    """Storage stub for a legacy/foreign DB lacking cost_ledger_dim.

    SQLiteBackend itself always runs migrations on connect, so the real
    backend can never present a missing table; this stub models the case the
    sentinel path defends against — sqlite_master has no such table.
    """

    async def fetch_sql(self, sql, params=()):
        return []

    async def close(self):
        return None


@pytest.mark.asyncio
async def test_report_missing_table_returns_sentinel(tmp_path):
    storage = _NoTableStorage()
    rows, total = await _today_report(storage, "2026-06-17")
    assert rows is None
    await storage.close()


def test_show_cost_missing_db_does_not_crash(tmp_path, capsys):
    # 指向一个不存在的 workspace/db，show_cost 应优雅退化而非抛异常
    from echo_agent.cli.cost import show_cost
    missing_ws = tmp_path / "nonexistent_workspace"
    show_cost(config_path=None, workspace=str(missing_ws), days=7)
    out = capsys.readouterr().out
    assert "成本" in out  # 至少打印了今日成本标题，未崩溃


@pytest.mark.asyncio
async def test_today_report_filters_to_today(tmp_path):
    today = __import__("datetime").date.today().isoformat()
    yesterday = _date_offset(1)
    storage = await _seed(tmp_path, [
        (today, "openai", "gpt-4o", "telegram", 0.40, 100, 10, 0, 0, "x"),
        (yesterday, "openai", "gpt-4o", "telegram", 0.99, 100, 10, 0, 0, "x"),
    ])
    rows, total = await _today_report(storage, today)
    assert len(rows) == 1
    assert abs(total - 0.40) < 1e-9
    await storage.close()


def test_render_trend_empty_no_output(capsys):
    from echo_agent.cli.cost import _render_trend

    _render_trend([])
    out = capsys.readouterr().out
    assert out == ""


def test_show_cost_corrupt_db_does_not_crash(tmp_path, capsys):
    # 文件存在但不是合法 sqlite 库，show_cost 应兜底退化而非崩溃
    from echo_agent.cli.cost import show_cost

    ws = tmp_path / "ws"
    db = ws / "data" / "echo_agent.db"
    db.parent.mkdir(parents=True)
    db.write_bytes(b"this is not a sqlite database \x00\x01\x02 garbage")
    show_cost(config_path=None, workspace=str(ws), days=7)
    out = capsys.readouterr().out
    assert "成本" in out  # 打印了今日成本标题，未崩溃


def test_cost_subcommand_registered():
    from echo_agent.__main__ import _build_parser
    parser = _build_parser()
    ns = parser.parse_args(["cost", "--days", "14"])
    assert ns.command == "cost"
    assert ns.days == 14


def test_cost_subcommand_days_default():
    from echo_agent.__main__ import _build_parser
    parser = _build_parser()
    ns = parser.parse_args(["cost"])
    assert ns.command == "cost"
    assert ns.days == 7
