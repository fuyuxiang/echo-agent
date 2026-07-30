"""Cronjob tool — create, list, and delete scheduled tasks."""

from __future__ import annotations

from typing import Any

from echo_agent.agent.approval_gate import APPROVAL_SOURCE_HUMAN
from echo_agent.agent.tools.base import Tool, ToolExecutionContext, ToolResult
from echo_agent.scheduler.authorization import grant as grant_authorization
from echo_agent.scheduler.delivery import target_from_session_key
from echo_agent.scheduler.service import ScheduledJob, Scheduler, TriggerKind


class CronjobTool(Tool):
    name = "cronjob"
    # Lists exactly the actions the enum accepts. "update" used to be advertised
    # here with no enum value and no implementation behind it, so the model kept
    # attempting edits that came back as "Unknown action".
    description = "Manage scheduled tasks: create, list, delete, or trigger cron-based jobs."
    risk_level = "dangerous"
    parameters = {
        "type": "object",
        "properties": {
            "action": {"type": "string", "enum": ["create", "list", "delete", "trigger"], "description": "Action to perform."},
            "name": {"type": "string", "description": "Job name (for create/delete/trigger)."},
            "schedule": {"type": "string", "description": "Cron expression (for create), e.g., '*/5 * * * *'."},
            "command": {"type": "string", "description": "Command or message to execute on schedule (for create)."},
            "job_id": {"type": "string", "description": "Job ID (for delete/trigger)."},
            "target_channel": {"type": "string", "description": "Optional delivery channel. Defaults to the current chat."},
            "target_chat_id": {"type": "string", "description": "Optional delivery chat id. Defaults to the current chat."},
        },
        "required": ["action"],
    }

    def __init__(self, scheduler: Scheduler | None):
        self._scheduler = scheduler

    async def execute(self, params: dict[str, Any], ctx: ToolExecutionContext | None = None) -> ToolResult:
        if not self._scheduler:
            return ToolResult(success=False, error="Scheduler not enabled")

        action = params["action"]

        if action == "create":
            name = params.get("name", "unnamed")
            schedule = params.get("schedule", "")
            command = params.get("command", "")
            if not schedule or not command:
                return ToolResult(success=False, error="Both 'schedule' and 'command' are required for create")
            source_session_key = ctx.session_key if ctx else ""
            default_channel, default_chat_id = target_from_session_key(source_session_key)
            target_channel = params.get("target_channel", "") or default_channel
            target_chat_id = params.get("target_chat_id", "") or default_chat_id
            payload = {
                "command": command,
                "source_session_key": source_session_key,
                "deliver_channel": target_channel,
                "deliver_chat_id": target_chat_id,
            }
            # Validate the expression before anything is persisted. An
            # unparseable expression used to be accepted: the job was stored and
            # even granted an authorization, but _compute_next_run returned None
            # so it never fired — a "created successfully" that silently does
            # nothing. Same croniter check the REST endpoint already applies.
            try:
                from croniter import croniter
                croniter(schedule)
            except (ValueError, KeyError, TypeError) as e:
                return ToolResult(
                    success=False,
                    error=f"Invalid cron expression '{schedule}': {e}",
                    error_kind="validation",
                )
            job = ScheduledJob(
                name=name,
                trigger=TriggerKind.CRON,
                cron_expr=schedule,
                payload=payload,
            )
            # Unattended WRITE/EXEC consent is only issued when a person actually
            # approved THIS call. The tool is risk_level="dangerous", but that
            # alone never guaranteed a prompt: with the shipped defaults
            # (profile=personal_cli, cli_auto_approve=True) the gate auto-approved
            # every risk level on cli channels, so this signed a grant labelled
            # "tui-approval" that no human had seen. approved_actions cannot tell
            # the two apart — both hold {"cronjob"} — hence approval_source.
            #
            # Without a grant the job still fires; only its privileged tool calls
            # are denied. So the fallback is a working reminder-style job plus a
            # pointer to the explicit authorization commands, not a failure.
            human_approved = bool(ctx and ctx.approval_source == APPROVAL_SOURCE_HUMAN)
            if human_approved:
                job.authorization = grant_authorization(
                    job,
                    operator=(ctx.user_id if ctx and ctx.user_id else "agent-approval"),
                    source="tui-approval",
                )
            created = self._scheduler.add_job(job)
            out = f"Created job '{name}' (id={created.id}): {schedule}"
            if not human_approved:
                out += (
                    "\n提示：该任务未获得无人值守授权，触发时无法执行写文件/命令等特权操作。"
                    f"如需放开，请运行 `echo-agent cron authorize {created.id}` "
                    "或在 Dashboard 的定时任务页勾选授权。"
                )
            if not target_channel or not target_chat_id:
                # No resolvable delivery target: the job will run but any user-
                # facing output falls back to the cron pseudo-channel and is
                # dropped. Surface this instead of silently creating a job whose
                # results the user will never receive.
                out += (
                    "\n⚠️ 警告：无法确定投递目标(缺少 target_channel/target_chat_id，"
                    "且当前会话无法推导)。该任务会按时执行,但产出不会发送给任何人。"
                    "如需收到结果,请在创建时指定 target_channel 和 target_chat_id。"
                )
            return ToolResult(output=out, metadata={"job_id": created.id})

        if action == "list":
            jobs = self._scheduler.list_jobs()
            if not jobs:
                return ToolResult(output="No scheduled jobs.")
            lines = []
            for j in jobs:
                payload = j.payload.get("command", "") if isinstance(j.payload, dict) else str(j.payload)
                schedule = j.cron_expr or str(j.interval_ms) or str(j.at_ms)
                lines.append(f"{j.id}: [{schedule}] {j.name} — {payload[:60]}")
            return ToolResult(output="\n".join(lines))

        if action == "delete":
            job_id = params.get("job_id", "")
            if not job_id:
                return ToolResult(success=False, error="'job_id' required for delete")
            removed = self._scheduler.remove_job(job_id)
            if removed:
                return ToolResult(output=f"Deleted job {job_id}")
            return ToolResult(success=False, error=f"Job '{job_id}' not found")

        if action == "trigger":
            job_id = params.get("job_id", "")
            if not job_id:
                return ToolResult(success=False, error="'job_id' required for trigger")
            triggered = await self._scheduler.trigger_job(job_id)
            if triggered:
                return ToolResult(output=f"Triggered job {job_id}")
            return ToolResult(success=False, error=f"Job '{job_id}' not found or failed to trigger")

        return ToolResult(success=False, error=f"Unknown action: {action}")
