"""Tool discovery and registration."""

from __future__ import annotations

from pathlib import Path
from typing import Any

from loguru import logger

from echo_agent.agent.tools.base import Tool
from echo_agent.agent.tools.filesystem import EditFileTool, ListDirTool, ReadFileTool, WriteFileTool
from echo_agent.agent.tools.message import MessageTool
from echo_agent.agent.tools.web import WebFetchTool, WebSearchTool
from echo_agent.bus.queue import MessageBus
from echo_agent.config.schema import Config
from echo_agent.models.provider import LLMProvider
from echo_agent.security.tool_policy import filter_tools_by_policy


def discover_tools(
    config: Config,
    workspace: Path,
    bus: MessageBus,
    provider: LLMProvider | None = None,
    scheduler: Any = None,
    session_manager: Any = None,
    skill_store: Any = None,
    memory_store: Any = None,
    contradiction_detector: Any = None,
    task_manager: Any = None,
    workflow_engine: Any = None,
    knowledge_index: Any = None,
    approval: Any = None,
) -> list[Tool]:
    ws = str(workspace)
    restrict = config.tools.restrict_to_workspace
    safe_write_root = config.tools.safe_write_root
    tools: list[Tool] = []
    executor = None

    if config.tools.exec.enabled:
        from echo_agent.agent.executors.factory import create_executor
        from echo_agent.agent.tools.shell import ShellTool
        executor = create_executor(config.execution, workspace, host=config.tools.exec.host)
        tools.append(ShellTool(
            ws,
            allowed=config.tools.exec.allowed_commands,
            blocked=config.tools.exec.blocked_commands,
            max_output=config.tools.exec.max_output_chars,
            executor=executor,
            exec_policy=config.tools.exec,
            network_policy=config.execution.network_policy,
        ))
    tools.append(ReadFileTool(ws, restrict))
    tools.append(WriteFileTool(ws, restrict, safe_write_root))
    tools.append(EditFileTool(ws, restrict, safe_write_root))
    tools.append(ListDirTool(ws, restrict))
    from echo_agent.agent.tools.document import ReadDocumentTool
    tools.append(ReadDocumentTool(ws, restrict))
    if config.tools.web.enabled and config.execution.network_policy != "deny":
        tools.append(WebFetchTool(proxy=config.tools.web.proxy, allow_private=config.tools.web.allow_private_addresses))
        if config.tools.web.search_api_key or config.tools.web.search_provider == "searxng":
            tools.append(WebSearchTool(
                api_key=config.tools.web.search_api_key,
                provider=config.tools.web.search_provider,
                api_base=config.tools.web.search_api_base,
                proxy=config.tools.web.proxy,
                timeout_seconds=config.tools.web.timeout_seconds,
            ))
    if config.tools.browser.enabled and config.execution.network_policy != "deny":
        from echo_agent.agent.tools.browser import BrowserTool
        from echo_agent.agent.browser.session import manager as browser_manager
        tools.append(BrowserTool(config=config.tools.browser, manager=browser_manager))
    tools.append(MessageTool(publish_fn=bus.publish_outbound))

    from echo_agent.agent.tools.send_file import SendFileTool
    tools.append(SendFileTool(ws, restrict, publish_fn=bus.publish_outbound))

    from echo_agent.agent.tools.search import SearchFilesTool
    tools.append(SearchFilesTool(ws, restrict))

    from echo_agent.agent.tools.patch import PatchTool
    tools.append(PatchTool(ws, restrict, safe_write_root))

    from echo_agent.agent.tools.todo import TodoTool
    tools.append(TodoTool(store_dir=workspace / "data" / "todos"))

    if task_manager:
        from echo_agent.agent.tools.task import TaskTool
        tools.append(TaskTool(manager=task_manager))

    if workflow_engine:
        from echo_agent.agent.tools.workflow import WorkflowTool
        tools.append(WorkflowTool(engine=workflow_engine))

    from echo_agent.agent.tools.clarify import ClarifyTool
    tools.append(ClarifyTool(bus=bus))

    from echo_agent.agent.tools.notify import NotifyTool
    tools.append(NotifyTool(bus=bus))

    if config.tools.exec.enabled and config.tools.code_exec.enabled:
        from echo_agent.agent.tools.code_exec import CodeExecTool
        tools.append(CodeExecTool(
            ws,
            executor=executor,
            allowed_languages=config.tools.code_exec.allowed_languages,
            max_output=config.tools.exec.max_output_chars,
            timeout_seconds=config.tools.code_exec.timeout_seconds,
            exec_policy=config.tools.exec,
            network_policy=config.execution.network_policy,
        ))

    if config.tools.exec.enabled:
        from echo_agent.agent.tools.process import ProcessTool
        tools.append(ProcessTool(
            ws,
            exec_policy=config.tools.exec,
            network_policy=config.execution.network_policy,
        ))

    # NOTE: spawn_task is NOT registered here. It needs the approval gate and
    # credential manager to run its background worker's tool calls safely, and
    # those are only available later — so it is constructed in
    # AgentLoop._setup_delegation alongside delegate_task.
    if provider:
        from echo_agent.agent.tools.vision import VisionTool
        tools.append(VisionTool(provider=provider, workspace=ws))

    _try_register_image_gen(tools, config, provider)
    _try_register_tts(tools, config, ws, provider, publish_fn=bus.publish_outbound)

    if session_manager:
        from echo_agent.agent.tools.session_search import SessionSearchTool
        tools.append(SessionSearchTool(session_manager=session_manager))

    if scheduler:
        from echo_agent.agent.tools.cronjob import CronjobTool
        tools.append(CronjobTool(scheduler=scheduler))

    if skill_store:
        from echo_agent.agent.tools.skills import SkillsListTool, SkillViewTool, SkillManageTool
        from echo_agent.agent.tools.skill_install import SkillInstallTool
        tools.append(SkillsListTool(store=skill_store))
        tools.append(SkillViewTool(store=skill_store, approval=approval, bus=bus, config=config))
        tools.append(SkillManageTool(store=skill_store))
        tools.append(SkillInstallTool(store=skill_store))

    if memory_store:
        from echo_agent.agent.tools.memory import MemoryTool
        tools.append(MemoryTool(store=memory_store, contradiction_detector=contradiction_detector))

    if knowledge_index:
        from echo_agent.agent.tools.knowledge import KnowledgeIndexTool, KnowledgeSearchTool
        tools.append(KnowledgeSearchTool(index=knowledge_index, default_limit=config.knowledge.max_results))
        tools.append(KnowledgeIndexTool(index=knowledge_index))

    tools = filter_tools_by_policy(config, tools)
    logger.info("Discovered {} tools after policy filtering", len(tools))
    return tools


