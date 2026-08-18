from __future__ import annotations

from typing import TYPE_CHECKING

from aiohttp import web

from echo_agent.scheduler.authorization import compute_fingerprint
from echo_agent.scheduler.authorization import grant as grant_authorization
from echo_agent.scheduler.authorization import verify as verify_authorization
from echo_agent.scheduler.service import ScheduledJob, TriggerKind

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
        return self._server._require_api_token(request, action=action)

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
        return self._server._require_admin_token(request, action=action)

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

    async def list_jobs(self, request: web.Request) -> web.Response:
        guard = self._guard_read(request, "cron_list")
        if guard is not None:
            return guard

        jobs = self._scheduler().list_jobs()
        return web.json_response({
            "jobs": [self._job_to_dict(j) for j in jobs],
            "total": len(jobs),
        })

    async def create_job(self, request: web.Request) -> web.Response:
        guard = self._guard_write(request, "cron_create")
        if guard is not None:
            return guard

        try:
            body = await request.json()
        except Exception:
            return web.json_response({"error": "invalid JSON body"}, status=400)

        name = body.get("name", "")
        cron_expr = body.get("cron_expr", "")
        if not cron_expr:
            return web.json_response({"error": "cron_expr is required"}, status=400)

        try:
            from croniter import croniter
            croniter(cron_expr)
        except (ValueError, KeyError, TypeError) as e:
            return web.json_response({"error": f"invalid cron_expr: {e}"}, status=400)

        payload = body.get("payload", {})
        if not _payload_has_content(payload):
            return web.json_response(
                {"error": "payload must contain a non-empty 'command' or 'message'"},
                status=400,
            )

        job = ScheduledJob(
            name=name,
            trigger=TriggerKind.CRON,
            cron_expr=cron_expr,
            payload=payload,
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

        job = self._scheduler().get_job(job_id)
        if not job:
            return web.json_response({"error": "not found"}, status=404)

        if "cron_expr" in body:
            try:
                from croniter import croniter
                croniter(body["cron_expr"])
            except (ValueError, KeyError, TypeError) as e:
                return web.json_response({"error": f"invalid cron_expr: {e}"}, status=400)

        # Same content guard as create_job: a PUT must not restore the empty-payload
        # state create_job now rejects (would re-open the fire-time ValueError hole).
        # Checked against the MERGED payload, not the request body: an update that
        # only touches, say, deliver_channel legitimately omits the instruction and
        # inherits the stored one, so validating the body alone would reject it.
        merged = None
        if "payload" in body:
            if not isinstance(body["payload"], dict):
                return web.json_response(
                    {"error": "payload must be an object"}, status=400
                )
            merged = _merge_payload(job.payload, body["payload"])
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
            cron_expr=body.get("cron_expr") if "cron_expr" in body else None,
            enabled=body.get("enabled") if "enabled" in body else None,
            payload=merged,
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
        updated = self._scheduler().update_job(
            job_id, authorization=authorization, set_authorization=True
        ) or updated
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
        success = await self._scheduler().trigger_job(job_id)
        if not success:
            return web.json_response({"error": "not found"}, status=404)
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
        runs = self._scheduler().get_run_history(job_id, limit=limit)
        return web.json_response({"runs": runs})

    def _job_to_dict(self, job: ScheduledJob) -> dict:
        return {
            "id": job.id,
            "name": job.name,
            "trigger": job.trigger.value,
            "cron_expr": job.cron_expr,
            "enabled": job.enabled,
            "status": job.status.value,
            "last_run_ms": job.last_run_ms,
            "next_run_ms": job.next_run_ms,
            "last_status": job.last_status,
            "payload": job.payload,
            "config_valid": _payload_has_content(job.payload),
            # Two separate facts: what grant exists (audit trail) and whether it
            # still applies (fingerprint check). A job can have the former
            # without the latter — that is the "需要重新授权" state the UI shows.
            "authorization": job.authorization.to_dict() if job.authorization else None,
            "authorization_valid": verify_authorization(job),
        }
