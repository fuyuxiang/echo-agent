"""Vision tool — analyze images via a vision-capable LLM."""

from __future__ import annotations

import base64
from pathlib import Path
from typing import Any

from echo_agent.tools import Tool, ToolExecutionContext, ToolResult
from echo_agent.models.provider import LLMProvider
from echo_agent.security.path_policy import check_read, resolve_path


def _encode_image(path: str) -> tuple[str, str]:
    from echo_agent.channels.qqbot_media import image_mime_for

    mime = image_mime_for(path)
    data = Path(path).read_bytes()
    return mime, base64.b64encode(data).decode()


class VisionTool(Tool):
    name = "vision_analyze"
    description = "Analyze an image using a vision-capable LLM. Provide a file path or URL and a question about the image."
    parameters = {
        "type": "object",
        "properties": {
            "image": {"type": "string", "description": "Path to a local image file or an image URL."},
            "prompt": {"type": "string", "description": "Question or instruction about the image."},
            "model": {"type": "string", "description": "Override model for vision. Uses the configured default if omitted."},
        },
        "required": ["image", "prompt"],
    }
    timeout_seconds = 60

    def __init__(self, provider: LLMProvider, workspace: str = "", *, restrict: bool = False,
                 spill_root: Path | None = None):
        self._provider = provider
        self._workspace = str(Path(workspace or ".").resolve())
        self._restrict = restrict
        self._spill_root = spill_root

    async def execute(self, params: dict[str, Any], ctx: ToolExecutionContext | None = None) -> ToolResult:
        image = params["image"]
        prompt = params["prompt"]
        model = params.get("model")

        if image.startswith(("http://", "https://")):
            image_content = {"type": "image_url", "image_url": {"url": image}}
        else:
            violation = check_read(image, self._workspace, spill_root=self._spill_root)
            if violation:
                return ToolResult(success=False, error=violation, error_kind="business")
            image_path = resolve_path(image, self._workspace)
            if self._restrict and not image_path.is_relative_to(self._workspace):
                return ToolResult(success=False, error="Image path is outside workspace", error_kind="business")
            if not image_path.exists():
                return ToolResult(success=False, error=f"Image not found: {image}")
            try:
                mime, b64 = _encode_image(str(image_path))
                image_content = {"type": "image_url", "image_url": {"url": f"data:{mime};base64,{b64}"}}
            except Exception as e:
                return ToolResult(success=False, error=f"Failed to read image: {e}")

        messages = [
            {"role": "user", "content": [
                image_content,
                {"type": "text", "text": prompt},
            ]},
        ]

        resp = await self._provider.chat_with_retry(messages=messages, model=model)
        if resp.finish_reason == "error":
            return ToolResult(success=False, error=resp.content or "Vision analysis failed")
        return ToolResult(output=resp.content or "(no analysis)")

    def execution_mode(self, params: dict[str, Any]) -> str:
        return "read_only"
