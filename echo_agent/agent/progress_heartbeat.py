"""Progress heartbeat — level-triggered feedback for long-running turns.

Sits on top of the existing edge-triggered tool events. A per-turn timer
periodically reports "still working — elapsed — current activity" and keeps
the typing indicator alive, sealing once the final answer is delivered.
"""

from __future__ import annotations

import asyncio
import time
from dataclasses import dataclass

from loguru import logger

from echo_agent.bus.events import OutboundEvent


_TOOL_FRIENDLY: dict[str, str] = {
    "web_search": "正在查阅资料",
    "search": "正在查阅资料",
    "read_file": "正在阅读文档",
    "filesystem": "正在整理文件",
    "shell": "正在执行命令",
    "code_exec": "正在运行代码",
    "memory": "正在回忆相关内容",
    "knowledge": "正在检索知识库",
    "vision": "正在查看图片",
    "document": "正在处理文档",
    "delegate": "正在协调子任务",
}
_PHASE_FRIENDLY: dict[str, str] = {
    "thinking": "思考中",
    "generating": "正在组织答案",
}
_FALLBACK_ACTIVITY = "处理中"


def friendly_activity(snapshot: "ActivitySnapshot") -> str:
    if snapshot.phase == "calling_tool" and snapshot.current_tool:
        return _TOOL_FRIENDLY.get(snapshot.current_tool, _FALLBACK_ACTIVITY)
    return _PHASE_FRIENDLY.get(snapshot.phase, _FALLBACK_ACTIVITY)


def format_elapsed(elapsed_sec: float) -> str:
    secs = int(elapsed_sec)
    if secs < 60:
        return f"{secs} 秒"
    return f"{secs // 60} 分钟"


def render_heartbeat(snapshot: "ActivitySnapshot", template: str) -> str:
    return template.format(
        elapsed=format_elapsed(snapshot.elapsed_sec),
        activity=friendly_activity(snapshot),
    )


@dataclass
class ActivitySnapshot:
    elapsed_sec: float
    phase: str
    current_tool: str | None
    milestone_seq: int = 0


@dataclass
class SharedActivityState:
    """Written by the inference stage, read by ProgressHeartbeat (one-way)."""

    started_at: float
    current_tool: str | None = None
    phase: str = "thinking"  # thinking | calling_tool | generating
    last_visible_feedback_at: float = 0.0
    milestone_seq: int = 0            # monotonic; only real progress bumps it
    last_delivered_milestone: int = 0  # written by delivery layer (manager)
    _first_tool_seen: bool = False
    last_milestone_is_key: bool = False  # current milestone is a key one

    def enter_tool(self, name: str) -> None:
        # Only the thinking -> calling_tool transition is a milestone, so a
        # retry of the same tool without leaving calling_tool does not bump.
        # The first tool entry of the turn is a key milestone; later ones are not.
        if self.phase != "calling_tool":
            self.milestone_seq += 1
            self.last_milestone_is_key = not self._first_tool_seen
            self._first_tool_seen = True
        self.current_tool = name
        self.phase = "calling_tool"

    def exit_tool(self) -> None:
        self.milestone_seq += 1
        self.last_milestone_is_key = False
        self.current_tool = None
        self.phase = "thinking"

    def set_generating(self) -> None:
        if self.phase != "generating":
            self.milestone_seq += 1
            self.last_milestone_is_key = True
        self.phase = "generating"

    def mark_visible_feedback(self, now: float | None = None) -> None:
        self.last_visible_feedback_at = now if now is not None else time.monotonic()

    def since_last_feedback(self, now: float | None = None) -> float:
        ref = now if now is not None else time.monotonic()
        base = self.last_visible_feedback_at or self.started_at
        return ref - base

    def snapshot(self, now: float | None = None) -> ActivitySnapshot:
        ref = now if now is not None else time.monotonic()
        return ActivitySnapshot(
            elapsed_sec=ref - self.started_at,
            phase=self.phase,
            current_tool=self.current_tool,
            milestone_seq=self.milestone_seq,
        )


# tick granularity: poll at most this often so stop() reacts promptly
_TICK_SEC = 0.5


