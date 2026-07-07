# echo_agent/gateway/api/cron_api.py
from __future__ import annotations

from typing import TYPE_CHECKING

from aiohttp import web

from echo_agent.scheduler.service import ScheduledJob, TriggerKind

if TYPE_CHECKING:
    from echo_agent.gateway.server import GatewayServer


class CronAPI:
    def __init__(self, server: GatewayServer):
        self._server = server

    def _guard(self, request: web.Request, action: str) -> web.Response | None:
        return self._server._require_api_token(request, action=action)

    def _scheduler(self):
        return self._server._agent_loop.scheduler

    async def list_jobs(self, request: web.Request) -> web.Response:
        guard = self._guard(request, "cron_list")
        if guard is not None:
            return guard

        jobs = self._scheduler().list_jobs()
        return web.json_response({
            "jobs": [self._job_to_dict(j) for j in jobs],
            "total": len(jobs),
        })

    async def create_job(self, request: web.Request) -> web.Response:
        guard = self._guard(request, "cron_create")
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

        job = ScheduledJob(
            name=name,
            trigger=TriggerKind.CRON,
            cron_expr=cron_expr,
            payload=body.get("payload", {}),
        )
        created = self._scheduler().add_job(job)
        return web.json_response({"id": created.id}, status=201)

    async def update_job(self, request: web.Request) -> web.Response:
        guard = self._guard(request, "cron_update")
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

        if "name" in body:
            job.name = body["name"]
        if "cron_expr" in body:
            job.cron_expr = body["cron_expr"]
        if "enabled" in body:
            job.enabled = body["enabled"]
        if "payload" in body:
            job.payload = body["payload"]

        self._scheduler().save_state()
        return web.json_response({"job": self._job_to_dict(job)})

    async def delete_job(self, request: web.Request) -> web.Response:
        guard = self._guard(request, "cron_delete")
        if guard is not None:
            return guard

        job_id = request.match_info["id"]
        success = self._scheduler().remove_job(job_id)
        if not success:
            return web.json_response({"error": "not found"}, status=404)
        return web.json_response({"status": "deleted"})

    async def trigger_job(self, request: web.Request) -> web.Response:
        guard = self._guard(request, "cron_trigger")
        if guard is not None:
            return guard

        job_id = request.match_info["id"]
        success = await self._scheduler().trigger_job(job_id)
        if not success:
            return web.json_response({"error": "not found"}, status=404)
        return web.json_response({"status": "triggered"})

    async def get_runs(self, request: web.Request) -> web.Response:
        guard = self._guard(request, "cron_runs")
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
        }
