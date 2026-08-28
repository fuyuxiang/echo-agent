"""MCP tool adapter — wraps MCP tools as echo-agent Tool instances."""

from __future__ import annotations

import json
from typing import Any

from loguru import logger

from echo_agent.tools import Tool, ToolExecutionContext, ToolResult
from echo_agent.mcp.client import MCPClient
from echo_agent.mcp.security import derive_tool_name
from echo_agent.security.risk_classifier import RiskLevel

#: Ordered by severity so a floor and a hint can be combined with ``max``.
_SEVERITY: dict[RiskLevel, int] = {
    RiskLevel.READ_ONLY: 0,
    RiskLevel.WRITE: 1,
    RiskLevel.EXEC: 2,
    RiskLevel.DANGEROUS: 3,
}


def _sanitize_name(server: str, tool: str) -> str:
    """Kept as the historical entry point; name derivation lives in security.py
    because collision detection has to reason about the same strings."""
    return derive_tool_name(server, tool)


def _convert_mcp_schema(mcp_tool: dict[str, Any]) -> dict[str, Any]:
    """Normalise a server-supplied ``inputSchema`` into a usable parameter schema.

    Structural validity is enforced earlier (``security.validate_input_schema``);
    what remains here is filling in the shape MCP allows to be implicit. A schema
    with ``properties`` but no ``type`` is common and legal on the wire, but the
    model's function-calling contract expects an explicit object.
    """
    schema = mcp_tool.get("inputSchema")
    if not isinstance(schema, dict) or not schema:
        return {"type": "object", "properties": {}}

    normalised = dict(schema)
    normalised.setdefault("type", "object")
    if normalised["type"] == "object":
        normalised.setdefault("properties", {})
    return normalised


