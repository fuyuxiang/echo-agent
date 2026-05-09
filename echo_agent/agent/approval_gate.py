"""Approval and security gate for tool calls."""

from __future__ import annotations

from dataclasses import dataclass
from typing import Any

from loguru import logger

from echo_agent.agent.tools.base import ToolResult
from echo_agent.bus.events import InboundEvent, OutboundEvent
from echo_agent.bus.queue import MessageBus
from echo_agent.config.schema import Config
from echo_agent.models.inference import InferenceController
from echo_agent.permissions.manager import ApprovalManager, ApprovalStatus
from echo_agent.security.guards import GuardDecision, evaluate_tool_call


@dataclass
class ApprovalCheck:
    denial: ToolResult | None = None
    approved_actions: frozenset[str] = frozenset()


class ApprovalGate:
    """Combines static policy, runtime guards, elevation, and async approval."""

    def __init__(
        self,
        *,
        config: Config,
        approval: ApprovalManager,
        inference: InferenceController,
        bus: MessageBus,
    ):
        self._config = config
        self._approval = approval
        self._inference = inference
        self._bus = bus

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
        guard = evaluate_tool_call(self._config, tool_name, arguments)
        if guard.denied:
            return ApprovalCheck(ToolResult(
                success=False,
                error=f"Tool '{tool_name}' blocked by security policy: {guard.reason}",
                metadata={"guard_pattern": guard.pattern_key},
            ))

        if self._requires_elevated(tool_name) and not self._elevated_allowed(channel, sender_id):
            return ApprovalCheck(ToolResult(
                success=False,
                error=(
                    f"Tool '{tool_name}' requires elevated execution rights for the configured "
                    "executor/security policy."
                ),
                metadata={"requires_elevated": True},
            ))

        if not self._approval_required(tool_name, guard):
            return ApprovalCheck()

        approval_req = self._approval.request_approval(
            tool_name, tool_name=tool_name, params=arguments, user_id=sender_id,
        )
        if approval_req.status == ApprovalStatus.DENIED:
            return ApprovalCheck(ToolResult(
                success=False,
                error=f"Tool '{tool_name}' denied by approval policy: {approval_req.reason}",
            ))

        approved_actions = self._approved_actions(tool_name, guard)
        if approval_req.status == ApprovalStatus.APPROVED:
            return ApprovalCheck(approved_actions=approved_actions)

        if approval_req.status == ApprovalStatus.PENDING:
            if self._should_auto_approve(channel):
                self._approval.approve(approval_req.id, decided_by="auto:cli")
                logger.info("Auto-approved '{}' for channel '{}' under personal_cli profile", tool_name, channel or "cli")
                return ApprovalCheck(approved_actions=approved_actions)

            if event is not None:
                await self._publish_approval_request(event, approval_req.id, tool_name, guard)

            if not running and channel in {"cli", "direct", ""}:
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
                return ApprovalCheck(approved_actions=approved_actions)
            if decided and decided.status == ApprovalStatus.DENIED:
                return ApprovalCheck(ToolResult(
                    success=False,
                    error=f"Tool '{tool_name}' denied by approval policy: {decided.reason}",
                    metadata={"approval_request_id": approval_req.id},
                ))
            return ApprovalCheck(ToolResult(
                success=False,
                error=(
                    f"Approval timed out before executing '{tool_name}'. "
                    f"Request id: {approval_req.id}. "
                    f"An admin can reply `/approve {approval_req.id}` or `/deny {approval_req.id} <reason>`."
                ),
                metadata={"approval_request_id": approval_req.id},
            ))

        return ApprovalCheck(approved_actions=approved_actions)

    async def _publish_approval_request(
        self,
        event: InboundEvent,
        request_id: str,
        tool_name: str,
        guard: GuardDecision,
    ) -> None:
        reason = guard.reason or "approval policy requires confirmation"
        text = (
            f"Approval required before executing '{tool_name}'.\n"
            f"Reason: {reason}\n"
            f"Request id: {request_id}\n"
            f"Reply `/approve {request_id}` or `/deny {request_id} <reason>`."
        )
        out = OutboundEvent.text_reply(
            channel=event.channel,
            chat_id=event.chat_id,
            text=text,
            reply_to_id=event.reply_to_id,
        )
        out.metadata = dict(event.metadata)
        out.metadata["_inbound_event_id"] = event.event_id
        out.metadata["_approval_request_id"] = request_id
        out.message_kind = "approval_required"
        out.is_final = False
        await self._bus.publish_outbound(out)

    def _approval_required(self, tool_name: str, guard: GuardDecision) -> bool:
        approval_cfg = self._config.permissions.approval
        if tool_name in approval_cfg.auto_approve:
            return False
        if tool_name in approval_cfg.auto_deny:
            return True
        if guard.needs_approval:
            return True
        if self._inference.needs_confirmation(tool_name):
            return True
        if tool_name in approval_cfg.require_approval:
            return True
        return False

    @staticmethod
    def _approved_actions(tool_name: str, guard: GuardDecision) -> frozenset[str]:
        actions = {tool_name}
        if guard.pattern_key:
            actions.add(guard.pattern_key)
        if guard.approval_action:
            actions.add(guard.approval_action)
        return frozenset(actions)

    def _should_auto_approve(self, channel: str) -> bool:
        approval_cfg = self._config.permissions.approval
        return (
            self._config.security.profile == "personal_cli"
            and approval_cfg.cli_auto_approve
            and channel in {"cli", "direct", ""}
        )

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
