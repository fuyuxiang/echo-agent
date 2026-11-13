from __future__ import annotations

from typing import TYPE_CHECKING

from aiohttp import web

from echo_agent.scheduler.authorization import compute_fingerprint
from echo_agent.scheduler.authorization import grant as grant_authorization
from echo_agent.scheduler.authorization import verify as verify_authorization
from echo_agent.scheduler.service import JobStatus, ScheduledJob
from echo_agent.scheduler.timing import schedule_timing

if TYPE_CHECKING:
    from echo_agent.gateway.server import GatewayServer


def _payload_has_content(payload: object) -> bool:
    """A scheduled job needs actual work to do. Mirror delivery.inbound_event_from_job:
    the effective instruction is payload['command'] or payload['message']; without a
    non-empty one the fire-time path raises ValueError, so reject at creation."""
    if not isinstance(payload, dict):
        return False
    command = str(payload.get("command") or payload.get("message") or "").strip()
    return bool(command)


def _validate_job_fields(body: dict) -> str:
    for field in ("name", "cron_expr", "at", "timezone", "trigger"):
        if field in body and not isinstance(body[field], str):
            return f"{field} must be a string"
    if "trigger" in body and body["trigger"] not in {"once", "cron"}:
        return "trigger must be once or cron"
    for field in ("enabled", "authorize_unattended"):
        if field in body and not isinstance(body[field], bool):
            return f"{field} must be a boolean"
    if "payload" in body:
        payload = body["payload"]
        if not isinstance(payload, dict):
            return "payload must be an object"
        for field in ("command", "message", "delivery_text", "delivery_mode"):
            if field in payload and not isinstance(payload[field], str):
                return f"payload.{field} must be a string"
        if "attachment_ids" in payload and (not isinstance(payload["attachment_ids"], list)
                or len(payload["attachment_ids"]) > 10
                or any(not isinstance(value, str) for value in payload["attachment_ids"])):
            return "payload.attachment_ids must be a list of at most 10 strings"
        if "await_attachment" in payload and not isinstance(payload["await_attachment"], bool):
            return "payload.await_attachment must be a boolean"
        history = payload.get("report_history_path")
        if history is not None:
            from pathlib import PurePosixPath

            if (not isinstance(history, str) or not history.endswith(".json") or
                    PurePosixPath(history).is_absolute() or ".." in PurePosixPath(history).parts):
                return "payload.report_history_path must be a relative JSON path inside the workspace"
    return ""


def _timing_fields(body: dict, current=None) -> dict:
    if not any(key in body for key in ("at", "cron_expr", "trigger", "timezone")):
        return {}
    once = body.get("trigger", getattr(getattr(current, "trigger", None), "value", "cron")) == "once" or bool(body.get("at"))
    params = {"timezone": body.get("timezone", getattr(current, "timezone", "")), "recurring": True}
    if once:
        params["at"] = body.get("at")
        if not params["at"]:
            if current is None or not current.at_ms:
                raise ValueError("at is required for a single reminder")
            # An unrelated rename of a completed one-shot need not move its time.
            return {}
    else:
        params["schedule"] = body.get("cron_expr", getattr(current, "cron_expr", ""))
        if not params["schedule"]:
            raise ValueError("cron_expr is required")
    if current is not None and once and params.get("at"):
        from datetime import datetime
        moment = datetime.fromisoformat(params["at"].replace("Z", "+00:00"))
        if moment.tzinfo is not None and abs(moment.timestamp() * 1000 - current.at_ms) < 1000:
            return {}
    timing = schedule_timing(params)
    if current is not None and not once and current.trigger.value == "cron":
        # A regular cron edit keeps unrelated trigger settings intact.
        return {key: value for key, value in timing.items() if key == "cron_expr" or key == "timezone" and "timezone" in body}
    return timing


