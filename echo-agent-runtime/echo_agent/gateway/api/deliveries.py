"""Operator recovery for partial and uncertain native-channel deliveries."""
from __future__ import annotations

from aiohttp import web


class DeliveriesAPI:
    def __init__(self, server):
        self._server = server

    def _store(self):
        return getattr(self._server._agent_loop, "_turn_runs", None)

    async def message_detail(self, request: web.Request) -> web.Response:
        guard = self._server._require_admin_token(request, action="deliveries_detail")
        if guard is not None:
            return guard
        store = self._store()
        event = await store.get_outbound_event(request.match_info["id"]) if store else None
        if event is None:
            return web.json_response({"error": "delivery record not found"}, status=404)
        from pathlib import Path
        from urllib.parse import urlsplit
        return web.json_response({"text": event.text,
                                  "files": [Path(urlsplit(block.url).path).name for block in event.content if block.url],
                                  "downloadable_files": [index for index, block in enumerate(b for b in event.content if b.url)
                                                         if self._local_file(block.url) is not None]})

    def _local_file(self, url):
        from pathlib import Path
        from urllib.parse import urlsplit

        root = getattr(self._server, "_workspace", None)
        if not isinstance(root, (str, Path)) or urlsplit(url).scheme:
            return None
        root = Path(root).resolve()
        path = Path(url)
        path = (root / path).resolve()
        return path if path.is_relative_to(root) and path.is_file() else None

    async def download_file(self, request: web.Request) -> web.StreamResponse:
        guard = self._server._require_admin_token(request, action="deliveries_download")
        if guard is not None:
            return guard
        store = self._store()
        event = await store.get_outbound_event(request.match_info["id"]) if store else None
        try:
            index = int(request.match_info["index"])
        except (ValueError, TypeError):
            index = -1
        blocks = [block for block in event.content if block.url] if event else []
        path = self._local_file(blocks[index].url) if 0 <= index < len(blocks) else None
        if path is None:
            return web.json_response({"error": "saved report is unavailable"}, status=404)
        from urllib.parse import quote

        return web.FileResponse(path, headers={"Content-Disposition": f"attachment; filename*=UTF-8''{quote(path.name)}",
                                               "Cache-Control": "no-store", "X-Content-Type-Options": "nosniff"})

    async def list_messages(self, request: web.Request) -> web.Response:
        guard = self._server._require_admin_token(request, action="deliveries_list")
        if guard is not None:
            return guard
        store = self._store()
        if store is None:
            return web.json_response({"messages": [], "durable": False})
        rows = await store.list_outbound()
        messages = []
        for row in rows:
            payload = row["payload"]
            metadata = payload.get("metadata", {})
            if request.query.get("scheduled") == "true" and not metadata.get("scheduled_output"):
                continue
            job_filter = request.query.get("job_id")
            if isinstance(job_filter, str) and job_filter and metadata.get("job_id") != job_filter:
                continue
            manager = getattr(self._server, "channel_manager", None)
            channel = manager.get_channel(payload["channel"]) if manager else None
            readiness = getattr(channel, "delivery_readiness", None)
            issue = readiness(payload["chat_id"]) if callable(readiness) else None
            detail = row.get("detail") or {}
            preview = "\n".join(block.get("text", "") for block in payload["content"] if block.get("text"))
            messages.append({
                "event_id": row["event_id"], "channel": payload["channel"], "chat_id": payload["chat_id"],
                "status": row["status"], "preview": preview[:500],
                "attachments": sum(bool(block.get("url")) for block in payload["content"]),
                "confirmed_parts": sum(bool(part["message_id"]) for part in row["parts"]),
                "unknown_parts": sum(not part["message_id"] and part["state"] == "unknown" for part in row["parts"]),
                "created_at": row["created_at"],
                "error": row.get("error", ""),
                "error_code": issue.error_code if issue else detail.get("error_code", ""),
                "recovery_action": issue.recovery_action if issue else "",
                "can_retry": issue is None and channel is not None and channel.is_running,
                "auto_retry": bool(metadata.get("scheduled_output") and row["status"] == "awaiting_context"),
                "job_id": metadata.get("job_id", ""),
            })
        return web.json_response({"messages": messages, "durable": True})

    async def recover(self, request: web.Request) -> web.Response:
        guard = self._server._require_admin_token(request, action="deliveries_recover")
        if guard is not None:
            return guard
        store = self._store()
        if store is None:
            return web.json_response({"error": "durable delivery storage is unavailable"}, status=409)
        try:
            body = await request.json()
        except (ValueError, TypeError):
            return web.json_response({"error": "invalid JSON request"}, status=400)
        if not isinstance(body, dict) or body.get("action") not in {"retry", "confirm_delivery"}:
            return web.json_response({"error": "choose retry or confirm_delivery"}, status=400)
        event_id = request.match_info["id"]
        event = await store.get_outbound_event(event_id)
        if event is None:
            return web.json_response({"error": "delivery record not found"}, status=404)
        state = event.metadata.get("_clarification")
        loop = self._server._agent_loop
        if event.message_kind == "clarification" and isinstance(state, dict):
            # Match AgentLoop's lock order: session, then transport. This also
            # prevents an old recovered question from replacing a newer one.
            session_lock = await loop.sessions.acquire(state["session_key"])
            async with session_lock:
                if not await loop.can_recover_clarification(event):
                    return web.json_response({"error": "this question has been superseded or its task has ended"}, status=409)
                return await self._recover_event(request, store, event, body, clarification_locked=True)
        return await self._recover_event(request, store, event, body)

    async def _recover_event(self, request, store, event, body, *, clarification_locked=False):
        event_id = event.event_id
        bus = self._server._bus
        actor = "operator"
        auth = getattr(self._server, "auth", None)
        if auth is not None:
            token = self._server._request_token(request)
            actor = auth.principal_for_token(token) or "local-operator"
        lock = bus.delivery_lock(f"{event.channel}:{event.chat_id}")
        if lock.locked():
            return web.json_response({"error": "this chat is still sending; wait for it to finish"}, status=409)
        async with lock:
            rows = await store._storage.fetch_sql("SELECT status FROM outbound_messages WHERE event_id=?", (event_id,))
            if rows[0]["status"] in {"delivered", "retrying"}:
                return web.json_response({"error": "delivery is already completed or being retried"}, status=409)
            try:
                if body["action"] == "confirm_delivery":
                    if body.get("confirmed_delivered") is not True:
                        return web.json_response({"error": "confirm that the recipient received the entire message"}, status=400)
                    receipts = {part["part_id"]: f"operator-confirmed:{part['part_id']}"
                                for part in await store.outbound_parts(event_id, event.channel) if not part["message_id"]}
                    await store.resolve_outbound(event_id, message_ids=receipts, actor=actor)
                    await store.update_outbound(event_id, "delivered")
                else:
                    channel = self._server.channel_manager.get_channel(event.channel)
                    if channel is None or not channel.is_running:
                        return web.json_response({"error": "start this channel before retrying"}, status=409)
                    readiness = getattr(channel, "delivery_readiness", None)
                    issue = readiness(event.chat_id) if callable(readiness) else None
                    if issue is not None:
                        return web.json_response({"error": issue.error, "error_code": issue.error_code,
                                                  "recovery_action": issue.recovery_action}, status=409)
                    await store.resolve_outbound(event_id,
                                                 confirmed_not_delivered=body.get("confirmed_not_delivered") is True, actor=actor)
                    await store.update_outbound(event_id, "retrying")
            except ValueError as exc:
                return web.json_response({"error": str(exc)}, status=409)
        # The adapter owns the chat lock while sending; do not hold it twice.
        if body["action"] == "confirm_delivery":
            from echo_agent.bus.delivery import DeliveryResult, DeliveryStage
            result = DeliveryResult(DeliveryStage.DELIVERED, event.channel, detail={"operator_confirmed": True})
            if bus.scheduled_outputs is not None:
                await bus.scheduled_outputs.record(event, result)
            callback = getattr(self._server._agent_loop, "record_recovered_delivery", None)
            if callback is not None:
                await callback(event, result)
            await self._server._agent_loop.restore_delivered_clarification(event, already_locked=clarification_locked)
            return web.json_response({"success": True, "status": "delivered"})
        event.metadata["_delivery_retry"] = True
        bus.track_outbound(event.event_id)
        if bus.scheduled_outputs is not None and event.metadata.get("scheduled_output"):
            result = await bus.scheduled_outputs.retry(event)
        else:
            result = await bus.publish_outbound(event)
        callback = getattr(self._server._agent_loop, "record_recovered_delivery", None)
        if callback is not None:
            await callback(event, result)
        if not result.ok:
            return web.json_response({"error": result.error or "delivery failed", "status": result.stage.value,
                                      "delivery_unknown": result.detail.get("delivery_unknown", False)}, status=502)
        await self._server._agent_loop.restore_delivered_clarification(event, already_locked=clarification_locked)
        return web.json_response({"success": True, "status": result.stage.value})
