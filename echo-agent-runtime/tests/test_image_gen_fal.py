"""Tests for FalImageGenTool."""

from __future__ import annotations

from unittest.mock import MagicMock, patch

import pytest

from echo_agent.agent.tools.image_gen_fal import (
    DEFAULT_MODEL,
    FAL_MODELS,
    FalImageGenTool,
    _build_payload,
)


class TestBuildPayload:

    def test_landscape_flux_schnell(self):
        payload = _build_payload("fal-ai/flux/schnell", "a cat", "landscape")
        assert payload["prompt"] == "a cat"
        assert payload["image_size"] == "landscape_16_9"

    def test_square_flux_schnell(self):
        payload = _build_payload("fal-ai/flux/schnell", "a cat", "square")
        assert payload["image_size"] == "square_hd"

    def test_portrait_flux_schnell(self):
        payload = _build_payload("fal-ai/flux/schnell", "a cat", "portrait")
        assert payload["image_size"] == "portrait_16_9"

    def test_invalid_aspect_defaults_to_landscape(self):
        payload = _build_payload("fal-ai/flux/schnell", "a cat", "invalid")
        assert payload["image_size"] == "landscape_16_9"

    def test_only_supported_keys_in_payload(self):
        payload = _build_payload("fal-ai/ideogram/v3", "hello", "square")
        for key in payload:
            assert key in FAL_MODELS["fal-ai/ideogram/v3"]["supports"]


class TestFalImageGenTool:

    def test_is_ready_with_key(self):
        tool = FalImageGenTool(fal_key="key-123")
        assert tool.is_ready() is True

    def test_is_ready_without_key(self):
        tool = FalImageGenTool(fal_key="")
        assert tool.is_ready() is False

    def test_default_model(self):
        tool = FalImageGenTool(fal_key="key-123")
        assert tool._model == DEFAULT_MODEL

    def test_custom_model(self):
        tool = FalImageGenTool(fal_key="key-123", model="fal-ai/flux-2-pro")
        assert tool._model == "fal-ai/flux-2-pro"

    @pytest.mark.asyncio
    async def test_execute_no_key(self):
        tool = FalImageGenTool(fal_key="")
        result = await tool.execute({"prompt": "test"})
        assert result.success is False
        assert "not configured" in result.error

    @pytest.mark.asyncio
    async def test_execute_unknown_model_errors(self):
        tool = FalImageGenTool(fal_key="key-123", model="fal-ai/nonexistent-model")
        result = await tool.execute({"prompt": "a cat"})
        assert result.success is False
        assert "Unknown FAL model" in result.error
        assert "fal-ai/flux/schnell" in result.error

    @pytest.mark.asyncio
    async def test_execute_empty_prompt(self):
        tool = FalImageGenTool(fal_key="key-123")
        result = await tool.execute({"prompt": ""})
        assert result.success is False
        assert "required" in result.error

    @pytest.mark.asyncio
    async def test_execute_import_error(self):
        tool = FalImageGenTool(fal_key="key-123")
        with patch("echo_agent.agent.tools.image_gen_fal._load_fal_client", side_effect=ImportError("no fal")):
            result = await tool.execute({"prompt": "a cat"})
        assert result.success is False
        assert "fal-client" in result.error

    @pytest.mark.asyncio
    async def test_execute_success(self):
        tool = FalImageGenTool(fal_key="key-123", model="fal-ai/flux/schnell")
        fake_handler = MagicMock()
        fake_handler.get.return_value = {
            "images": [{"url": "https://fal.ai/result.png", "width": 1024, "height": 576}]
        }
        fake_fal = MagicMock()
        fake_fal.submit.return_value = fake_handler

        with patch("echo_agent.agent.tools.image_gen_fal._load_fal_client", return_value=fake_fal):
            result = await tool.execute({"prompt": "a beautiful sunset", "aspect_ratio": "landscape"})

        assert result.success is True
        assert "https://fal.ai/result.png" in result.output
        assert result.metadata["url"] == "https://fal.ai/result.png"

    @pytest.mark.asyncio
    async def test_execute_api_error(self):
        tool = FalImageGenTool(fal_key="key-123")
        fake_fal = MagicMock()
        fake_fal.submit.side_effect = RuntimeError("connection failed")

        with patch("echo_agent.agent.tools.image_gen_fal._load_fal_client", return_value=fake_fal):
            result = await tool.execute({"prompt": "a cat"})

        assert result.success is False
        assert "connection failed" in result.error