class ProgressHeartbeat:
    """Per-turn level-triggered progress timer. One instance per turn."""

    def __init__(self, bus, event, config, cognitive_emitter=None) -> None:
        self._bus = bus
        self._event = event
        self._config = config
        self._cog = cognitive_emitter
        self._activity: SharedActivityState | None = None
        self._task: asyncio.Task | None = None
        self._sealed = False

    async def start(self, activity: SharedActivityState) -> None:
        if not self._config.enabled:
            return
        self._activity = activity
        self._task = asyncio.create_task(self._run())

    async def stop(self) -> None:
        self._sealed = True
        task = self._task
        self._task = None
        if task is not None:
            task.cancel()
            try:
                await task
            except (asyncio.CancelledError, Exception):  # noqa: BLE001
                pass

    async def _run(self) -> None:
        try:
            first_delay = getattr(self._config, "first_delay_sec", 30)
            await asyncio.sleep(first_delay)
            while not self._sealed:
                activity = self._activity
                if activity is not None and not self._sealed:
                    if self._should_beat(activity):
                        # Capture the seq we are about to deliver before the
                        # await in _publish, so the inference stage bumping a new
                        # milestone mid-publish cannot make us over-advance the
                        # source gate past a beat we never actually sent.
                        beat_seq = activity.milestone_seq
                        await self._publish(activity)
                        # mark_visible_feedback keeps the existing throttle line
                        # alive; the authoritative dedup is last_delivered_milestone.
                        activity.mark_visible_feedback()
                        # Advance the source-side progress gate (I2): once we have
                        # delivered milestone N, do not rebeat the same N until a
                        # genuinely new milestone (N+1) appears.
                        activity.last_delivered_milestone = beat_seq
                await asyncio.sleep(_TICK_SEC)
        except asyncio.CancelledError:
            raise
        except Exception as e:  # noqa: BLE001 — heartbeat must never crash a turn
            logger.debug("heartbeat loop error: {}", e)

    def _should_beat(self, activity: SharedActivityState) -> bool:
        # Progress gate: only beat when a genuine milestone happened since the
        # last one we delivered. Pure thinking/generation never bumps the seq,
        # so a long no-tool generation produces zero text beats.
        if activity.milestone_seq <= activity.last_delivered_milestone:
            return False
        # Key milestones (first tool entry / entering finalize) bypass the
        # min_interval throttle so important progress is never swallowed; the
        # first_delay silence gate (the initial sleep in _run) still applies.
        if getattr(activity, "last_milestone_is_key", False):
            return True
        # Silence gate: throttle bursts of milestones against the last visible
        # feedback. min_interval_sec=0 disables throttling (used in tests).
        # Fall back to the legacy name interval_sec during the rename transition
        # (formal rename lands in Task 5).
        min_interval = getattr(self._config, "min_interval_sec", None)
        if min_interval is None:
            min_interval = getattr(self._config, "interval_sec", 60)
        last_fb = activity.last_visible_feedback_at
        if last_fb and (time.monotonic() - last_fb) < min_interval:
            return False
        return True

    async def _publish(self, activity: SharedActivityState) -> None:
        if self._sealed:
            return
        try:
            text = render_heartbeat(activity.snapshot(), self._config.template)
            ev = self._event
            out = OutboundEvent.text_reply(
                channel=ev.channel,
                chat_id=ev.chat_id,
                text=text,
                reply_to_id=ev.reply_to_id,
                is_final=False,
                message_kind="heartbeat",
            )
            out.metadata = {
                "_heartbeat": True,
                "_inbound_event_id": ev.event_id,
                "_hb_milestone": activity.milestone_seq,
                "_hb_key": activity.last_milestone_is_key,
            }
            logger.info("heartbeat published: channel={} text={!r}", ev.channel, text)
            await self._bus.publish_outbound(out)
            # Additive cognitive emission (cli-gated). Wrapped so it can NEVER
            # break the existing heartbeat publish path on failure.
            if self._cog is not None:
                try:
                    await self._cog.emit(
                        self._event, "heartbeat",
                        {"stage": activity.phase, "note": text}, text,
                    )
                except Exception:  # noqa: BLE001
                    pass
        except Exception as e:  # noqa: BLE001
            logger.debug("heartbeat publish failed: {}", e)
