"""Cost command — display per-dimension cost attribution from cost_ledger_dim."""

from __future__ import annotations

import asyncio
import json
from datetime import date, timedelta
from pathlib import Path
from typing import Any

from loguru import logger

from echo_agent.cli.colors import Colors, color, print_header, print_warning, set_color_override
from echo_agent.cli.workspace import resolve_effective_workspace
from echo_agent.config.loader import load_config, resolve_config_file


async def _table_exists(storage: Any, name: str) -> bool:
    """Probe sqlite_master for *name*.

    Note: SQLiteBackend runs _run_migrations() unconditionally on every
    connect, so against the real backend cost_ledger_dim always exists by the
    time we get here — i.e. opening a cost report on a legacy DB will itself
    materialize the table (report-triggers-migration, accepted by design).
    This probe therefore exists mainly to defend non-standard storage: test
    stubs, foreign/legacy DBs surfaced through another backend, or a future
    read-only connection that skips migrations.

    fetch_sql swallows exceptions and returns [] on error, so a missing table
    cannot be detected by catching a raised exception. We probe the catalog
    explicitly instead.
    """
    rows = await storage.fetch_sql(
        "SELECT name FROM sqlite_master WHERE type = 'table' AND name = ?",
        (name,),
    )
    return bool(rows)


async def _today_report(storage: Any, today: str):
    """Return (rows, total) grouped by channel+model for *today*.

    rows is None when the table is missing (legacy DB before migration 21).
    """
    if not await _table_exists(storage, "cost_ledger_dim"):
        return None, 0.0
    rows = await storage.fetch_sql(
        "SELECT channel, model, SUM(spent_usd) AS spent, "
        "SUM(input_tokens) AS input_tokens, SUM(output_tokens) AS output_tokens "
        "FROM cost_ledger_dim WHERE window_date = ? "
        "GROUP BY channel, model ORDER BY spent DESC",
        (today,),
    )
    total = sum(float(r["spent"]) for r in rows)
    return rows, total


async def _trend_report(storage: Any, since: str):
    """Return per-day totals from *since* (inclusive). [] when table missing."""
    if not await _table_exists(storage, "cost_ledger_dim"):
        return []
    return await storage.fetch_sql(
        "SELECT window_date, SUM(spent_usd) AS total FROM cost_ledger_dim "
        "WHERE window_date >= ? GROUP BY window_date ORDER BY window_date",
        (since,),
    )


def _fmt_int(n: int) -> str:
    return f"{int(n):,}"


def _render_today(rows, total, today: str) -> None:
    print_header(f"今日成本（{today}）— 仅主推理计量")
    if rows is None:
        # A missing cost_ledger_dim is a genuine anomaly: the real
        # SQLiteBackend migrates on connect, so this branch only fires for
        # non-standard storage (stub / foreign DB / read-only connection) or
        # when the DB file is absent/corrupt and we deliberately degrade here.
        print_warning("当前数据库无成本归因表，请先以新版本运行一次 echo-agent。")
        return
    if not rows:
        print_warning("暂无成本数据。")
        return
    print(f"  {'渠道':<12}{'模型':<22}{'花费($)':>10}{'输入tok':>12}{'输出tok':>12}")
    for r in rows:
        ch = r["channel"] or "—"
        print(
            f"  {ch:<12}{r['model']:<22}"
            f"{float(r['spent']):>10.4f}"
            f"{_fmt_int(r['input_tokens']):>12}"
            f"{_fmt_int(r['output_tokens']):>12}"
        )
    print(color(f"  合计 ${total:.4f}", Colors.BOLD))


def _render_trend(rows) -> None:
    if not rows:
        return
    print_header("近期每日趋势")
    peak = max((float(r["total"]) for r in rows), default=0.0) or 1.0
    for r in rows:
        amt = float(r["total"])
        bar = "█" * int(round(amt / peak * 20))
        print(f"  {r['window_date']}  {amt:>8.4f}  {color(bar, Colors.CYAN)}")


def show_cost(config_path: str | Path | None = None,
              workspace: str | Path | None = None, days: int = 7,
              as_json: bool = False) -> int:
    """Render the cost report and return a process exit code.

    Returns 0 on success (data present or a legitimately empty ledger) and 1
    when the cost table is unavailable (missing/legacy/unreadable DB) — a
    signal CI or scripts can gate on. ``as_json`` emits structured JSON with
    color forced off.
    """
    config_file = resolve_config_file(config_path=config_path, search_dir=workspace)
    overrides = {"workspace": str(workspace)} if workspace else None
    config = load_config(config_path=config_file, overrides=overrides)
    effective_workspace = resolve_effective_workspace(
        config,
        str(config_file) if config_file and config_file.exists() else None,
        str(workspace) if workspace else None,
    )
    db_path = effective_workspace / config.storage.database_path

    async def _collect() -> tuple[Any, float, list, bool]:
        """Return (today_rows, total, trend_rows, table_available)."""
        today = date.today().isoformat()
        if not db_path.exists():
            return None, 0.0, [], False
        from echo_agent.storage.sqlite import SQLiteBackend
        storage = SQLiteBackend(db_path)
        try:
            try:
                rows, total = await _today_report(storage, today)
                since = (date.today() - timedelta(days=max(1, days) - 1)).isoformat()
                trend = await _trend_report(storage, since)
            except Exception as exc:  # noqa: BLE001 - degrade on unreadable DB
                logger.warning("cost report: failed to read DB at {}: {}", db_path, exc)
                return None, 0.0, [], False
            return rows, total, trend, rows is not None
        finally:
            await storage.close()

    rows, total, trend, table_available = asyncio.run(_collect())
    today = date.today().isoformat()

    if as_json:
        set_color_override(False)
        try:
            payload = {
                "date": today,
                "table_available": table_available,
                "total": round(float(total), 6),
                "today": [
                    {
                        "channel": r["channel"] or None,
                        "model": r["model"],
                        "spent": round(float(r["spent"]), 6),
                        "input_tokens": int(r["input_tokens"]),
                        "output_tokens": int(r["output_tokens"]),
                    }
                    for r in (rows or [])
                ],
                "trend": [
                    {"date": r["window_date"], "total": round(float(r["total"]), 6)}
                    for r in (trend or [])
                ],
            }
            print(json.dumps(payload, ensure_ascii=False, indent=2))
        finally:
            set_color_override(None)
        return 0 if table_available else 1

    _render_today(rows, total, today)
    _render_trend(trend)
    return 0 if table_available else 1
