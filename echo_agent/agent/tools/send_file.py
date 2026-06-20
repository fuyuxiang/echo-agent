"""send_file tool — send a local file or image to a specific channel/chat.

Mirrors MessageTool: a thin wrapper that publishes an OutboundEvent. The actual
upload/transport is handled by the channel adapter (e.g. weixin uploads to the
WeChat CDN). The tool only resolves and validates the local path, then routes a
structured FILE/IMAGE content block onto the bus.
"""

from __future__ import annotations

import mimetypes
from pathlib import Path
from typing import Any

from echo_agent.agent.tools.base import Tool, ToolExecutionContext, ToolResult
from echo_agent.bus.events import ContentBlock, ContentType, OutboundEvent
from echo_agent.security.path_policy import check_read, resolve_path


class SendFileTool(Tool):
    name = "send_file"
    description = (
        "Send a local file or image to a specific channel and chat. "
        "Use to deliver a document, image, or other attachment the user asked for. "
        "Provide a local file path; set as_image=true to render images inline."
    )
    risk_level = "read_only"
    parameters = {
        "type": "object",
        "properties": {
            "channel": {"type": "string", "description": "Target channel name (e.g. weixin)."},
            "chat_id": {"type": "string", "description": "Target chat ID."},
            "file_path": {"type": "string", "description": "Local path to the file to send."},
            "caption": {"type": "string", "description": "Optional text sent before the file."},
            "as_image": {
                "type": "boolean",
                "description": "Force image rendering. Omit to infer from the file's MIME type.",
            },
        },
        "required": ["channel", "chat_id", "file_path"],
    }

    def __init__(self, workspace: str, restrict: bool = False, publish_fn=None):
        self._workspace = str(Path(workspace).resolve())
        self._restrict = restrict
        self._publish = publish_fn

    async def execute(self, params: dict[str, Any], ctx: ToolExecutionContext | None = None) -> ToolResult:
        if not self._publish:
            return ToolResult(success=False, error="Message bus not connected")
        file_path = params["file_path"]

        violation = check_read(file_path, self._workspace)
        if violation:
            return ToolResult(success=False, error=violation)
        if self._restrict:
            resolved = resolve_path(file_path, self._workspace)
            try:
                resolved.relative_to(self._workspace)
            except ValueError:
                return ToolResult(success=False, error=f"Path {file_path} is outside workspace {self._workspace}")

        resolved = resolve_path(file_path, self._workspace)
        if not resolved.exists():
            return ToolResult(success=False, error=f"File not found: {file_path}")

        as_image = params.get("as_image")
        if as_image is None:
            mime = mimetypes.guess_type(str(resolved))[0] or ""
            as_image = mime.startswith("image/")
        content_type = ContentType.IMAGE if as_image else ContentType.FILE

        blocks: list[ContentBlock] = []
        caption = params.get("caption") or ""
        if caption:
            blocks.append(ContentBlock(type=ContentType.TEXT, text=caption))
        blocks.append(ContentBlock(
            type=content_type,
            url=str(resolved),
            metadata={"name": resolved.name},
        ))

        event = OutboundEvent(channel=params["channel"], chat_id=params["chat_id"], content=blocks)
        try:
            await self._publish(event)
            return ToolResult(output=f"File sent to {params['channel']}:{params['chat_id']} ({resolved.name})")
        except Exception as e:
            return ToolResult(success=False, error=str(e))