class MCPToolAdapter(Tool):

    timeout_seconds = 120
    # Declared rather than inferred from the ``mcp_`` name prefix, so tool policy
    # can deny MCP wholesale by capability (see security/tool_policy.py) without
    # depending on a naming convention holding.
    capabilities = ("mcp.call",)

    def __init__(
        self,
        server_name: str,
        mcp_tool: dict[str, Any],
        client: MCPClient,
        *,
        trust_level: str = "untrusted",
        registered_name: str = "",
    ):
        self._server_name = server_name
        self._mcp_tool_name = mcp_tool.get("name", "")
        self._client = client
        self._trust_level = trust_level

        # The registered name is derived once during validation (where collisions
        # are detected) and passed in. Re-deriving it here would be a second
        # source of truth for the same string.
        self.name = registered_name or derive_tool_name(server_name, self._mcp_tool_name)
        self.description = mcp_tool.get("description", f"MCP tool from {server_name}")
        self.parameters = _convert_mcp_schema(mcp_tool)
        self.risk_level = self._classify_risk(mcp_tool, trust_level).value

    @staticmethod
    def _classify_risk(mcp_tool: dict[str, Any], trust_level: str = "untrusted") -> RiskLevel:
        """Classify an MCP tool's risk. Annotations may only *raise* it.

        MCP ``ToolAnnotations`` are hints supplied by the server, and the
        specification is explicit that clients must not treat them as
        trustworthy for security decisions when the server is untrusted. The
        previous implementation let ``readOnlyHint: true`` map straight to
        ``READ_ONLY``, and ``ApprovalGate`` passes READ_ONLY and WRITE without
        asking anyone — so any server could self-declare its delete/transfer/
        upload tool as read-only and skip EXEC approval entirely. The hint was
        doing the one job it is documented as unfit for.

        So trust is now a property of the *server*, set by the operator in
        config, and never of the payload:

        * ``untrusted`` (default) — floor of EXEC, i.e. a human approves the
          first call. A ``destructiveHint`` still escalates to DANGEROUS;
          ``readOnlyHint`` is ignored, because a claim that lowers the gate is
          exactly the claim an attacker would make.
        * ``trusted`` — the operator has vouched for this server, so hints are
          honoured in both directions and behave as documented.

        Malformed annotations (non-dict, non-boolean fields) never relax
        anything: they fall through to the floor.
        """
        floor = RiskLevel.WRITE if trust_level == "trusted" else RiskLevel.EXEC

        annotations = mcp_tool.get("annotations")
        if not isinstance(annotations, dict):
            return floor

        # `is True` rather than truthiness: a server sending the string "yes"
        # must not be read as a boolean claim.
        destructive = annotations.get("destructiveHint") is True
        read_only = annotations.get("readOnlyHint") is True

        if destructive:
            # Escalation from either floor. Conflicting hints resolve here, so a
            # tool claiming both read-only and destructive is treated as the
            # more dangerous of the two.
            return RiskLevel.DANGEROUS if trust_level != "trusted" else RiskLevel.EXEC

        if read_only and trust_level == "trusted":
            return RiskLevel.READ_ONLY

        return floor

    async def execute(self, params: dict[str, Any], ctx: ToolExecutionContext | None = None) -> ToolResult:
        try:
            resp = await self._client.call_tool(
                self._mcp_tool_name, params, timeout=self.timeout_seconds,
            )
        except TimeoutError as e:
            # error_kind is what the circuit breaker reads (ToolResult.
            # INFRA_ERROR_KINDS). Leaving it blank classified every MCP outage as
            # a *business* failure, so a dead server never tripped the breaker and
            # each call kept paying the full timeout.
            return ToolResult(success=False, error=str(e), error_kind="timeout")
        except (ConnectionError, OSError) as e:
            return ToolResult(
                success=False,
                error=f"MCP server '{self._server_name}' is unreachable: {e}",
                error_kind="dependency",
            )
        except Exception as e:
            logger.error("MCP tool '{}' call failed: {}", self.name, e)
            return ToolResult(success=False, error=f"MCP call failed: {e}", error_kind="dependency")

        return self._adapt_result(resp)

    def _adapt_result(self, resp: dict[str, Any]) -> ToolResult:
        """Render an MCP ``tools/call`` result into a ToolResult.

        Every content block the spec defines is represented. The previous version
        kept ``text`` and turned an image into a bare ``[image: mime]`` marker
        while dropping ``audio``, ``resource_link`` and ``structuredContent``
        entirely — for a server that answers primarily with structured content
        (the direction the spec has taken since 2025-06) that meant a successful
        call rendered as an empty string, which reads to the model as "the tool
        returned nothing" rather than "the payload was discarded".
        """
        content_parts = resp.get("content", [])
        is_error = bool(resp.get("isError", False))
        text_parts: list[str] = []

        if isinstance(content_parts, list):
            for part in content_parts:
                rendered = self._render_content_part(part)
                if rendered:
                    text_parts.append(rendered)

        structured = resp.get("structuredContent")
        if structured is not None:
            try:
                text_parts.append(
                    "[structuredContent]\n" + json.dumps(structured, ensure_ascii=False, indent=2)
                )
            except (TypeError, ValueError):
                text_parts.append(f"[structuredContent] {structured!r}")

        output = "\n".join(p for p in text_parts if p)
        metadata: dict[str, Any] = {
            "mcp_server": self._server_name,
            "mcp_tool": self._mcp_tool_name,
            "mcp_trust_level": self._trust_level,
        }
        if structured is not None:
            metadata["mcp_structured"] = structured

        if is_error:
            # A tool-level error is the server reporting a business outcome
            # ("file not found"), not infrastructure breaking, so it carries no
            # error_kind: it must not count toward the breaker.
            return ToolResult(
                success=False,
                error=output or "MCP tool reported an error with no detail",
                metadata=metadata,
            )
        return ToolResult(success=True, output=output, metadata=metadata)

    @staticmethod
    def _render_content_part(part: Any) -> str:
        if isinstance(part, str):
            return part
        if not isinstance(part, dict):
            return ""

        kind = part.get("type")
        if kind == "text":
            return str(part.get("text", ""))
        if kind == "image":
            return f"[image: {part.get('mimeType', 'unknown')}]"
        if kind == "audio":
            return f"[audio: {part.get('mimeType', 'unknown')}]"
        if kind == "resource_link":
            name = part.get("name") or part.get("uri", "")
            desc = part.get("description", "")
            return f"[resource_link: {name}]" + (f" {desc}" if desc else "")
        if kind == "resource":
            res = part.get("resource", {})
            if not isinstance(res, dict):
                return "[resource]"
            uri = res.get("uri", "")
            if "text" in res:
                return f"[resource: {uri}]\n{res.get('text', '')}"
            # Binary resources arrive base64-encoded; the bytes are useless to the
            # model, the descriptor is not.
            return f"[resource: {uri} ({res.get('mimeType', 'binary')})]"
        # Unknown block type: say so rather than dropping it silently, so a new
        # spec revision surfaces as a visible gap instead of missing output.
        return f"[unsupported content type: {kind}]" if kind else ""

    def execution_mode(self, params: dict[str, Any]) -> str:
        return "read_only" if self.risk_level == RiskLevel.READ_ONLY.value else "side_effect"

    def readiness_detail(self) -> tuple[bool, str]:
        """Report the live connection state.

        The registry drops non-ready tools from the definitions it sends the
        model, so a server that has dropped off stops being advertised on the
        next turn instead of being offered and failing on use.
        """
        if self._client is None:
            return False, "MCP client not attached"
        if not getattr(self._client, "is_connected", False):
            return False, f"MCP server '{self._server_name}' is disconnected"
        return True, "ok"

    def is_ready(self) -> bool:
        return self.readiness_detail()[0]
