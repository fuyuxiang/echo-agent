"""Event-loop watchdog — detect a frozen asyncio loop and self-heal via exit.

The gateway runs a single asyncio event loop; any synchronous blocking call on
that loop thread (e.g. an unbounded ``rglob``/``read_text``) starves every other
task — channel polling, the HTTP health endpoint, other sessions. When that
happens nothing on the loop can observe it, because the observer coroutine is
itself starved.

Strategy (do NOT try to "unfreeze" the loop — impossible from outside a thread
stuck in a C call): a cheap heartbeat coroutine stamps ``_last_beat`` on the
loop; a **daemon thread** (which keeps running because blocking syscalls release
the GIL) checks how stale that stamp is. Past ``warn_seconds`` it logs + dumps
tracebacks; past ``kill_seconds`` it dumps and calls ``os._exit`` so the process
manager (launchd KeepAlive / systemd Restart=always) respawns a clean process.

Why this is safe to auto-exit: the heartbeat only stops advancing when the loop
is *actually blocked*. Any normal ``await`` (a 120s model stream, a git clone)
still yields to the heartbeat, so ``kill_seconds`` (default 30s) effectively
only fires on pathological synchronous-blocking, which is the very bug we hit.

Known limitation: a pure-Python busy loop that never releases the GIL would also
stall this daemon thread. That case is rare; the common offender is blocking
I/O, which releases the GIL and lets the watchdog run.
"""

from __future__ import annotations

import asyncio
import faulthandler
import os
import threading
import time
from pathlib import Path
from typing import Callable

from loguru import logger

from echo_agent.observability.restart_guard import RestartGuard


class LoopWatchdog:
    """Detects a frozen event loop and exits so a supervisor can respawn.

    All time comparisons use ``time.monotonic()`` (immune to wall-clock jumps).
    The exit function is injectable so tests never actually kill the process.
    """

    def __init__(
        self,
        *,
        warn_seconds: float = 5.0,
        kill_seconds: float = 30.0,
        check_interval_seconds: float = 5.0,
        heartbeat_interval_seconds: float = 2.0,
        restart_guard: RestartGuard | None = None,
        supervised: bool = True,
        dump_path: Path | None = None,
        exit_fn: Callable[[int], None] = os._exit,
        clock: Callable[[], float] = time.monotonic,
    ) -> None:
        self._warn = warn_seconds
        self._kill = kill_seconds
        self._check_interval = check_interval_seconds
        self._hb_interval = heartbeat_interval_seconds
        self._guard = restart_guard
        self._supervised = supervised
        self._dump_path = dump_path
        self._exit_fn = exit_fn
        self._clock = clock

        self._loop: asyncio.AbstractEventLoop | None = None
        self._hb_task: asyncio.Task | None = None
        self._thread: threading.Thread | None = None
        self._stop = threading.Event()
        self._last_beat = clock()
        self._warned = False
        self._dump_fh = None

    def start(self) -> None:
        """Arm the watchdog: capture the running loop, start heartbeat + thread.

        If the restart guard has tripped (too many self-exits recently) the
        watchdog refuses to arm — self-exiting again would just feed a respawn
        loop. If the process is not supervised, kill actions degrade to
        warn-only so a foreground run is never left dead.
        """
        self._loop = asyncio.get_running_loop()

        if self._guard is not None and self._guard.is_tripped():
            logger.critical(
                "Loop watchdog NOT armed: restart guard tripped "
                "({} self-exits within window). Investigate the freeze; "
                "auto-restart is suspended to avoid a respawn storm.",
                self._guard.recent_count(),
            )
            return

        if not self._supervised:
            logger.warning(
                "Loop watchdog armed in warn-only mode: process is not under a "
                "service manager, so a self-exit would not be respawned."
            )

        # Keep a live traceback dump file so we can write from the frozen state.
        if self._dump_path is not None:
            try:
                self._dump_path.parent.mkdir(parents=True, exist_ok=True)
                self._dump_fh = self._dump_path.open("a", encoding="utf-8")
            except OSError as e:
                logger.debug("Loop watchdog: cannot open dump file: {}", e)

        self._last_beat = self._clock()
        self._stop.clear()
        self._hb_task = self._loop.create_task(self._heartbeat(), name="loop-watchdog-heartbeat")
        self._thread = threading.Thread(
            target=self._monitor, name="loop-watchdog", daemon=True
        )
        self._thread.start()
        logger.info(
            "Loop watchdog armed (warn={}s kill={}s check={}s supervised={})",
            self._warn, self._kill, self._check_interval, self._supervised,
        )

    async def stop(self) -> None:
        self._stop.set()
        if self._hb_task is not None:
            self._hb_task.cancel()
            try:
                await self._hb_task
            except (asyncio.CancelledError, Exception):
                pass
            self._hb_task = None
        if self._thread is not None:
            self._thread.join(timeout=self._check_interval + 1)
            self._thread = None
        if self._dump_fh is not None:
            try:
                self._dump_fh.close()
            except OSError:
                pass
            self._dump_fh = None

    async def _heartbeat(self) -> None:
        """Cheap loop-resident stamp. Stops advancing only if the loop blocks."""
        try:
            while not self._stop.is_set():
                self._last_beat = self._clock()
                await asyncio.sleep(self._hb_interval)
        except asyncio.CancelledError:
            pass

    def _monitor(self) -> None:
        """Daemon-thread loop: watch heartbeat staleness, warn then kill."""
        while not self._stop.wait(self._check_interval):
            stale = self._clock() - self._last_beat
            if stale >= self._kill:
                self._on_kill(stale)
                return
            if stale >= self._warn:
                if not self._warned:
                    self._warned = True
                    self._on_warn(stale)
            else:
                self._warned = False

    def _on_warn(self, stale: float) -> None:
        logger.warning(
            "Event loop stalled for {:.1f}s (warn threshold {}s) — dumping "
            "thread stacks. If this crosses {}s the process will self-exit.",
            stale, self._warn, self._kill,
        )
        self._dump_traceback()

    def _on_kill(self, stale: float) -> None:
        logger.critical(
            "Event loop frozen for {:.1f}s (kill threshold {}s) — dumping stacks "
            "and self-exiting for supervisor respawn.",
            stale, self._kill,
        )
        self._dump_traceback()
        if not self._supervised:
            logger.critical(
                "Not supervised — staying alive in warn-only mode. Manual "
                "restart required; the loop is unrecoverable."
            )
            return
        if self._guard is not None:
            self._guard.record_restart()
        # os._exit: the loop thread is wedged in a C call, so sys.exit (which
        # only raises in this daemon thread) cannot bring the process down.
        self._exit_fn(75)

    def _dump_traceback(self) -> None:
        try:
            fh = self._dump_fh
            if fh is not None:
                fh.write(f"\n===== loop-watchdog dump @ {time.strftime('%Y-%m-%d %H:%M:%S')} =====\n")
                fh.flush()
                faulthandler.dump_traceback(file=fh)
                fh.flush()
            else:
                faulthandler.dump_traceback()
        except Exception as e:  # never let dumping crash the watchdog
            logger.debug("Loop watchdog: traceback dump failed: {}", e)
