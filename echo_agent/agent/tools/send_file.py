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

    def __init__(self, workspace: str, restrict: bool = False, publish_fn=None,
                 channel_lookup=None, spill_root: Path | None = None):
        self._workspace = str(Path(workspace).resolve())
        self._restrict = restrict
        self._publish = publish_fn
        # spill 闸门在这里比在读取工具上更要紧:这条路径的终点是把文件当附件
        # 投到聊天里,越权内容直接离开进程,连一次模型转述都不需要。
        self._spill_root = spill_root
        # Resolves a channel name to its adapter so capability can be checked
        # before promising the model an upload. Optional: without it the tool
        # degrades to reporting whatever the delivery receipt says.
        self._channel_lookup = channel_lookup

    def _unsupported_reason(self, channel: str) -> str:
        """Why *channel* cannot deliver a file, or "" when it can (or is unknown).

        Only channels that actually consume structured FILE/IMAGE blocks upload
        an attachment; the rest send the caption text and drop the file. Saying
        so up front is the difference between the model learning it must find
        another route and the model believing a file was delivered.
        """
        if self._channel_lookup is None:
            return ""
        adapter = self._channel_lookup(channel)
        if adapter is None:
            return ""
        if getattr(adapter, "supports_files", False):
            return ""
        return (
            f"channel '{channel}' cannot send files — it delivers text only, so "
            "the attachment would be dropped. Send the content as text, or use a "
            "channel with file support."
        )

    async def execute(self, params: dict[str, Any], ctx: ToolExecutionContext | None = None) -> ToolResult:
        if not self._publish:
            return ToolResult(success=False, error="Message bus not connected")
        file_path = params["file_path"]

        violation = check_read(file_path, self._workspace, spill_root=self._spill_root)
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

        unsupported = self._unsupported_reason(params["channel"])
        if unsupported:
            return ToolResult(success=False, error=unsupported, error_kind="business")

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

        event = OutboundEvent(
            channel=params["channel"], chat_id=params["chat_id"], content=blocks,
        ).mark_tool_delivery(ctx)
        try:
            receipt = await self._publish(event)
        except Exception as e:
            return ToolResult(success=False, error=str(e))
        # publish_outbound returns a DeliveryResult. Ignoring it is how this tool
        # used to report "File sent" for a file that reached no handler at all
        # (NO_HANDLER) or that the channel explicitly refused (FAILED).
        if receipt is not None and not getattr(receipt, "ok", True):
            stage = getattr(getattr(receipt, "stage", None), "value", "failed")
            detail = getattr(receipt, "error", "") or stage
            return ToolResult(
                success=False,
                error=f"File not delivered to {params['channel']}:{params['chat_id']}: {detail}",
                error_kind="dependency",
            )
        return ToolResult(output=f"File sent to {params['channel']}:{params['chat_id']} ({resolved.name})")