def _unwrap_provider(provider: LLMProvider | None) -> LLMProvider | None:
    """Unwrap decorator layers (RateLimitedProvider, _PooledProvider) to get the real provider."""
    if provider is None:
        return None
    inner = provider
    while hasattr(inner, "_inner"):
        inner = inner._inner
    return inner


def _is_openai_compatible_provider(provider: LLMProvider | None) -> bool:
    """Check if provider uses the OpenAI-compatible API (images/audio endpoints available)."""
    from echo_agent.models.providers.openai_provider import OpenAIProvider
    inner = _unwrap_provider(provider)
    return isinstance(inner, OpenAIProvider)


def _infer_image_model(api_base: str) -> str:
    """Infer image generation model name from API base URL."""
    base = (api_base or "").lower()
    if "minimax" in base:
        return "image-01"
    if "dashscope" in base or "aliyun" in base:
        return "wanx-v1"
    if "zhipu" in base or "bigmodel" in base:
        return "cogview-3"
    return "dall-e-3"


def _infer_tts_model(api_base: str) -> str:
    """Infer TTS model name from API base URL."""
    base = (api_base or "").lower()
    if "minimax" in base:
        return "speech-02"
    if "dashscope" in base or "aliyun" in base:
        return "cosyvoice-v1"
    return "tts-1"


def _try_register_image_gen(tools: list[Tool], config: Config, provider: LLMProvider | None = None) -> None:
    ig = getattr(config.tools, "image_gen", None)
    backend = getattr(ig, "backend", "openai") if ig else "openai"

    if backend == "fal":
        fal_key = getattr(ig, "fal_key", "") if ig else ""
        fal_model = getattr(ig, "fal_model", "") if ig else ""
        if not fal_key:
            logger.info(
                "image_generate tool not registered: no fal_key configured. "
                "Set tools.image_gen.fal_key and tools.image_gen.fal_model in config to enable."
            )
            return
        from echo_agent.agent.tools.image_gen_fal import FalImageGenTool, FAL_MODELS
        if fal_model and fal_model not in FAL_MODELS:
            logger.warning(
                "image_generate: configured fal_model '{}' is not in the built-in catalog. "
                "Supported: {}. The tool will error at execution time.",
                fal_model, ", ".join(sorted(FAL_MODELS.keys())),
            )
        tools.append(FalImageGenTool(fal_key=fal_key, model=fal_model))
        return

    api_key = getattr(ig, "api_key", "") if ig else ""
    api_base = getattr(ig, "api_base", "") if ig else ""
    model = getattr(ig, "model", "") if ig else ""

    if not api_key:
        if _is_openai_compatible_provider(provider):
            logger.info(
                "image_generate tool not registered: no explicit image_gen.api_key configured. "
                "Set tools.image_gen.api_key, tools.image_gen.api_base, and tools.image_gen.model in config to enable."
            )
        return

    if not model:
        model = _infer_image_model(api_base)
        logger.debug("image_generate: model not configured, inferred '{}' from api_base", model)

    from echo_agent.agent.tools.image_gen import ImageGenTool
    tools.append(ImageGenTool(api_key=api_key, api_base=api_base, model=model))


def _try_register_tts(tools: list[Tool], config: Config, ws: str, provider: LLMProvider | None = None, publish_fn=None) -> None:
    from echo_agent.agent.tools.tts import TTSTool
    tts_cfg = getattr(config.tools, "tts", None)
    openai_key = getattr(tts_cfg, "openai_api_key", "") if tts_cfg else ""
    openai_base = getattr(tts_cfg, "openai_api_base", "") if tts_cfg else ""
    tts_model = getattr(tts_cfg, "model", "") if tts_cfg else ""
    default_backend = getattr(tts_cfg, "default_backend", "edge") if tts_cfg else "edge"
    default_voice = getattr(tts_cfg, "default_voice", "") if tts_cfg else ""

    if not openai_key and _is_openai_compatible_provider(provider):
        logger.info(
            "TTS openai backend not available: no explicit tts.openai_api_key configured. "
            "Set tools.tts.openai_api_key, tools.tts.openai_api_base, and tools.tts.model in config to enable. "
            "edge backend remains available without configuration."
        )

    if not tts_model and openai_key:
        tts_model = _infer_tts_model(openai_base)
        logger.debug("TTS: model not configured, inferred '{}' from openai_api_base", tts_model)
    if not tts_model:
        tts_model = "tts-1"

    tools.append(TTSTool(
        workspace=ws,
        openai_api_key=openai_key,
        openai_api_base=openai_base,
        tts_model=tts_model,
        default_backend=default_backend,
        default_voice=default_voice,
        publish_fn=publish_fn,
    ))
