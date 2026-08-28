"""Cronjob tool — create, list, delete, trigger and authorize scheduled tasks.

This is the only surface where a scheduled job's unattended authorization can be
managed from inside a conversation. The alternatives are unreachable there: the
CLI refuses while the gateway holds the workspace instance lock, and REST /
Dashboard need a browser or a token. So `authorize` / `revoke` are not
conveniences duplicating those — they are the sole chat-side path, and for a
grant that a routine edit invalidates by design, that path has to exist.
"""

from __future__ import annotations

from typing import Any

from echo_agent.agent.approval_gate import APPROVAL_SOURCE_HUMAN
from echo_agent.tools.base import Tool, ToolExecutionContext, ToolResult
from echo_agent.scheduler.authorization import consent_facts
from echo_agent.scheduler.authorization import grant as grant_authorization
from echo_agent.scheduler.authorization import verify as verify_authorization
from echo_agent.scheduler.delivery import target_from_session_key
from echo_agent.scheduler.service import ScheduledJob, Scheduler, TriggerKind

# How an unauthorized job gets authorized, in the order a user can actually act
# on. The in-conversation path leads because it is the only one available while
# the agent is running — and any message carrying this text was produced by a
# running agent. `echo-agent cron authorize` is listed last and with its
# precondition attached: cron_cmd refuses outright while the gateway holds the
# instance lock, so advertising it unqualified sent users to a command that was
# guaranteed to fail in the exact situation that produced the advice.
#
# Shared by the tool and the approval gate's unattended denial so the two cannot
# drift into recommending different things.
UNAUTHORIZED_HINT = (
    "如需放开，在对话里说「授权定时任务 {job_id}」即可（回复确认后生效）；"
    "也可在 Dashboard 的定时任务页勾选授权，"
    "或停止服务后运行 `echo-agent cron authorize {job_id}`。"
)


