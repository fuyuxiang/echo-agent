"""Receipt-driven scheduled output, with safe recovery after fresh chat activity."""
from __future__ import annotations

import asyncio
import hashlib
import json
import os
import tempfile
import re
from dataclasses import replace
from datetime import datetime
from pathlib import Path
from zoneinfo import ZoneInfo
from urllib.parse import urlsplit

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
        self._recovery_loop: asyncio.Task | None = None
        self._stopping = False
        self.recovery_interval_seconds = 30.0

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

    async def artifact_progress(self, source, tool, manifest) -> None:
        """Persist generation separately from transport, before attempting a send."""
        if source.event_type != EventType.CRON or not isinstance(manifest, dict) or not manifest.get("artifact_id"):
            return
        job = self.scheduler.get_job(str(source.metadata.get("job_id") or ""))
        run = next((r for r in job.run_history if r.get("run_id") == source.metadata.get("run_id")), None) if job else None
        if job is None or run is None:
            return
        fields = {"artifact_id": manifest["artifact_id"], "report_title": manifest.get("title", ""),
                  "report_bytes": manifest.get("total_bytes", 0),
                  "generation_status": "ready" if manifest.get("state") == "finalized" else "generating"}
        if manifest.get("state") == "finalized" and tool is not None:
            try:
                path, _ = await tool._store.finalized_path(source.session_key, manifest["artifact_id"])
                fields.update(await self._archive(job, run, path))
                fields["archive_error"] = ""
            except (OSError, ValueError):
                logger.exception("Could not archive scheduled report for {}", job.id)
                fields["archive_error"] = "report_archive_failed"
        await self.scheduler.record_artifact(job.id, run["run_id"], fields)
        if fields["generation_status"] == "ready":
            await self._history(job, run, "pending_delivery")

    async def _archive(self, job, run, source: Path) -> dict:
        relative = job.payload.get("report_history_path")
        # Always retain a workspace-relative download reference, even for jobs
        # without an optional report journal.
        source = source.resolve()
        if not source.is_relative_to(self.workspace):
            raise ValueError("report must be inside the workspace")
        fields = {"report_file": str(source.relative_to(self.workspace))}
        if not relative:
            return fields
        history = (self.workspace / relative).resolve()
        if not history.is_relative_to(self.workspace) or history.suffix != ".json":
            raise ValueError("report history must be a JSON file inside the workspace")
        zone = ZoneInfo(job.timezone) if job.timezone else None
        day = datetime.fromtimestamp(run["ts"] / 1000, tz=zone).date().isoformat()
        path = history.parent / "reports" / (day + source.suffix)
        if not path.resolve().is_relative_to(self.workspace):
            raise ValueError("report archive escaped the workspace")
        content = source.read_bytes()
        async with self._history_lock:
            # A later rerun must not replace a report already linked to a
            # receipt. Preserve the requested daily path for the first report.
            if path.exists() and path.read_bytes() != content:
                path = path.with_name(f"{day}-{run['run_id'][:12]}{source.suffix}")
            path.parent.mkdir(parents=True, exist_ok=True)
            fd, temporary = tempfile.mkstemp(prefix=".report-", dir=path.parent)
            try:
                with os.fdopen(fd, "wb") as handle:
                    handle.write(content)
                    handle.flush()
                    os.fsync(handle.fileno())
                os.replace(temporary, path)
            finally:
                Path(temporary).unlink(missing_ok=True)
        fields["report_file"] = str(path.relative_to(self.workspace))
        if source.suffix.lower() in {".md", ".txt"}:
            text = content.decode("utf-8")
            fields["report_sections"] = re.findall(r"^#{2,4}\s+(.+)$", text, re.M)[:40]
            fields["report_sources"] = list(dict.fromkeys(re.findall(r"https?://[^\s<>\)\]\"']+", text)))[:100]
            fields["report_items"] = []
            for section in re.finditer(r"^####\s+\d+[.、]\s+(.+)\n([\s\S]*?)(?=^#{1,4}\s|\Z)", text, re.M):
                urls = list(dict.fromkeys(re.findall(r"https?://[^\s<>\)\]\"']+", section[2])))[:20]
                fields["report_items"].append({"name": section[1].strip(), "sources": urls})
                if len(fields["report_items"]) >= 40:
                    break
        return fields

    @staticmethod
    def _reconcile_items(data, run, day, delivered):
        """Recover dedup facts from the saved report, never fabricate research.

        Detailed research fields remain model-owned. For existing technology
        journals we can still preserve names, exact source links and receipt-
        backed push dates when the model did not finish updating its index.
        """
        technologies = data.get("technologies")
        if not isinstance(technologies, list):
            return

        def paper(value):
            match = re.search(r"(?:arxiv:|arxiv\.org/(?:abs|pdf)/)(\d{4}\.\d{4,5})", str(value), re.I)
            return match[1] if match else ""

        for item in run.get("report_items", []):
            sources = item["sources"]
            paper_id = next((paper(url) for url in sources if paper(url)), "")
            existing = next((t for t in technologies if isinstance(t, dict) and (
                t.get("name") == item["name"] or
                paper_id and paper_id in {paper(t.get("paper_id")), paper(t.get("official_url"))}
            )), None)
            if existing is None:
                existing = {"name": item["name"], "first_seen": day, "sources": sources,
                            "extracted_from_report": run.get("report_file", "")}
                if sources:
                    existing["official_url"] = next((u for u in sources if paper(u)), sources[0])
                    repos = [u for u in sources if u.lower().startswith("https://github.com/") or u.lower().startswith("http://github.com/")]
                    if repos:
                        existing["repo"] = urlsplit(repos[0]).path.strip("/")
                if paper_id:
                    existing["paper_id"] = "arXiv:" + paper_id
                technologies.append(existing)
            if delivered and str(existing.get("last_pushed") or "") <= day:
                existing["last_pushed"] = day
                existing.pop("pending_push_date", None)
            elif not delivered and str(existing.get("last_pushed") or "") < day:
                existing["pending_push_date"] = day

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
            # Also reconcile legacy/pending outputs after an upgrade. The
            # immutable outbound file is the source of truth, never LLM prose.
            file = next((b.url for b in event.content if b.type.value == "file" and b.url), "")
            if file and not run.get("report_file"):
                try:
                    fields = await self._archive(job, run, Path(file))
                    fields.update(artifact_id=artifact_id, generation_status="ready")
                    await self.scheduler.record_artifact(job.id, run["run_id"], fields)
                except (OSError, ValueError):
                    logger.exception("Could not archive pending scheduled output for {}", job.id)
        await self._history(job, run, "success" if run.get("delivery_status") == "delivered" else "pending_delivery")

    async def record(self, event, result) -> None:
        if not event.metadata.get("scheduled_output"):
            return
        job, run = self._run(event)
        if job is None or run is None:
            return
        status = ("delivered" if result.ok and not result.detail.get("skipped") and not result.detail.get("deferred")
                  else "unknown" if result.detail.get("delivery_unknown")
                  else "awaiting_context" if result.detail.get("recovery_action") in {"send_message", "relogin", "retry_later"}
                  else "failed")
        item = {
            "event_id": event.event_id, "artifact_id": event.metadata.get("_artifact_id", ""),
            "status": status, "error": result.error or "", "error_code": result.detail.get("error_code", ""),
            "recovery_action": result.detail.get("recovery_action", ""),
            "retry_at_ms": result.detail.get("retry_at_ms", 0),
            "retry_attempts": result.detail.get("retry_attempts", 0),
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
        if self._stopping:
            return
        key = (channel, chat_id)
        if key not in self._recoveries:
            task = asyncio.create_task(self._recover(channel, chat_id))
            self._recoveries[key] = task
            task.add_done_callback(lambda _: self._recoveries.pop(key, None))

    async def resume_waiting(self, channel_lookup) -> None:
        await self._scan_waiting(channel_lookup, startup=True)
        if self._recovery_loop is None and not self._stopping:
            self._recovery_loop = asyncio.create_task(self._watch_waiting(channel_lookup), name="scheduled-delivery-recovery")

    async def _watch_waiting(self, channel_lookup) -> None:
        while not self._stopping:
            await asyncio.sleep(self.recovery_interval_seconds)
            try:
                await self._scan_waiting(channel_lookup)
            except Exception:
                logger.exception("Could not scan waiting scheduled deliveries; recovery will continue")

    async def _scan_waiting(self, channel_lookup, *, startup=False) -> None:
        for route in await self.store.waiting_outbound_routes():
            channel = channel_lookup(route["channel"])
            readiness = getattr(channel, "delivery_readiness", None)
            automatic = getattr(channel, "automatic_delivery_recovery", None)
            if (channel is not None and channel.is_running and callable(readiness)
                    and readiness(route["chat_id"]) is None
                    and (startup or callable(automatic) and automatic(route["chat_id"]))):
                self.channel_ready(route["channel"], route["chat_id"])

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
        self._stopping = True
        tasks = list(self._recoveries.values())
        if self._recovery_loop is not None:
            tasks.append(self._recovery_loop)
            self._recovery_loop = None
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
                report = next((r for r in reports if r.get("date") == day and r.get("run_id") in (None, run["run_id"])), None)
                if report is None:
                    report = {"date": day}
                    reports.append(report)
                report.update(status=status, job_id=job.id, run_id=run["run_id"],
                              delivery_status=run.get("delivery_status", "pending"))
                for source_key, report_key in (("report_file", "file"), ("report_title", "title"),
                                               ("report_sections", "sections"), ("report_sources", "sources")):
                    if run.get(source_key):
                        report[report_key] = run[source_key]
                delivered = status == "success"
                self._reconcile_items(data, run, day, delivered)
                day_delivered = delivered or any(r.get("date") == day and r.get("status") == "success" for r in reports)
                acknowledged_at = max((item.get("updated_at_ms", run["ts"]) for item in run.get("deliveries", [])), default=run["ts"])
                report["pushed_at"] = datetime.fromtimestamp(acknowledged_at / 1000, tz=zone).astimezone(zone).isoformat() if delivered else None
                for technology in data.get("technologies", []):
                    if not day_delivered and technology.get("last_pushed") == day:
                        technology["last_pushed"] = None
                        technology["pending_push_date"] = day
                    elif (delivered and (technology.get("pending_push_date") == day or technology.get("first_seen") == day)
                          and str(technology.get("last_pushed") or "") <= day):
                        technology["last_pushed"] = day
                        technology.pop("pending_push_date", None)
                meta = data.setdefault("meta", {})
                previous_ms = meta.get("last_run_ms", 0)
                if not previous_ms and meta.get("last_run"):
                    try:
                        previous_ms = datetime.fromisoformat(meta["last_run"]).timestamp() * 1000
                    except (ValueError, TypeError):
                        previous_ms = 0
                if previous_ms <= run["ts"]:
                    meta["last_run_status"] = status
                    meta["last_run"] = datetime.fromtimestamp(run["ts"] / 1000, tz=zone).astimezone(zone).isoformat()
                    meta["last_run_ms"] = run["ts"]
                meta["reports_count"] = len(reports)
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
