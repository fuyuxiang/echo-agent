"""In-memory ring buffer for recent log records.

The dashboard's ``/api/v1/logs`` endpoint reads structured log entries from
here. loguru itself only fans out to sinks (stderr, files); it keeps no queryable
history. We attach a sink that appends a compact dict per record to a bounded
deque, so the API can page/filter recent logs without touching disk.

The buffer is process-global (a single deque) rather than per-``AgentLoop``:
loguru sinks are registered on the one process-wide ``logger``, so binding the
buffer to an instance would leak records across instances and across tests. A
single module-level buffer with an idempotent installer keeps ownership clear.
"""

from __future__ import annotations

import collections
from typing import Any

from loguru import logger

# Keep the most recent N records. Sized to cover a dashboard page (200) many
# times over while staying trivially bounded in memory.
_MAX_LOG_RECORDS = 2000

_buffer: collections.deque[dict[str, Any]] = collections.deque(maxlen=_MAX_LOG_RECORDS)

# loguru sink id, so re-installing replaces the old sink instead of stacking a
# second one that would double every record.
_sink_id: int | None = None


def _record_sink(message: Any) -> None:
    """loguru sink: project each record down to the shape the logs API serves."""
    record = message.record
    _buffer.append(
        {
            "ts": record["time"].isoformat(),
            "level": record["level"].name,
            "message": record["message"],
        }
    )


def install_log_buffer(level: str = "DEBUG") -> collections.deque[dict[str, Any]]:
    """Attach (or re-attach) the buffering sink to the global logger.

    Idempotent: a prior sink is removed first so repeated calls (e.g. logging
    reconfiguration) never duplicate records. Returns the shared buffer.
    """
    global _sink_id
    if _sink_id is not None:
        try:
            logger.remove(_sink_id)
        except ValueError:
            # Sink id already gone (e.g. logger.remove() cleared all sinks).
            pass
    _sink_id = logger.add(_record_sink, level=level, format="{message}")
    return _buffer


def get_log_buffer() -> collections.deque[dict[str, Any]]:
    """Return the shared log buffer (empty until a record is logged)."""
    return _buffer


def clear_log_buffer() -> None:
    """Drop all buffered records. Primarily for test isolation."""
    _buffer.clear()