def _merge_payload(current: object, incoming: dict) -> dict:
    """Merge an update into the stored payload instead of replacing it.

    A PUT used to swap ``job.payload`` wholesale, so any key the client did not
    resend was silently dropped. Clients only send the fields they are editing,
    so an unrelated rename quietly discarded ``is_group``, extension metadata and
    the inspection-tick marker — state the fire-time path relies on and the
    caller never intended to touch.

    Payload keys also feed the authorization fingerprint, so replacing rather
    than merging would change the job's content behind the caller's back and
    invalidate a grant for reasons unrelated to the actual edit.

    ``command`` and ``message`` are treated as one logical slot: they are
    alternative spellings of the instruction, so accepting a new value for one
    must clear the other. Merging them independently would leave the previous
    instruction behind, and the fire-time path (which prefers ``command``) could
    then run text the user believed they had replaced.
    """
    merged = dict(current) if isinstance(current, dict) else {}
    merged.update(incoming)
    if merged.get("delivery_mode") == "reminder" and ("command" in incoming or "delivery_text" in incoming):
        text = incoming.get("delivery_text", incoming.get("command"))
        merged.update(command=text, delivery_text=text)
    if "command" in incoming and "message" not in incoming:
        merged.pop("message", None)
    elif "message" in incoming and "command" not in incoming:
        merged.pop("command", None)
    return merged