class CronjobTool(Tool):
    name = "cronjob"
    # Lists exactly the actions the enum accepts. "update" used to be advertised
    # here with no enum value and no implementation behind it, so the model kept
    # attempting edits that came back as "Unknown action".
    description = (
        "Manage scheduled tasks: create, list, delete, trigger, authorize, or "
        "revoke authorization for cron-based jobs. Use 'authorize' to let an "
        "existing job run write/command tools unattended."
    )
    risk_level = "dangerous"
    parameters = {
        "type": "object",
        "properties": {
            "action": {"type": "string", "enum": ["create", "list", "delete", "trigger", "authorize", "revoke"], "description": "Action to perform."},
            "name": {"type": "string", "description": "Job name (for create/delete/trigger)."},
            "schedule": {"type": "string", "description": "Cron expression (for create), e.g., '*/5 * * * *'."},
            "command": {"type": "string", "description": "Command or message to execute on schedule (for create)."},
            "job_id": {"type": "string", "description": "Job ID (for delete/trigger/authorize/revoke)."},
            "target_channel": {"type": "string", "description": "Optional delivery channel. Defaults to the current chat."},
            "target_chat_id": {"type": "string", "description": "Optional delivery chat id. Defaults to the current chat."},
        },
        "required": ["action"],
    }

    def __init__(self, scheduler: Scheduler | None):
        self._scheduler = scheduler

    def describe_for_approval(self, arguments: dict[str, Any]) -> str:
        """What the user is consenting to, resolved from the stored job.

        The approval prompt could otherwise only echo the arguments, and for
        every action but `create` those are just an id: "action=authorize,
        job_id=148fb4a4b9" asked someone to let a job run write/command tools
        unattended without telling them what it runs, on what schedule, or who
        receives the output. The CLI has always shown those three before asking
        (cron_cmd._describe) — a prompt on the surface most users actually have
        must clear the same bar, or the confirmation is a formality.

        Returns "" to accept the generic rendering whenever the job cannot be
        resolved. Never raises: the gate treats this as display only.
        """
        action = str(arguments.get("action", "") or "")
        if action == "create":
            # No stored job yet, so the arguments ARE the facts.
            fields = {
                "任务": str(arguments.get("name", "") or "(未命名)"),
                "频率": str(arguments.get("schedule", "") or ""),
                "指令": str(arguments.get("command", "") or ""),
            }
            channel = str(arguments.get("target_channel", "") or "")
            chat_id = str(arguments.get("target_chat_id", "") or "")
            if channel or chat_id:
                fields["投递"] = f"{channel}:{chat_id}"
            return _render_fields("新建定时任务", fields)

        labels = {
            "authorize": "授权定时任务在无人值守下执行写入/命令类工具",
            "revoke": "撤销定时任务的无人值守授权",
            "delete": "删除定时任务",
            "trigger": "立即触发定时任务",
        }
        if action not in labels or self._scheduler is None:
            return ""
        job_id = str(arguments.get("job_id", "") or "")
        if not job_id:
            return ""
        job = self._scheduler.get_job(job_id)
        if job is None:
            return ""

        facts = consent_facts(job)
        fields = {
            "任务": f"{facts['id']}  {facts['name']}".strip(),
            "频率": facts["trigger"],
            "投递": facts["target"],
            "指令": facts["instruction"],
        }
        if action == "authorize":
            # "Never authorized" and "grant invalidated by an edit" look
            # identical to the user otherwise, and they are different decisions:
            # a first grant versus re-consent to changed content.
            if verify_authorization(job):
                fields["当前状态"] = "已授权（重新授权将按当前内容重新签发）"
            elif getattr(job, "authorization", None) is not None:
                fields["当前状态"] = "授权已失效（任务内容被修改过）"
            else:
                fields["当前状态"] = "未授权"
        return _render_fields(labels[action], fields)

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
                    source=_grant_source(ctx),
                )
            created = self._scheduler.add_job(job)
            out = f"Created job '{name}' (id={created.id}): {schedule}"
            if not human_approved:
                out += (
                    "\n提示：该任务未获得无人值守授权，触发时无法执行写文件/命令等特权操作。"
                    + UNAUTHORIZED_HINT.format(job_id=created.id)
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

        if action in ("authorize", "revoke"):
            return self._authorize(action, params, ctx)

        return ToolResult(success=False, error=f"Unknown action: {action}")

    def _authorize(
        self, action: str, params: dict[str, Any], ctx: ToolExecutionContext | None,
    ) -> ToolResult:
        """(Re-)grant or withdraw a job's unattended authorization.

        Before this existed, `create` was the only chat-reachable caller of
        grant() — so consent could only ever be given at the instant a job was
        born. Every later need for it had no path from a conversation: a job
        stored before the authorization contract, a job whose fingerprint an edit
        deliberately invalidated, a grant the dashboard revoked. The CLI refuses
        while the gateway holds the instance lock, and REST/Dashboard are not
        reachable from inside a chat, which left the denial message pointing at
        two things the person reading it could not do.

        revoke is included for the same reason, in the other direction: granting
        from chat while only the CLI can take it back would make the privilege
        one-way for anyone without a terminal.
        """
        # execute() already returned on a missing scheduler; re-checked rather
        # than asserted because `python -O` strips asserts, and the fallback here
        # is a clear message instead of an AttributeError.
        if self._scheduler is None:
            return ToolResult(success=False, error="Scheduler not enabled")
        job_id = str(params.get("job_id", "") or "")
        if not job_id:
            return ToolResult(
                success=False,
                error=f"'job_id' required for {action}",
                error_kind="validation",
            )
        job = self._scheduler.get_job(job_id)
        if job is None:
            return ToolResult(
                success=False,
                error=f"Job '{job_id}' not found",
                error_kind="business",
            )

        if action == "revoke":
            # No human-consent check: withdrawing a privilege is the safe
            # direction, and requiring confirmation to *reduce* access would mean
            # a misfiring job cannot be reined in from the only surface the user
            # has. The gate's DANGEROUS tier still governs reaching this at all.
            self._scheduler.update_job(job_id, authorization=None, set_authorization=True)
            return ToolResult(
                output=f"已撤销任务 {job_id}（{job.name}）的无人值守授权。",
                metadata={"job_id": job_id, "authorized": False},
            )

        # Defense in depth, not the primary control. `cronjob` is DANGEROUS, and
        # the gate routes DANGEROUS to a real prompt even under cli_auto_approve
        # (approval_gate step 7), so reaching here normally means a person
        # answered. This check covers the configurations that can still bypass
        # that — auto_approve listing "cronjob", mode="off", a trusted channel —
        # none of which is a human consenting to hand THIS job standing
        # privileges. Same reasoning, and same failure history, as create's.
        if not (ctx and ctx.approval_source == APPROVAL_SOURCE_HUMAN):
            return ToolResult(
                success=False,
                error=(
                    f"授权任务 {job_id} 需要人工在对话中明确确认，当前调用未经人工批准。"
                    "请让用户直接回复确认后重试。"
                ),
                error_kind="business",
            )

        # Signed against the job as it stands right now, which is what makes this
        # the fix for a stale grant: compute_fingerprint reads the current
        # instruction/schedule/target, so re-consenting binds to what the user
        # was just shown rather than re-blessing older content.
        authorization = grant_authorization(
            job,
            operator=(ctx.user_id if ctx and ctx.user_id else "agent-approval"),
            source=_grant_source(ctx),
        )
        updated = self._scheduler.update_job(
            job_id, authorization=authorization, set_authorization=True,
        )
        if updated is None or not verify_authorization(updated):
            # update_job persists and returns the stored job; a grant that does
            # not verify against it means the store and the signature disagree.
            # Report failure rather than claim success the scheduler will not act
            # on — the user would otherwise wait for a job that stays denied.
            return ToolResult(
                success=False,
                error=f"任务 {job_id} 的授权未能生效，请重试或检查调度器存储。",
                error_kind="internal",
            )
        return ToolResult(
            output=(
                f"已授权任务 {job_id}（{updated.name}）在无人值守下执行写入/命令类工具。"
                "修改任务内容会使授权失效，需要重新授权。"
            ),
            metadata={"job_id": job_id, "authorized": True},
        )


def _render_fields(headline: str, fields: dict[str, str]) -> str:
    """One label-per-line block. The gate collapses whitespace and caps lengths."""
    parts = [headline]
    for label, value in fields.items():
        text = str(value).strip()
        if text:
            parts.append(f"{label}: {text}")
    return "\n".join(parts)


def _grant_source(ctx: ToolExecutionContext | None) -> str:
    """Provenance for the audit trail: which surface the consent came from.

    Every grant minted from a tool call used to be recorded as "tui-approval",
    including the ones a person approved from weixin or telegram — so the audit
    field named a surface the operator had never touched. The channel is on the
    context already; use it.
    """
    channel = str(getattr(ctx, "channel", "") or "")
    if channel in ("cli", "direct", "", "gateway:cli"):
        return "tui-approval"
    return "chat-approval"
