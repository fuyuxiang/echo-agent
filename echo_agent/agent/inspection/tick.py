from __future__ import annotations

import time
from typing import Any

from loguru import logger

from echo_agent.agent.inspection.prompt import build_inspection_prompt
from echo_agent.agent.inspection.store import InspectStore
from echo_agent.bus.events import ContentBlock, ContentType, EventType, InboundEvent


async def run_inspection_tick(
    store: InspectStore, cfg: Any, bus: Any, *, now_sec: float | None = None
) -> int:
    """Run one inspection tick. Returns the number of due items dispatched."""
    now = int(now_sec if now_sec is not None else time.time())
    try:
        items = store.load_items()
        state = store.load_state()
        due = store.due_items(items, state, now, cfg.max_items_per_tick)
    except Exception as e:
        logger.warning("inspection tick load failed (skipped): {}", e)
        return 0
    if not due:
        return 0

    prompt = build_inspection_prompt(due, state)
    channel = getattr(cfg, "deliver_channel", "") or "cron"
    chat_id = getattr(cfg, "deliver_chat_id", "") or "inspection"
    event = InboundEvent(
        event_type=EventType.CRON,
        channel=channel,
        sender_id="inspection",
        chat_id=chat_id,
        content=[ContentBlock(type=ContentType.TEXT, text=prompt)],
        metadata={"_inspection": True},
    )
    try:
        accepted = await bus.publish_inbound(event)
    except Exception as e:
        logger.warning("inspection tick publish failed: {}", e)
        return 0
    if not accepted:
        logger.warning("inspection tick rejected by bus")
        return 0

    # mark due items checked; keep prior conclusion (agent回填留后续)
    for item in due:
        entry = state.get(item.name, {})
        entry["last_checked_at"] = now
        entry.setdefault("last_conclusion", "")
        state[item.name] = entry
    store.save_state(state)
    return len(due)