class CronAPI:
    def __init__(self, server: GatewayServer):
        self._server = server

    def _guard_read(self, request: web.Request, action: str) -> web.Response | None:
        """Read-level guard: listing jobs and run history is chat-scope info."""
        guard = self._server._require_api_token(request, action=action)
        return guard if guard is not None else self._unavailable()

    def _guard_write(self, request: web.Request, action: str) -> web.Response | None:
        """Admin guard for every mutation.

        Creating a cron job means scheduling unattended work, and (with the
        authorize_unattended flag) granting it permission to run WRITE/EXEC
        tools with nobody watching. That is admin-tier, not chat-tier: a plain
        API token used to be enough. _require_admin_token also enforces CSRF and
        refuses the ?token= query backdoor, which matters because these are
        state-changing endpoints a cross-site page could otherwise reach. On a
        deployment with no tokens at all it passes through, leaving the Origin /
        CSRF checks as the boundary.
        """
        guard = self._server._require_admin_token(request, action=action)
        return guard if guard is not None else self._unavailable()

    def _unavailable(self) -> web.Response | None:
        if self._scheduler() is None:
            return web.json_response(
                {"error": "scheduler is disabled (scheduler.enabled=false)"}, status=503,
            )
        return None

    def _operator(self, request: web.Request) -> str:
        """Who to record as having issued a grant.

        A digest rather than the token's first 8 characters: these records are
        served by GET /cron to any read-scope caller, while cron mutations are
        admin-only — so the plain prefix leaked part of an admin credential to a
        lower privilege level (and a short token leaked entirely)."""
        token = self._server.auth.token_from_headers(request.headers)
        return self._server.auth.token_identifier(token) or "local-no-auth"

    def _scheduler(self):
        return self._server._agent_loop.scheduler

    def _validate_reminder(self, payload: dict) -> str:
        if payload.get("delivery_mode") != "reminder":
            return ""
        if not str(payload.get("delivery_text") or "").strip():
            return "A fixed reminder requires non-empty delivery_text"
        source = str(payload.get("source_session_key") or payload.get("session_key") or "")
        from echo_agent.scheduler.delivery import target_from_session_key
        channel, chat = target_from_session_key(source)
        if not (payload.get("deliver_channel") or payload.get("channel") or channel) or not (
                payload.get("deliver_chat_id") or payload.get("chat_id") or chat):
            return "A fixed reminder requires a delivery target"
        try:
            from echo_agent.attachments import AttachmentStore
            for attachment_id in payload.get("attachment_ids", []):
                AttachmentStore(self._server._workspace).get(source, attachment_id)
        except (ValueError, OSError, AttributeError) as exc:
            return str(exc)
        manager = getattr(self._server, "channel_manager", None)
        if manager is not None and (payload.get("attachment_ids") or payload.get("await_attachment")):
            adapter = manager.get_channel(payload.get("deliver_channel") or payload.get("channel") or channel)
            if adapter is None or not getattr(adapter, "supports_files", False):
                return "目标渠道不支持原始附件投递，请选择支持文件的渠道或仅设置文字提醒。"
        if payload.get("await_attachment"):
            import time
            payload.setdefault("attachment_window_start", time.time() - 600)
            payload.setdefault("attachment_window_end", time.time() + 600)
        return ""

    async def list_jobs(self, request: web.Request) -> web.Response:
        guard = self._guard_read(request, "cron_list")
        if guard is not None:
            return guard

        jobs = self._scheduler().list_jobs()
        return web.json_response(
            {
                "jobs": [self._job_to_dict(j) for j in jobs],
                "total": len(jobs),
            }
        )

    async def create_job(self, request: web.Request) -> web.Response:
        guard = self._guard_write(request, "cron_create")
        if guard is not None:
            return guard

        try:
            body = await request.json()
        except Exception:
            return web.json_response({"error": "invalid JSON body"}, status=400)
        if not isinstance(body, dict):
            return web.json_response({"error": "JSON body must be an object"}, status=400)
        if error := _validate_job_fields(body):
            return web.json_response({"error": error}, status=400)

        name = body.get("name", "")
        try:
            timing = _timing_fields(body)
            if not timing:
                raise ValueError("cron_expr is required, or provide at for a single reminder")
        except (ValueError, KeyError, TypeError) as exc:
            return web.json_response({"error": f"invalid cron_expr or at: {exc}"}, status=400)

        payload = body.get("payload", {})
        if error := self._validate_reminder(payload):
            return web.json_response({"error": error}, status=400)
        if not _payload_has_content(payload):
            return web.json_response(
                {"error": "payload must contain a non-empty 'command' or 'message'"},
                status=400,
            )

        job = ScheduledJob(
            name=name,
            payload=payload,
            **timing,
        )
        # Unattended WRITE/EXEC permission is opt-in and explicit. Absence is
        # not consent: the caller must say so in this request, having been shown
        # what the job will run. Anything else recreates the old hole where
        # merely creating a job granted it privileged unattended execution.
        if body.get("authorize_unattended") is True:
            job.authorization = grant_authorization(
                job,
                operator=self._operator(request),
                source="rest",
            )
        created = self._scheduler().add_job(job)
        return web.json_response({"id": created.id}, status=201)

    async def update_job(self, request: web.Request) -> web.Response:
        guard = self._guard_write(request, "cron_update")
        if guard is not None:
            return guard

        job_id = request.match_info["id"]
        try:
            body = await request.json()
        except Exception:
            return web.json_response({"error": "invalid JSON body"}, status=400)
        if not isinstance(body, dict):
            return web.json_response({"error": "JSON body must be an object"}, status=400)
        if error := _validate_job_fields(body):
            return web.json_response({"error": error}, status=400)

        job = self._scheduler().get_job(job_id)
        if not job:
            return web.json_response({"error": "not found"}, status=404)

        try:
            timing = _timing_fields(body, job)
        except (ValueError, KeyError, TypeError) as exc:
            return web.json_response({"error": f"invalid cron_expr or at: {exc}"}, status=400)
        if job.active_run_id and (timing or "payload" in body):
            return web.json_response({"error": "Cannot update a running scheduled task"}, status=409)

        # Same content guard as create_job: a PUT must not restore the empty-payload
        # state create_job now rejects (would re-open the fire-time ValueError hole).
        # Checked against the MERGED payload, not the request body: an update that
        # only touches, say, deliver_channel legitimately omits the instruction and
        # inherits the stored one, so validating the body alone would reject it.
        merged = None
        if "payload" in body:
            if not isinstance(body["payload"], dict):
                return web.json_response({"error": "payload must be an object"}, status=400)
            merged = _merge_payload(job.payload, body["payload"])
            if error := self._validate_reminder(merged):
                return web.json_response({"error": error}, status=400)
            if not _payload_has_content(merged):
                return web.json_response(
                    {"error": "payload must contain a non-empty 'command' or 'message'"},
                    status=400,
                )

        # Snapshot what the grant is bound to BEFORE the edit lands, so the
        # decision below can compare against what it becomes.
        fingerprint_before = compute_fingerprint(job)

        # Scheduler owns the mutation so cron_expr / re-enable also recompute
        # next_run_ms; assigning here and calling save_state() left the job
        # pointing at an occurrence of the previous expression.
        updated = self._scheduler().update_job(
            job_id,
            name=body.get("name") if "name" in body else None,
            enabled=body.get("enabled") if "enabled" in body else None,
            payload=merged,
            **timing,
        )
        if updated is None:
            return web.json_response({"error": "not found"}, status=404)

        # Re-grant or revoke in a second pass, never leave a stale grant in place.
        # The grant must be fingerprinted against the job as it now stands: signing
        # before the edit lands would bind it to the old cron_expr / timezone, so an
        # explicit "authorize" alongside a schedule change would store a grant that
        # is already invalid. Writing None when the caller did not re-authorize is
        # what makes the UI able to say "需要重新授权" instead of displaying a grant
        # that silently no longer applies.
        #
        # Revoking is scoped to edits that actually CHANGED the fingerprint,
        # compared before/after the mutation. Doing it for ANY authorization-less
        # PUT silently killed grants on requests that change nothing a human
        # consented to: the dashboard's pause/resume toggle sends {"enabled": ...}
        # alone, so pausing an authorized job and resuming it a week later left it
        # firing with WRITE/EXEC denied, at night, with no warning in the UI.
        #
        # Keying off "which keys appear in the request" instead was still wrong,
        # just less often: the dashboard form resends cron_expr and the full
        # payload on every submit, so renaming a job revoked its grant even though
        # nothing it binds had moved. Any client that PUTs a whole object — the
        # normal REST shape — hit the same thing. Comparing the fingerprint itself
        # makes "editing content revokes, renaming does not" a property of the
        # data rather than a convention each caller has to observe.
        if body.get("authorize_unattended") is True:
            authorization = grant_authorization(
                updated,
                operator=self._operator(request),
                source="rest",
            )
        elif compute_fingerprint(updated) != fingerprint_before:
            authorization = None
        else:
            # Nothing fingerprinted changed and no new consent was given: pass
            # neither argument so update_job's set_authorization stays False and
            # the stored grant is left untouched.
            return web.json_response({"job": self._job_to_dict(updated)})
        updated = self._scheduler().update_job(job_id, authorization=authorization, set_authorization=True) or updated
        return web.json_response({"job": self._job_to_dict(updated)})

    async def delete_job(self, request: web.Request) -> web.Response:
        guard = self._guard_write(request, "cron_delete")
        if guard is not None:
            return guard

        job_id = request.match_info["id"]
        success = self._scheduler().remove_job(job_id)
        if not success:
            return web.json_response({"error": "not found"}, status=404)
        return web.json_response({"status": "deleted"})

    async def trigger_job(self, request: web.Request) -> web.Response:
        guard = self._guard_write(request, "cron_trigger")
        if guard is not None:
            return guard

        job_id = request.match_info["id"]
        scheduler = self._scheduler()
        job = scheduler.get_job(job_id)
        if job is None:
            return web.json_response({"error": "not found"}, status=404)
        if job.active_run_id:
            return web.json_response(
                {"error": "job already has an active run; inspect run history before triggering again",
                 "active_run_id": job.active_run_id}, status=409,
            )
        if not job.enabled or job.status != JobStatus.ACTIVE:
            return web.json_response(
                {"error": "job is disabled, paused or terminal; enable an active job before triggering"}, status=409,
            )
        success = await scheduler.trigger_job(job_id)
        if not success:
            # State can change while another trigger acquires the run lock.
            if scheduler.get_job(job_id) is None:
                return web.json_response({"error": "not found"}, status=404)
            if job.last_status == "error" and not job.active_run_id:
                return web.json_response(
                    {"error": "job dispatch failed; inspect run history and scheduler storage before retrying",
                     "code": "dispatch_failed"}, status=503,
                )
            capacity = getattr(scheduler, "_max_concurrent", None)
            if isinstance(capacity, int) and sum(bool(item.active_run_id) for item in scheduler.list_jobs()) >= capacity:
                return web.json_response(
                    {"error": "scheduler execution capacity is full; wait for an active run to finish",
                     "code": "capacity_exhausted"}, status=409,
                )
            return web.json_response(
                {"error": "job was not dispatched; another run or scheduler lock is active; inspect run history",
                 "code": "run_conflict"}, status=409,
            )
        return web.json_response({"status": "triggered"})

    async def get_runs(self, request: web.Request) -> web.Response:
        guard = self._guard_read(request, "cron_runs")
        if guard is not None:
            return guard

        job_id = request.match_info["id"]
        try:
            limit = int(request.query.get("limit", "10"))
        except (ValueError, TypeError):
            return web.json_response({"error": "invalid limit parameter"}, status=400)
        if not 1 <= limit <= 100:
            return web.json_response({"error": "limit must be between 1 and 100"}, status=400)
        runs = self._scheduler().get_run_history(job_id, limit=limit)
        if self._scheduler().get_job(job_id) is None:
            return web.json_response({"error": "not found"}, status=404)
        return web.json_response({"runs": runs})

    async def download_report(self, request: web.Request) -> web.StreamResponse:
        guard = self._guard_write(request, "cron_report_download")
        if guard is not None:
            return guard
        job = self._scheduler().get_job(request.match_info["id"])
        run = next((r for r in job.run_history if r.get("run_id") == request.match_info["run_id"]), None) if job else None
        from echo_agent.gateway.api.deliveries import DeliveriesAPI
        from urllib.parse import quote

        path = DeliveriesAPI(self._server)._local_file(run.get("report_file", "")) if run and run.get("report_file") else None
        if path is None:
            return web.json_response({"error": "saved report is unavailable"}, status=404)
        return web.FileResponse(path, headers={"Content-Disposition": f"attachment; filename*=UTF-8''{quote(path.name)}",
                                               "Cache-Control": "no-store", "X-Content-Type-Options": "nosniff"})

    def _job_to_dict(self, job: ScheduledJob) -> dict:
        from echo_agent.attachments import AttachmentStore
        details = []
        if job.payload.get("attachment_ids"):
            store = AttachmentStore(self._server._workspace)
            for attachment_id in job.payload["attachment_ids"]:
                try:
                    record = store.get(job.payload.get("source_session_key", ""), attachment_id)
                    details.append({"id": attachment_id, "name": record["name"], "available": True})
                except (ValueError, OSError):
                    details.append({"id": attachment_id, "name": "", "available": False})
        return {
            "id": job.id,
            "name": job.name,
            "trigger": job.trigger.value,
            "cron_expr": job.cron_expr,
            "at_ms": job.at_ms,
            "timezone": job.timezone,
            "enabled": job.enabled,
            "status": job.status.value,
            "last_run_ms": job.last_run_ms,
            "next_run_ms": job.next_run_ms,
            "last_status": job.last_status,
            "last_error": job.last_error,
            "delivery_status": job.run_history[-1].get("delivery_status", "") if job.run_history else "",
            "active_run_id": job.active_run_id,
            "payload": job.payload,
            "attachment_details": details,
            "config_valid": _payload_has_content(job.payload),
            # Two separate facts: what grant exists (audit trail) and whether it
            # still applies (fingerprint check). A job can have the former
            # without the latter — that is the "需要重新授权" state the UI shows.
            "authorization": job.authorization.to_dict() if job.authorization else None,
            "authorization_valid": verify_authorization(job),
        }
