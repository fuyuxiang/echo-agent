"""Text-to-speech tool — convert text to audio via edge-tts or OpenAI TTS."""

from __future__ import annotations

from pathlib import Path
from typing import Any

from echo_agent.agent.tools.base import Tool, ToolExecutionContext, ToolResult
from echo_agent.dependencies.lazy_deps import INSTALL_TIMEOUT_SECONDS
from echo_agent.scheduler.delivery import target_from_session_key


class TTSTool(Tool):
    name = "text_to_speech"
    description = (
        "Convert text to speech audio. Uses edge-tts (free) by default, or OpenAI TTS if configured. "
        "Set deliver=true to also send the audio file to the user's chat in the same step — use this "
        "for scheduled/unattended tasks so the audio actually reaches the user instead of only being saved."
    )
    parameters = {
        "type": "object",
        "properties": {
            "text": {"type": "string", "description": "Text to convert to speech."},
            "voice": {"type": "string", "description": "Voice name. For edge-tts: e.g., 'en-US-AriaNeural'. For OpenAI: 'alloy', 'echo', 'fable', 'onyx', 'nova', 'shimmer'."},
            "output_path": {"type": "string", "description": "Output file path (relative to workspace). Auto-generated if omitted."},
            "backend": {"type": "string", "enum": ["edge", "openai"], "description": "TTS backend to use."},
            "deliver": {"type": "boolean", "description": "If true, automatically send the generated audio file to the chat after synthesis. Defaults to false."},
            "deliver_channel": {"type": "string", "description": "Target channel for delivery. Defaults to the current chat's channel."},
            "deliver_chat_id": {"type": "string", "description": "Target chat id for delivery. Defaults to the current chat."},
            "caption": {"type": "string", "description": "Optional text sent before the audio when deliver=true."},
        },
        "required": ["text"],
    }
    # First use may lazily install the edge-tts backend (up to
    # INSTALL_TIMEOUT_SECONDS on the serialized executor) before synthesizing.
    # Keep the registry's wait_for ceiling above that plus synthesis overhead.
    timeout_seconds = INSTALL_TIMEOUT_SECONDS + 60

    def __init__(self, workspace: str, openai_api_key: str = "", openai_api_base: str = "", tts_model: str = "tts-1", default_backend: str = "", default_voice: str = "", publish_fn=None):
        self._workspace = Path(workspace)
        self._openai_key = openai_api_key
        self._openai_base = (openai_api_base or "https://api.openai.com/v1").rstrip("/")
        self._tts_model = tts_model
        self._default_backend = default_backend or "edge"
        self._default_voice = default_voice
        self._publish = publish_fn

    async def execute(self, params: dict[str, Any], ctx: ToolExecutionContext | None = None) -> ToolResult:
        text = params["text"]
        backend = params.get("backend", self._default_backend)
        voice = params.get("voice", self._default_voice or "")
        output_path = params.get("output_path", "")

        if not output_path:
            output_path = f"tts_output_{id(text) % 100000}.mp3"
        full_path = (self._workspace / output_path).resolve()

        if backend == "openai":
            result = await self._openai_tts(text, voice or "alloy", full_path)
        else:
            result = await self._edge_tts(text, voice or "en-US-AriaNeural", full_path)

        if result.success and params.get("deliver"):
            delivered = await self._deliver(full_path, params, ctx)
            # Fold the delivery outcome into the result so the caller (and the
            # audit log) reflects whether the audio actually reached the user,
            # not just that a file was written.
            if delivered is not None:
                result.output = f"{result.output}; {delivered}"
        return result

    async def _deliver(self, audio: Path, params: dict[str, Any], ctx: ToolExecutionContext | None) -> str | None:
        """Publish the generated audio as a FILE block to the target chat.

        Returns a human-readable status suffix, or None if delivery could not be
        attempted (which keeps the file-saved success intact)."""
        if not self._publish:
            return "not delivered (message bus not connected)"

        channel = str(params.get("deliver_channel") or "").strip()
        chat_id = str(params.get("deliver_chat_id") or "").strip()
        if (not channel or not chat_id) and ctx:
            channel = channel or (ctx.channel or "")
            chat_id = chat_id or (ctx.chat_id or "")
        if (not channel or not chat_id) and ctx and ctx.session_key:
            sk_channel, sk_chat = target_from_session_key(ctx.session_key)
            channel = channel or sk_channel
            chat_id = chat_id or sk_chat
        if not channel or not chat_id:
            return "not delivered (no target chat resolved)"

        from echo_agent.bus.events import ContentBlock, ContentType, OutboundEvent

        blocks: list[ContentBlock] = []
        caption = str(params.get("caption") or "").strip()
        if caption:
            blocks.append(ContentBlock(type=ContentType.TEXT, text=caption))
        blocks.append(ContentBlock(type=ContentType.FILE, url=str(audio), metadata={"name": audio.name}))
        event = OutboundEvent(channel=channel, chat_id=chat_id, content=blocks)
        try:
            await self._publish(event)
            return f"delivered to {channel}:{chat_id}"
        except Exception as e:
            return f"delivery failed: {e}"

    async def _edge_tts(self, text: str, voice: str, output: Path) -> ToolResult:
        try:
            from echo_agent.dependencies.lazy_deps import ensure_async, FeatureUnavailable
            await ensure_async("skill.tts-voice", prompt=False)
        except FeatureUnavailable as e:
            return ToolResult(success=False, error=str(e))
        try:
            import edge_tts
        except ImportError:
            return ToolResult(success=False, error="edge-tts not installed: pip install edge-tts")

        try:
            communicate = edge_tts.Communicate(text, voice)
            await communicate.save(str(output))
            return ToolResult(output=f"Audio saved to {output.name}", metadata={"path": str(output), "voice": voice})
        except Exception as e:
            return ToolResult(success=False, error=f"edge-tts failed: {e}")

    async def _openai_tts(self, text: str, voice: str, output: Path) -> ToolResult:
        if not self._openai_key:
            return ToolResult(success=False, error="OpenAI API key not configured for TTS")

        import aiohttp
        url = f"{self._openai_base}/audio/speech"
        headers = {"Authorization": f"Bearer {self._openai_key}", "Content-Type": "application/json"}
        body = {"model": self._tts_model, "input": text, "voice": voice, "response_format": "mp3"}

        try:
            async with aiohttp.ClientSession() as session:
                async with session.post(url, json=body, headers=headers, timeout=aiohttp.ClientTimeout(total=30)) as resp:
                    if resp.status != 200:
                        err = await resp.text()
                        return ToolResult(success=False, error=f"OpenAI TTS error {resp.status}: {err[:300]}")
                    data = await resp.read()
                    output.parent.mkdir(parents=True, exist_ok=True)
                    output.write_bytes(data)
                    return ToolResult(output=f"Audio saved to {output.name}", metadata={"path": str(output), "voice": voice})
        except Exception as e:
            return ToolResult(success=False, error=f"OpenAI TTS failed: {e}")
