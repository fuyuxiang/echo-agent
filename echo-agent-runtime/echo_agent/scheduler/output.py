"""Receipt-driven scheduled output, with safe recovery after fresh chat activity."""
from __future__ import annotations

import asyncio
import hashlib
import json
import os
import tempfile
from dataclasses import replace
from datetime import datetime
from pathlib import Path
from zoneinfo import ZoneInfo

from loguru import logger

from echo_agent.bus.events import EventType
from echo_agent.bus.delivery import DeliveryResult, DeliveryStage


class ScheduledOutputs:
    def __init__(self, bus, scheduler, store, workspace: Path):
        self.bus, self.scheduler, self.store = bus, scheduler, store
        self.workspace = workspace.resolve()
        self._sources = {}
        self._recovery_sources = {}
        self.artifact_deliver = None
        self._recoveries: dict[tuple[str, str], asyncio.Task] = {}
        self._history_lock = asyncio.Lock()

    def register(self, event) -> None:
        if event.event_type == EventType.CRON:
            self._sources[event.event_id] = replace(event, metadata=dict(event.metadata))

    def _run(self, event):
        source = event.delivery_origin
        if source is None or source.event_type != EventType.CRON:
            return None, None
        job = self.scheduler.get_job(str(source.metadata.get("job_id") or ""))
        run_id = str(source.metadata.get("run_id") or "")
        run = next((r for r in job.run_history if r.get("run_id") == run_id), None) if job else None
        return job, run

    async def prepare(self, event) -> None:
        source_id = str(event.metadata.get("_inbound_event_id") or "")
        source = self._sources.get(source_id) or (self._recovery_sources.get(source_id) or [None])[0]
        if event.delivery_origin is None and source is not None:
            event.delivery_origin = source
        job, run = self._run(event)
        if not job or not run or not event.is_final or event.message_kind != "final":
            return
        # An error summary is useful in chat, but must never replace or later
        # replay alongside the actual report that is waiting for delivery.
        if event.metadata.get("_turn_status") in {"failed", "incomplete", "interrupted"}:
            event.metadata["scheduled_notice"] = True
            return
        artifact_id = str(event.metadata.get("_artifact_id") or event.metadata.get("scheduled_artifact_id") or "")
        if not artifact_id and any(item.get("artifact_id") for item in run.get("deliveries", [])):
            event.metadata["scheduled_notice"] = True
            return
        if (event.channel, event.chat_id) != (event.delivery_origin.channel, event.delivery_origin.chat_id):
            return
        part = event.metadata.get("_artifact_part", event.metadata.get("scheduled_part", 0))
        identity = f"{job.id}:{run['run_id']}:{artifact_id or 'reply'}:{part}"
        event.event_id = "scheduled:" + hashlib.sha256(identity.encode()).hexdigest()[:32]
        event.metadata["scheduled_output"] = True
        event.metadata["job_id"] = job.id
        event.metadata["run_id"] = run["run_id"]
        event.metadata["scheduled_artifact_id"] = artifact_id
        event.metadata["scheduled_part"] = part
        if event.metadata.get("_artifact_parts"):
            event.metadata["scheduled_parts"] = event.metadata["_artifact_parts"]
        saved = await self.store.get_outbound_event(event.event_id)
        if saved is not None:
            # The first accepted delivery intent is immutable. Caption changes
            # during a model retry must not produce a second report or conflict
            # with the transport's durable multipart fingerprints.
            retry = event.metadata.get("_delivery_retry")
            event.content, event.metadata = saved.content, dict(saved.metadata)
            if retry:
                event.metadata["_delivery_retry"] = True
        if artifact_id:
            event.metadata.update(_artifact_id=artifact_id, _tool_delivery=True)
            if part:
                event.metadata["_artifact_part"] = part
                event.metadata["_artifact_parts"] = event.metadata.get("scheduled_parts", part)
        await self._history(job, run, "success" if run.get("delivery_status") == "delivered" else "pending_delivery")

    async def record(self, event, result) -> None:
        if not event.metadata.get("scheduled_output"):
            return
        job, run = self._run(event)
        if job is None or run is None:
            return
        status = ("delivered" if result.ok and not result.detail.get("skipped") and not result.detail.get("deferred")
                  else "unknown" if result.detail.get("delivery_unknown")
                  else "awaiting_context" if result.detail.get("recovery_action") in {"send_message", "relogin"}
                  else "failed")
        item = {
            "event_id": event.event_id, "artifact_id": event.metadata.get("_artifact_id", ""),
            "status": status, "error": result.error or "", "error_code": result.detail.get("error_code", ""),
            "recovery_action": result.detail.get("recovery_action", ""),
            "channel": event.channel, "chat_id": event.chat_id,
            "part": event.metadata.get("scheduled_part", 0),
            "total_parts": event.metadata.get("scheduled_parts", 1),
            "updated_at_ms": int(datetime.now().timestamp() * 1000),
        }
        await self.scheduler.record_delivery(job.id, run["run_id"], item)
        await self._history(job, run, "success" if run.get("delivery_status") == "delivered" else "pending_delivery")

    async def finished(self, event) -> None:
        self._sources.pop(event.event_id, None)
        job = self.scheduler.get_job(str(event.metadata.get("job_id") or ""))
        run = next((r for r in job.run_history if r.get("run_id") == event.metadata.get("run_id")), None) if job else None
        if run is not None:
            await self._history(job, run, "success" if run.get("delivery_status") == "delivered" else
                                "pending_delivery" if run.get("deliveries") else "failed")

    def channel_ready(self, channel: str, chat_id: str) -> None:
        key = (channel, chat_id)
        if key not in self._recoveries:
            task = asyncio.create_task(self._recover(channel, chat_id))
            self._recoveries[key] = task
            task.add_done_callback(lambda _: self._recoveries.pop(key, None))

    async def resume_waiting(self, channel_lookup) -> None:
        for row in await self.store.list_outbound():
            if row["status"] != "awaiting_context" or not row["payload"].get("metadata", {}).get("scheduled_output"):
                continue
            payload = row["payload"]
            channel = channel_lookup(payload["channel"])
            readiness = getattr(channel, "delivery_readiness", None)
            if channel is not None and channel.is_running and callable(readiness) and readiness(payload["chat_id"]) is None:
                self.channel_ready(payload["channel"], payload["chat_id"])

    async def _recover(self, channel: str, chat_id: str) -> None:
        try:
            # Only explicit rejections are safe to replay automatically. Unknown
            # sends retain their existing operator verification requirement.
            for row in await self.store.waiting_outbound(channel, chat_id):
                event = await self.store.get_outbound_event(row["event_id"])
                job, run = self._run(event)
                if not job or not run or not event.metadata.get("scheduled_output"):
                    continue
                if any(not part["message_id"] for part in await self.store.outbound_parts(event.event_id, channel)):
                    continue
                event.metadata["_delivery_retry"] = True
                self.bus.track_outbound(event.event_id)
                if not (await self.retry(event)).ok:
                    break
        except asyncio.CancelledError:
            raise
        except Exception:
            logger.exception("Scheduled output recovery failed for {}", channel)

    async def retry(self, event):
        """The same complete artifact resume powers automatic and operator recovery."""
        job, run = self._run(event)
        artifact_id = event.metadata.get("scheduled_artifact_id")
        if not job or not run or not artifact_id or self.artifact_deliver is None:
            return await asyncio.wait_for(self.bus.publish_outbound(event), 180)
        from echo_agent.tools import ToolExecutionContext

        source = event.delivery_origin
        recovery = self._recovery_sources.setdefault(source.event_id, [source, 0])
        recovery[1] += 1
        try:
            tool = self.artifact_deliver
            chunk_chars = event.metadata.get("artifact_chunk_chars")
            if isinstance(chunk_chars, int) and 0 < chunk_chars <= 100000 and chunk_chars != tool._text_fallback_chunk_chars:
                from echo_agent.agent.tools.artifact import ArtifactDeliverTool

                tool = ArtifactDeliverTool(tool._store, tool._publish, tool._channel_lookup,
                    text_fallback_max_chars=tool._text_fallback_max_chars, text_fallback_chunk_chars=chunk_chars)
            result = await asyncio.wait_for(tool.execute({
                "artifact_id": artifact_id,
                "caption": event.metadata.get("artifact_caption", event.text)[:160],
            }, ToolExecutionContext(session_key=source.session_key, channel=event.channel, chat_id=event.chat_id,
                inbound_event_id=source.event_id,
                artifact_intent_id=event.metadata.get("artifact_intent_id", ""))), 180)
            if result.success:
                rows = await self.store._storage.fetch_sql("SELECT status FROM outbound_messages WHERE event_id=?", (event.event_id,))
                if rows and rows[0]["status"] != "delivered":
                    await self.store.update_outbound(event.event_id, "delivered")
                return DeliveryResult(DeliveryStage.DELIVERED, event.channel)
            return DeliveryResult(DeliveryStage.FAILED, event.channel, error=result.error,
                                  detail={"delivery_unknown": run.get("delivery_status") == "unknown"})
        finally:
            recovery[1] -= 1
            if not recovery[1]:
                self._recovery_sources.pop(source.event_id, None)

    async def stop(self) -> None:
        tasks = list(self._recoveries.values())
        for task in tasks:
            task.cancel()
        if tasks:
            await asyncio.gather(*tasks, return_exceptions=True)

    async def _history(self, job, run, status: str) -> None:
        """Optional report journal integration. Transport receipts own these fields.

        Research content/technology metadata remains editable. Delivery facts
        are reconciled after each receipt and again after the model turn ends.
        The scheduler ledger remains authoritative if this optional file fails.
        """
        relative = job.payload.get("report_history_path")
        if not relative:
            return
        path = (self.workspace / relative).resolve()
        if not path.is_relative_to(self.workspace) or path.suffix != ".json":
            raise ValueError("report_history_path must be a JSON file inside the workspace")
        zone = ZoneInfo(job.timezone) if job.timezone else None
        day = datetime.fromtimestamp(run["ts"] / 1000, tz=zone).date().isoformat()
        async with self._history_lock:
            try:
                data = json.loads(path.read_text()) if path.exists() else {}
                reports = data.setdefault("reports", [])
                report = next((r for r in reports if r.get("date") == day), None)
                if report is None:
                    report = {"date": day}
                    reports.append(report)
                report.update(status=status, job_id=job.id, run_id=run["run_id"],
                              delivery_status=run.get("delivery_status", "pending"))
                delivered = status == "success"
                acknowledged_at = max((item.get("updated_at_ms", run["ts"]) for item in run.get("deliveries", [])), default=run["ts"])
                report["pushed_at"] = datetime.fromtimestamp(acknowledged_at / 1000, tz=zone).astimezone(zone).isoformat() if delivered else None
                for technology in data.get("technologies", []):
                    if not delivered and technology.get("last_pushed") == day:
                        technology["last_pushed"] = None
                        technology["pending_push_date"] = day
                    elif delivered and (technology.get("pending_push_date") == day or technology.get("first_seen") == day):
                        technology["last_pushed"] = day
                        technology.pop("pending_push_date", None)
                meta = data.setdefault("meta", {})
                if str(meta.get("last_run", ""))[:10] <= day:
                    meta["last_run_status"] = status
                path.parent.mkdir(parents=True, exist_ok=True)
                fd, temporary = tempfile.mkstemp(prefix=f".{path.name}.", dir=path.parent)
                try:
                    with os.fdopen(fd, "w", encoding="utf-8") as handle:
                        json.dump(data, handle, ensure_ascii=False, indent=2)
                        handle.flush()
                        os.fsync(handle.fileno())
                    os.replace(temporary, path)
                finally:
                    Path(temporary).unlink(missing_ok=True)
            except (OSError, ValueError, TypeError, AttributeError):
                logger.exception("Could not reconcile scheduled report history for {}", job.id)
