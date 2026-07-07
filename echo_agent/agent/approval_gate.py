"""Approval and security gate for tool calls."""

from __future__ import annotations

from dataclasses import dataclass
from typing import Any


from echo_agent.agent.degraded_notice import (
    REASON_APPROVAL_TIMEOUT,
    REASON_APPROVAL_UNAVAILABLE,
    notice_for,
)
from echo_agent.agent.tools.base import ToolResult
from echo_agent.bus.events import InboundEvent, OutboundEvent
from echo_agent.bus.queue import MessageBus
from echo_agent.config.schema import Config
from echo_agent.models.inference import InferenceController
from echo_agent.permissions.allowlist import ApprovalAllowlist, ApprovalLevel, build_pattern_key
from echo_agent.permissions.manager import ApprovalManager, ApprovalStatus
from echo_agent.security.guards import GuardDecision, evaluate_tool_call
from echo_agent.security.risk_classifier import RiskLevel, classify_risk


@dataclass
class ApprovalCheck:
    denial: ToolResult | None = None
    approved_actions: frozenset[str] = frozenset()
    notify_user: bool = False
    notice: str = ""


class ApprovalGate:
    """Combines risk classification, channel trust, smart approval, and manual approval."""

    def __init__(
        self,
        *,
        config: Config,
        approval: ApprovalManager,
        inference: InferenceController,
        bus: MessageBus,
        provider: Any = None,
        allowlist: ApprovalAllowlist | None = None,
        registry: Any = None,
        router: Any = None,
        cognitive_emitter: Any = None,
    ):
        self._config = config
        self._approval = approval
        self._inference = inference
        self._bus = bus
        self._provider = provider
        self._allowlist = allowlist or ApprovalAllowlist()
        self._router = router
        self._cog = cognitive_emitter
        # The registry lets the gate read a tool's *declared* risk_level (e.g.
        # MCP tools classify destructiveHint → EXEC at adapter construction).
        # Without it, dynamic tools fall through to the WRITE default and skip
        # approval entirely.
        self._registry = registry

    async def check(
        self,
        tool_name: str,
        arguments: dict[str, Any],
        sender_id: str,
        *,
        channel: str = "",
        event: InboundEvent | None = None,
        running: bool = True,
    ) -> ApprovalCheck:
        approval_cfg = self._config.permissions.approval

        # Step 1: Static guard (hard block dangerous patterns)
        guard = evaluate_tool_call(self._config, tool_name, arguments)
        if guard.denied:
            return ApprovalCheck(ToolResult(
                success=False,
                error=f"Tool '{tool_name}' blocked by security policy: {guard.reason}",
                metadata={"guard_pattern": guard.pattern_key},
            ))

        # Step 2: Elevated rights check
        if self._requires_elevated(tool_name) and not self._elevated_allowed(channel, sender_id):
            return ApprovalCheck(ToolResult(
                success=False,
                error=(
                    f"Tool '{tool_name}' requires elevated execution rights for the configured "
                    "executor/security policy."
                ),
                metadata={"requires_elevated": True},
            ))

        # Step 3: Risk classification — fall back to a dynamic tool's *declared*
        # risk_level (e.g. MCP destructiveHint→EXEC) when it isn't in the static
        # map, so dynamic tools no longer slip through as the WRITE default.
        declared_risk = ""
        if self._registry is not None:
            tool = self._registry.get(tool_name)
            if tool is not None:
                declared_risk = getattr(tool, "risk_level", "") or ""
        risk = classify_risk(tool_name, arguments, tool_risk_level=declared_risk)

        # Approved-pass: any path that lets the call through must tell the tool
        # which actions were approved. The tool's own guard (e.g. CodeExecTool)
        # re-checks exec/code policy and blocks unless ctx.approved_actions
        # contains the action — so returning an empty ApprovalCheck() here would
        # silently fail EXEC tools even though the gate "approved" them.
        def _approved() -> ApprovalCheck:
            return ApprovalCheck(approved_actions=self._approved_actions(tool_name, guard))

        # Step 4: READ_ONLY and WRITE always pass — they are protected by sandbox/path restrictions
        if risk in (RiskLevel.READ_ONLY, RiskLevel.WRITE):
            return _approved()

        # Step 5: Explicit auto_approve list
        if tool_name in approval_cfg.auto_approve:
            return _approved()

        # Step 6: CLI auto-approve (all levels)
        if self._should_auto_approve_cli(channel):
            return _approved()

        # Step 7: Channel trust — EXEC level auto-approved on trusted channels
        if risk == RiskLevel.EXEC and self._is_trusted_channel(channel):
            return _approved()

        # Step 8: Approval mode "off" — bypass everything except hard blocks
        if approval_cfg.mode == "off":
            return _approved()

        # Step 9: Check if this tool actually requires approval
        if not self._approval_required(tool_name, guard, risk):
            return _approved()

        # Step 10: Allowlist check (EXEC and DANGEROUS)
        # The write path (ApprovalAllowlist.approve) records an "always" grant for
        # ANY risk level, but historically this read path only honoured it for
        # EXEC — so a permanent grant for a DANGEROUS tool (e.g. tool:cronjob) was
        # written to disk yet never matched, and the user kept getting re-prompted
        # despite choosing "always". Honour the grant for DANGEROUS too. The
        # Step-1 static guard has already hard-blocked genuinely destructive
        # patterns above, so this respects the user's explicit persistent consent
        # without weakening those hard blocks.
        session_key = event.session_key if event else ""
        pattern_key = build_pattern_key(tool_name, arguments)
        if risk in (RiskLevel.EXEC, RiskLevel.DANGEROUS) and self._allowlist.is_approved(session_key, pattern_key):
            return _approved()

        # Step 11: Unattended mode check
        if self._is_unattended(event, channel):
            return self._resolve_unattended(tool_name, risk, session_key, pattern_key, guard, event)

        # Step 12: Smart approval (EXEC level only)
        if risk == RiskLevel.EXEC and approval_cfg.mode == "smart" and self._provider:
            verdict = await self._run_smart_approval(tool_name, arguments, guard)
            if verdict == "approve":
                self._allowlist.approve(session_key, pattern_key, ApprovalLevel.SESSION)
                return _approved()
            if verdict == "deny":
                return ApprovalCheck(ToolResult(
                    success=False,
                    error=f"Smart approval denied '{tool_name}': {guard.reason or 'assessed as dangerous'}",
                ))
            if verdict == "unavailable":
                # Provider outage: fail closed but tell the user, instead of
                # falling through to a blocking manual wait that times out
                # silently. See spec 2.1.
                return ApprovalCheck(
                    denial=ToolResult(
                        success=False,
                        error=f"Approval system unavailable for '{tool_name}' (provider outage).",
                    ),
                    notify_user=True,
                    notice=notice_for(REASON_APPROVAL_UNAVAILABLE),
                )

        # Step 13: Manual approval flow
        approved_actions = self._approved_actions(tool_name, guard)
        return await self._manual_approval_flow(
            tool_name, arguments, sender_id, channel, event, running,
            guard, approved_actions, session_key, pattern_key, risk,
        )

    async def _run_smart_approval(
        self, tool_name: str, arguments: dict[str, Any], guard: GuardDecision,
    ) -> str:
        from echo_agent.security.smart_approval import smart_approve
        command = str(arguments.get("command", "") or arguments.get("code", "") or arguments)
        description = guard.reason or f"tool '{tool_name}' requires approval"
        model = self._config.permissions.approval.smart_model
        return await smart_approve(tool_name, command, description, self._provider, model=model, router=self._router)

    async def _manual_approval_flow(
        self,
        tool_name: str,
        arguments: dict[str, Any],
        sender_id: str,
        channel: str,
        event: InboundEvent | None,
        running: bool,
        guard: GuardDecision,
        approved_actions: frozenset[str],
        session_key: str,
        pattern_key: str,
        risk: RiskLevel = RiskLevel.EXEC,
    ) -> ApprovalCheck:
        approval_req = self._approval.request_approval(
            tool_name, tool_name=tool_name, params=arguments, user_id=sender_id,
        )
        if approval_req.status == ApprovalStatus.DENIED:
            return ApprovalCheck(ToolResult(
                success=False,
                error=f"Tool '{tool_name}' denied by approval policy: {approval_req.reason}",
            ))
        if approval_req.status == ApprovalStatus.APPROVED:
            return ApprovalCheck(approved_actions=approved_actions)

        if event is not None:
            await self._publish_approval_request(
                event, approval_req.id, tool_name, guard, pattern_key, arguments, risk
            )

        if not running and self._is_interactive_channel(channel):
            return ApprovalCheck(ToolResult(
                success=False,
                error=(
                    f"Approval required before executing '{tool_name}'. "
                    f"Request id: {approval_req.id}."
                ),
                metadata={"approval_request_id": approval_req.id},
            ))

        decided = await self._approval.wait_for_decision(
            approval_req.id,
            timeout_seconds=self._config.permissions.approval.wait_timeout_seconds,
        )
        if decided and decided.status == ApprovalStatus.APPROVED:
            level = self._parse_approval_level(decided.reason)
            self._record_approval(session_key, pattern_key, level)
            return ApprovalCheck(approved_actions=approved_actions)
        if decided and decided.status == ApprovalStatus.DENIED:
            return ApprovalCheck(ToolResult(
                success=False,
                error=f"Tool '{tool_name}' denied: {decided.reason}",
                metadata={"approval_request_id": approval_req.id},
            ))
        return ApprovalCheck(
            denial=ToolResult(
                success=False,
                error=(
                    f"Approval timed out for '{tool_name}'. "
                    f"Request id: {approval_req.id}. "
                    f"Reply `/approve {approval_req.id}` or `/deny {approval_req.id} <reason>`."
                ),
                metadata={"approval_request_id": approval_req.id},
            ),
            notify_user=True,
            notice=notice_for(REASON_APPROVAL_TIMEOUT, tool=tool_name, request_id=approval_req.id),
        )

    @staticmethod
    def _describe_action(tool_name: str, arguments: dict[str, Any] | None) -> str:
        """A one-line, human-readable summary of what is about to run.

        For shell/code tools the actual command/snippet is what the user needs to
        judge risk — the bare tool name ('exec') tells them nothing. Truncated so
        a huge heredoc can't flood the chat."""
        args = arguments or {}
        raw = ""
        if tool_name in ("exec", "process"):
            raw = str(args.get("command", "")).strip()
        elif tool_name == "execute_code":
            lang = str(args.get("language", "")).strip()
            raw = f"[{lang}] " + str(args.get("code", "")).strip()
        if not raw:
            # Fall back to a compact key=value view of the args.
            raw = ", ".join(f"{k}={str(v)[:60]}" for k, v in args.items()) or tool_name
        raw = " ".join(raw.split())  # collapse newlines/whitespace to one line
        return raw if len(raw) <= 200 else raw[:200] + " …(截断)"

    async def _publish_approval_request(
        self,
        event: InboundEvent,
        request_id: str,
        tool_name: str,
        guard: GuardDecision,
        pattern_key: str,
        arguments: dict[str, Any] | None = None,
        risk: RiskLevel = RiskLevel.EXEC,
    ) -> None:
        reason = guard.reason or "审批策略要求确认"
        action = self._describe_action(tool_name, arguments)
        # The scope wording is written out in full so the user knows exactly what
        # each choice grants — the old "同类操作" was ambiguous about whether it
        # meant this one command or every command.
        family = pattern_key.split(":", 1)[0] if ":" in pattern_key else ""
        cmd_name = pattern_key.split(":", 1)[1] if ":" in pattern_key else pattern_key
        lines = [
            f"⚠️ 需要确认执行  (风险: {risk.value.upper()})",
            f"工具: {tool_name}",
            f"操作: {action}",
            f"原因: {reason}",
            "",
            "回复其一:",
            f"  /approve {request_id}          仅本次",
        ]
        # "approve all" only makes sense (and is only honoured) for EXEC-family
        # shell work — don't advertise it for DANGEROUS tools like cronjob.
        if family in self._SESSION_ALL_FAMILIES:
            lines.append(
                f"  /approve {request_id} all      本轮任务全放行:本会话内所有命令自动执行(推荐,省去逐条确认)"
            )
            lines.append(
                f"  /approve {request_id} session  本会话内只放行相同命令({cmd_name})"
            )
        else:
            lines.append(
                f"  /approve {request_id} session  本会话内放行相同操作"
            )
        lines.append(
            f"  /approve {request_id} always   永久放行相同操作({cmd_name}),写入磁盘"
        )
        lines.append(f"  /deny {request_id} [原因]      拒绝")
        text = "\n".join(lines)
        out = OutboundEvent.text_reply(
            channel=event.channel,
            chat_id=event.chat_id,
            text=text,
            reply_to_id=event.reply_to_id,
        )
        out.metadata = dict(event.metadata)
        out.metadata["_approval_request"] = True
        out.metadata["_inbound_event_id"] = event.event_id
        await self._bus.publish_outbound(out)
        # Additive: emit an interactive approval frame for an attached cli TUI.
        # Fires AFTER the text publish and never changes approval outcome.
        # Gate before building the params dict so IM channels pay nothing.
        if self._cog is not None and self._cog.active(event):
            await self._cog.emit(
                event, "approval_request",
                {"request_id": request_id, "action": tool_name, "tool": tool_name,
                 "params": {k: str(v)[:120] for k, v in (arguments or {}).items()},
                 "risk": reason},
                f"⚠️ 需要确认: {tool_name}",
            )

    def _approval_required(self, tool_name: str, guard: GuardDecision, risk: RiskLevel) -> bool:
        approval_cfg = self._config.permissions.approval
        if tool_name in approval_cfg.auto_deny:
            return True
        if guard.needs_approval:
            return True
        if self._inference.needs_confirmation(tool_name):
            return True
        if tool_name in approval_cfg.require_approval:
            return True
        if risk in (RiskLevel.EXEC, RiskLevel.DANGEROUS):
            return True
        return False

    def _should_auto_approve_cli(self, channel: str) -> bool:
        approval_cfg = self._config.permissions.approval
        return (
            self._config.security.profile == "personal_cli"
            and approval_cfg.cli_auto_approve
            and channel in {"cli", "direct", ""}
        )

    def _is_trusted_channel(self, channel: str) -> bool:
        return channel in self._config.permissions.approval.trusted_channels

    @staticmethod
    def _is_interactive_channel(channel: str) -> bool:
        """cli attaches over the gateway, so its channel is 'gateway:cli' — the
        bare {'cli','direct',''} check would misroute it into unattended/auto
        paths. Treat both spellings as interactive."""
        return channel in {"cli", "direct", "", "gateway:cli"}

    def _is_unattended(self, event: InboundEvent | None, channel: str) -> bool:
        # Read the typed trust field, never metadata: only a trusted producer
        # (scheduler/delivery.py) can set event.unattended, whereas metadata is
        # attacker-influenced on external channels. The channel fallback keeps
        # the bare cron/scheduler pseudo-channels unattended even if a caller
        # somehow constructed the event without the flag.
        if event and event.unattended:
            return True
        return channel in {"cron", "scheduler"}

    def _resolve_unattended(
        self, tool_name: str, risk: RiskLevel, session_key: str, pattern_key: str,
        guard: GuardDecision, event: InboundEvent | None = None,
    ) -> ApprovalCheck:
        policy = self._config.permissions.approval.unattended_policy
        approved = self._approved_actions(tool_name, guard)

        # Per-job authorization: a cron job only lands in the scheduler because
        # its creation passed the DANGEROUS-tier cronjob approval, which is the
        # human's up-front consent to let this specific job run unattended. When
        # the fired event carries that authorization we allow its WRITE/EXEC work
        # regardless of the global unattended_policy — the Step-1 static guard has
        # already hard-blocked genuinely destructive patterns (rm -rf, fork bombs,
        # denied network, etc.) above, so this is not a blanket exec bypass.
        #
        # DANGEROUS stays denied even when authorized: that prevents an unattended
        # job from creating more cron jobs / installing skills with no human in
        # the loop (a privilege-escalation / recursion vector). The authorization
        # is scoped to the job's own work, not to spawning new privileged state.
        if event is not None and event.cron_authorized:
            if risk in (RiskLevel.WRITE, RiskLevel.EXEC):
                return ApprovalCheck(approved_actions=approved)
            # DANGEROUS (and anything else) falls through to the deny below.

        if policy == "allow_safe":
            if risk == RiskLevel.WRITE:
                return ApprovalCheck(approved_actions=approved)
            if risk == RiskLevel.EXEC and self._allowlist.is_approved(session_key, pattern_key):
                return ApprovalCheck(approved_actions=approved)
        return ApprovalCheck(ToolResult(
            success=False,
            error=f"Tool '{tool_name}' requires approval but no user is available (unattended mode).",
        ))

    @staticmethod
    def _approved_actions(tool_name: str, guard: GuardDecision) -> frozenset[str]:
        actions = {tool_name}
        if guard.pattern_key:
            actions.add(guard.pattern_key)
        if guard.approval_action:
            actions.add(guard.approval_action)
        return frozenset(actions)

    # Only these families can be granted session-wide via "approve all". They map
    # to EXEC-risk shell work (build_pattern_key emits exec:/code:/process:). A
    # "tool:*" wildcard is deliberately NOT allowed — that would blanket-approve
    # DANGEROUS tools (cronjob, skill install, spawn) for the session, the exact
    # privilege-escalation / recursion vector the unattended path guards against.
    _SESSION_ALL_FAMILIES = frozenset({"exec", "code", "process"})

    def _record_approval(self, session_key: str, pattern_key: str, level: ApprovalLevel) -> None:
        """Persist the user's decision at the requested scope.

        For SESSION_ALL we widen the recorded key to a family wildcard (exec:*)
        so later, differently-named commands in the same session run without a
        fresh prompt. If the family isn't an EXEC-family one, we fall back to a
        plain SESSION grant of the exact key — never a broad wildcard.
        """
        if level == ApprovalLevel.SESSION_ALL:
            family = pattern_key.split(":", 1)[0] if ":" in pattern_key else ""
            if family in self._SESSION_ALL_FAMILIES:
                self._allowlist.approve(session_key, f"{family}:*", ApprovalLevel.SESSION_ALL)
                return
            # Non-exec family (e.g. tool:cronjob): downgrade to an exact
            # session grant so "all" can't broaden a DANGEROUS tool.
            self._allowlist.approve(session_key, pattern_key, ApprovalLevel.SESSION)
            return
        self._allowlist.approve(session_key, pattern_key, level)

    @staticmethod
    def _parse_approval_level(reason: str) -> ApprovalLevel:
        if reason:
            lower = reason.strip().lower()
            if lower == "always":
                return ApprovalLevel.ALWAYS
            if lower in ("all", "session-all", "session_all", "task"):
                return ApprovalLevel.SESSION_ALL
            if lower == "session":
                return ApprovalLevel.SESSION
        return ApprovalLevel.ONCE

    def _requires_elevated(self, tool_name: str) -> bool:
        if tool_name not in {"exec", "execute_code", "process"}:
            return False
        host = self._config.tools.exec.host
        if host == "auto":
            host = self._config.execution.default_executor
        return host in {"local", "remote"} or self._config.tools.exec.security == "full"

    def _elevated_allowed(self, channel: str, sender_id: str) -> bool:
        elevated = self._config.permissions.elevated
        if not elevated.enabled:
            return False
        allow_from = elevated.allow_from or {}
        candidates = set(allow_from.get("*", [])) | set(allow_from.get(channel, []))
        if "*" in candidates or sender_id in candidates:
            return True
        return sender_id in (self._config.permissions.admin_users or [])
