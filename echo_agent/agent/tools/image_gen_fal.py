"""Image generation tool — FAL.ai backend."""

from __future__ import annotations

import asyncio
import os
import threading
from typing import Any

from echo_agent.agent.tools.base import Tool, ToolExecutionContext, ToolResult

DEFAULT_MODEL = "fal-ai/flux/schnell"

FAL_MODELS: dict[str, dict[str, Any]] = {
    "fal-ai/flux/schnell": {
        "display": "FLUX Schnell",
        "size_style": "image_size_preset",
        "sizes": {
            "landscape": "landscape_16_9",
            "square": "square_hd",
            "portrait": "portrait_16_9",
        },
        "defaults": {
            "num_images": 1,
            "output_format": "png",
            "enable_safety_checker": False,
            "sync_mode": True,
        },
        "supports": {
            "prompt", "image_size", "num_images", "output_format",
            "enable_safety_checker", "sync_mode", "seed",
        },
    },
    "fal-ai/flux-2-pro": {
        "display": "FLUX 2 Pro",
        "size_style": "image_size_preset",
        "sizes": {
            "landscape": "landscape_16_9",
            "square": "square_hd",
            "portrait": "portrait_16_9",
        },
        "defaults": {
            "num_inference_steps": 50,
            "guidance_scale": 4.5,
            "num_images": 1,
            "output_format": "png",
            "enable_safety_checker": False,
            "sync_mode": True,
        },
        "supports": {
            "prompt", "image_size", "num_inference_steps", "guidance_scale",
            "num_images", "output_format", "enable_safety_checker",
            "sync_mode", "seed",
        },
    },
    "fal-ai/ideogram/v3": {
        "display": "Ideogram V3",
        "size_style": "image_size_preset",
        "sizes": {
            "landscape": "landscape_16_9",
            "square": "square_hd",
            "portrait": "portrait_16_9",
        },
        "defaults": {
            "rendering_speed": "BALANCED",
            "expand_prompt": True,
            "style": "AUTO",
        },
        "supports": {
            "prompt", "image_size", "rendering_speed", "expand_prompt",
            "style", "seed",
        },
    },
    "fal-ai/recraft/v4/pro/text-to-image": {
        "display": "Recraft V4 Pro",
        "size_style": "image_size_preset",
        "sizes": {
            "landscape": "landscape_16_9",
            "square": "square_hd",
            "portrait": "portrait_16_9",
        },
        "defaults": {
            "enable_safety_checker": False,
        },
        "supports": {
            "prompt", "image_size", "enable_safety_checker",
        },
    },
    "fal-ai/qwen-image": {
        "display": "Qwen Image",
        "size_style": "image_size_preset",
        "sizes": {
            "landscape": "landscape_16_9",
            "square": "square_hd",
            "portrait": "portrait_16_9",
        },
        "defaults": {
            "num_inference_steps": 30,
            "guidance_scale": 2.5,
            "num_images": 1,
            "output_format": "png",
        },
        "supports": {
            "prompt", "image_size", "num_inference_steps", "guidance_scale",
            "num_images", "output_format", "seed", "sync_mode",
        },
    },
}

VALID_ASPECT_RATIOS = ("landscape", "square", "portrait")


def _load_fal_client():
    try:
        from echo_agent.dependencies import ensure
        ensure("tool.image-gen-fal")
    except Exception:
        pass
    import fal_client  # type: ignore
    return fal_client


_fal_env_lock = threading.Lock()


def _build_payload(model_id: str, prompt: str, aspect_ratio: str) -> dict[str, Any]:
    meta = FAL_MODELS[model_id]
    size_style = meta["size_style"]
    sizes = meta["sizes"]

    aspect = aspect_ratio if aspect_ratio in sizes else "landscape"
    payload: dict[str, Any] = dict(meta.get("defaults", {}))
    payload["prompt"] = prompt.strip()

    if size_style == "image_size_preset":
        payload["image_size"] = sizes[aspect]
    elif size_style == "aspect_ratio":
        payload["aspect_ratio"] = sizes[aspect]

    supports = meta["supports"]
    return {k: v for k, v in payload.items() if k in supports}


class FalImageGenTool(Tool):
    name = "image_generate"
    description = "Generate an image from a text prompt using FAL.ai (supports FLUX, Ideogram, Recraft, Qwen and more)."
    parameters = {
        "type": "object",
        "properties": {
            "prompt": {"type": "string", "description": "Text description of the image to generate."},
            "aspect_ratio": {
                "type": "string",
                "enum": ["landscape", "square", "portrait"],
                "description": "Image aspect ratio.",
            },
        },
        "required": ["prompt"],
    }
    timeout_seconds = 120

    def __init__(self, fal_key: str = "", model: str = ""):
        self._fal_key = fal_key
        self._model = model or DEFAULT_MODEL

    def is_ready(self) -> bool:
        return bool(self._fal_key)

    def readiness_detail(self) -> tuple[bool, str]:
        if self._fal_key:
            return True, "ok"
        return False, "FAL.ai API key not configured"

    async def execute(self, params: dict[str, Any], ctx: ToolExecutionContext | None = None) -> ToolResult:
        if not self._fal_key:
            return ToolResult(success=False, error="FAL.ai API key not configured")

        prompt = params.get("prompt", "").strip()
        if not prompt:
            return ToolResult(success=False, error="prompt is required")

        aspect_ratio = params.get("aspect_ratio", "landscape").lower().strip()
        if aspect_ratio not in VALID_ASPECT_RATIOS:
            aspect_ratio = "landscape"

        model_id = self._model
        if model_id not in FAL_MODELS:
            supported = ", ".join(sorted(FAL_MODELS.keys()))
            return ToolResult(
                success=False,
                error=f"Unknown FAL model '{model_id}'. Supported models: {supported}",
            )

        arguments = _build_payload(model_id, prompt, aspect_ratio)

        try:
            fal = _load_fal_client()
        except ImportError:
            return ToolResult(
                success=False,
                error="fal-client package not installed. Run: pip install fal-client",
            )

        def _submit_with_key():
            with _fal_env_lock:
                prev_key = os.environ.get("FAL_KEY")
                os.environ["FAL_KEY"] = self._fal_key
                try:
                    handler = fal.submit(model_id, arguments=arguments)
                    return handler.get()
                finally:
                    if prev_key is not None:
                        os.environ["FAL_KEY"] = prev_key
                    else:
                        os.environ.pop("FAL_KEY", None)

        try:
            result = await asyncio.to_thread(_submit_with_key)
        except Exception as e:
            return ToolResult(success=False, error=f"FAL.ai generation failed: {e}")

        images = result.get("images") if isinstance(result, dict) else None
        if not images:
            return ToolResult(success=False, error="No images returned from FAL.ai")

        image_url = images[0].get("url", "")
        width = images[0].get("width", "")
        height = images[0].get("height", "")
        output = f"Image URL: {image_url}"
        if width and height:
            output += f"\nSize: {width}x{height}"
        return ToolResult(output=output, metadata={"url": image_url})
