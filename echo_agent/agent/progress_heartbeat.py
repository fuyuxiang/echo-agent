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


@dataclass
class SharedActivityState:
    """Written by the inference stage, read by ProgressHeartbeat (one-way)."""

    started_at: float
    current_tool: str | None = None
    phase: str = "thinking"  # thinking | calling_tool | generating
    last_visible_feedback_at: float = 0.0

    def enter_tool(self, name: str) -> None:
        self.current_tool = name
        self.phase = "calling_tool"

    def exit_tool(self) -> None:
        self.current_tool = None
        self.phase = "thinking"

    def set_generating(self) -> None:
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
        )


# tick granularity: poll at most this often so stop() reacts promptly
_TICK_SEC = 0.5


class ProgressHeartbeat:
    """Per-turn level-triggered progress timer. One instance per turn."""

    def __init__(self, bus, event, config) -> None:
        self._bus = bus
        self._event = event
        self._config = config
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
            await asyncio.sleep(self._config.first_delay_sec)
            while not self._sealed:
                activity = self._activity
                if activity is not None and not self._sealed:
                    if self._should_beat(activity):
                        await self._publish(activity)
                        activity.mark_visible_feedback()
                await asyncio.sleep(min(_TICK_SEC, self._config.interval_sec))
        except asyncio.CancelledError:
            raise
        except Exception as e:  # noqa: BLE001 — heartbeat must never crash a turn
            logger.debug("heartbeat loop error: {}", e)

    def _should_beat(self, activity: SharedActivityState) -> bool:
        # The first_delay_sec gate already provided the initial silence window.
        # Afterwards, throttle only against an actual recent visible feedback:
        # if none has happened yet, beat; otherwise require interval_sec to pass.
        last_fb = activity.last_visible_feedback_at
        if not last_fb:
            return True
        return (time.monotonic() - last_fb) >= self._config.interval_sec

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
                "_hb_on_uneditable": self._config.on_uneditable,
            }
            logger.info("heartbeat published: channel={} text={!r}", ev.channel, text)
            await self._bus.publish_outbound(out)
        except Exception as e:  # noqa: BLE001
            logger.debug("heartbeat publish failed: {}", e)
