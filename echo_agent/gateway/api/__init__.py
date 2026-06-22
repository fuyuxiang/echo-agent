"""Management API routes for the desktop client.

All routes are registered under the gateway's api_prefix (default /api).
Authentication reuses the existing GatewayAuth token check.
"""

from __future__ import annotations

from typing import TYPE_CHECKING

from aiohttp import web

if TYPE_CHECKING:
    from echo_agent.gateway.server import GatewayServer


def register_management_routes(app: web.Application, prefix: str, server: GatewayServer) -> None:
    from echo_agent.gateway.api.memory import MemoryAPI
    from echo_agent.gateway.api.skills import SkillsAPI
    from echo_agent.gateway.api.channels import ChannelsAPI
    from echo_agent.gateway.api.knowledge import KnowledgeAPI
    from echo_agent.gateway.api.chat_attachments import ChatAttachmentAPI
    from echo_agent.gateway.api.config import ConfigAPI
    from echo_agent.gateway.api.lifecycle import LifecycleAPI

    memory_api = MemoryAPI(server)
    skills_api = SkillsAPI(server)
    channels_api = ChannelsAPI(server)
    knowledge_api = KnowledgeAPI(server)
    chat_attachment_api = ChatAttachmentAPI(server)
    config_api = ConfigAPI(server)
    lifecycle_api = LifecycleAPI(server)

    app.router.add_get(f"{prefix}/memory", memory_api.list_entries)
    app.router.add_get(f"{prefix}/memory/stats", memory_api.stats)
    app.router.add_post(f"{prefix}/memory/search", memory_api.search)
    app.router.add_get(f"{prefix}/memory/{{id}}", memory_api.get_entry)
    app.router.add_put(f"{prefix}/memory/{{id}}", memory_api.update_entry)
    app.router.add_delete(f"{prefix}/memory/{{id}}", memory_api.delete_entry)

    app.router.add_get(f"{prefix}/skills", skills_api.list_skills)
    app.router.add_post(f"{prefix}/skills/import", skills_api.import_skill)
    app.router.add_get(f"{prefix}/skills/{{name}}", skills_api.get_skill)
    app.router.add_get(f"{prefix}/skills/{{name}}/deps", skills_api.get_skill_deps)
    app.router.add_post(f"{prefix}/skills/{{name}}/deps/install", skills_api.install_skill_deps)
    app.router.add_post(f"{prefix}/skills/{{name}}/toggle", skills_api.toggle_skill)
    app.router.add_delete(f"{prefix}/skills/{{name}}", skills_api.delete_skill)

    app.router.add_get(f"{prefix}/channels", channels_api.list_channels)

    app.router.add_get(f"{prefix}/knowledge/status", knowledge_api.get_status)
    app.router.add_post(f"{prefix}/knowledge/rebuild", knowledge_api.rebuild)
    app.router.add_post(f"{prefix}/knowledge/upload", knowledge_api.upload)
    app.router.add_get(f"{prefix}/knowledge/documents", knowledge_api.list_documents)
    app.router.add_delete(f"{prefix}/knowledge/documents/{{path}}", knowledge_api.delete_document)

    app.router.add_post(f"{prefix}/chat/attachments", chat_attachment_api.upload)

    app.router.add_get(f"{prefix}/config", config_api.get_config)
    app.router.add_get(f"{prefix}/config/models", config_api.get_models)

    app.router.add_post(f"{prefix}/shutdown", lifecycle_api.shutdown)
