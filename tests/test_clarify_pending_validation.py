"""A rejected clarify call must not leave a pending request behind.

_prepare_clarify runs before ToolRegistry.execute, which is where parameters are
actually validated. So a malformed call (options as the literal JSON *text*,
question as a dict) registered a pending request first and was rejected second,
leaving state nothing would ever resolve:

- CLI: a live ClarifyRequest whose id was never handed to the tool, so
  wait_for_answer is never called and resolve() never comes.
- IM: worse than cosmetic. _on_inbound binds the next inbound message on that
  session as the answer to a question the user was never asked, so their next
  message is swallowed as a reply to nothing.

The fix delegates to the tool's own validate_params, so the predicate here stays
"would execute() reject this?" rather than a second copy of the rules.
"""
from __future__ import annotations

from types import SimpleNamespace

import pytest

from echo_agent.agent.clarify_manager import ClarifyManager
from echo_agent.agent.pipeline.inference_stage import InferenceStage
from echo_agent.agent.tools.clarify import ClarifyTool
from echo_agent.agent.tools.registry import ToolRegistry


def _stage(clarify: ClarifyManager) -> InferenceStage:
    """An InferenceStage with only the collaborators _prepare_clarify touches.

    Constructed via __new__ rather than the real __init__: the constructor wants
    the whole pipeline (provider, router, memory, credentials…), none of which
    this path reads.
    """
    registry = ToolRegistry()
    registry.register(ClarifyTool(clarify))
    stage = InferenceStage.__new__(InferenceStage)
    stage._clarify = clarify
    stage._tools = registry
    stage._cog = None
    return stage


def _call(arguments: dict) -> SimpleNamespace:
    return SimpleNamespace(name="clarify", arguments=arguments)


def _event(channel: str) -> SimpleNamespace:
    return SimpleNamespace(
        channel=channel, session_key=f"{channel}:u1", sender_id="u1",
    )


# ── IM path: a stale pending swallows the user's next message ────────────────

@pytest.mark.asyncio
@pytest.mark.parametrize(
    "arguments",
    [
        # options emitted as its JSON text instead of an array — the exact shape
        # validate_params rejects for an "array" parameter.
        {"question": "选哪个?", "options": "['甲','乙']"},
        {"question": {"text": "选哪个?"}},  # wrong type for a string parameter
        {"options": ["甲", "乙"]},  # missing the required question
    ],
    ids=["options_as_string", "question_as_dict", "missing_question"],
)
async def test_malformed_im_clarify_registers_no_pending(arguments):
    clarify = ClarifyManager()
    await _stage(clarify)._prepare_clarify(_call(arguments), _event("weixin"))
    assert clarify.take_im_pending("weixin:u1", ttl_seconds=300) is None


@pytest.mark.asyncio
async def test_wellformed_im_clarify_still_registers_pending():
    """Positive control: the fix must not stop valid calls from registering."""
    clarify = ClarifyManager()
    await _stage(clarify)._prepare_clarify(
        _call({"question": "选哪个?", "options": ["甲", "乙"]}), _event("weixin"),
    )
    pending = clarify.take_im_pending("weixin:u1", ttl_seconds=300)
    assert pending is not None
    assert pending.question == "选哪个?"
    assert pending.options == ["甲", "乙"]


@pytest.mark.asyncio
async def test_im_clarify_without_options_is_valid():
    """options is optional — an open-ended question must still register."""
    clarify = ClarifyManager()
    await _stage(clarify)._prepare_clarify(
        _call({"question": "你想怎么做?"}), _event("weixin"),
    )
    assert clarify.take_im_pending("weixin:u1", ttl_seconds=300) is not None


# ── CLI path: no orphaned ClarifyRequest, no injected id ─────────────────────

@pytest.mark.asyncio
async def test_malformed_cli_clarify_creates_no_request():
    clarify = ClarifyManager()
    call = _call({"question": "选哪个?", "options": "['甲','乙']"})
    await _stage(clarify)._prepare_clarify(call, _event("gateway:cli"))

    # No id injected, so the tool self-registers or fails on its own terms
    # instead of blocking on an id the pipeline invented.
    assert "_clarify_id" not in call.arguments
    assert clarify.get("") is None


@pytest.mark.asyncio
async def test_wellformed_cli_clarify_injects_an_id():
    clarify = ClarifyManager()
    call = _call({"question": "选哪个?", "options": ["甲", "乙"]})
    await _stage(clarify)._prepare_clarify(call, _event("gateway:cli"))

    clarify_id = call.arguments.get("_clarify_id")
    assert clarify_id
    request = clarify.get(clarify_id)
    assert request is not None
    assert request.question == "选哪个?"
