"""Tool-line vocabulary: tool id -> localized verb, which param is the operand,
and how to word a result. Pure functions with no UI dependency, shared by the
Textual transcript and the inline renderer.
"""

from __future__ import annotations

import os

from echo_agent.cli.i18n import get_locale, t
from echo_agent.cli.render.text import clip

_TOOL_VERB_ZH = {
    "read_file": "读取", "write_file": "写入", "edit_file": "编辑",
    "patch": "打补丁", "list_dir": "列出", "search_files": "搜索",
    "session_search": "检索会话", "knowledge_search": "查知识库",
    "exec": "执行", "process": "运行进程", "web_fetch": "抓取网页",
    "web_search": "联网搜索", "memory": "记忆", "todo": "更新待办",
    "read_document": "读取文档", "read_spill": "读取完整结果",
    "execute_code": "运行代码", "browser": "操作浏览器",
    "clarify": "询问", "cronjob": "管理定时任务",
    "delegate_task": "委派任务", "spawn_task": "启动后台任务",
    "image_generate": "生成图片", "vision_analyze": "分析图片",
    "knowledge_index": "更新知识库", "message": "发送消息",
    "notify": "发送通知", "send_file": "发送文件",
    "skills_list": "列出技能", "skill_view": "查看技能",
    "skill_manage": "管理技能", "skill_install": "安装技能",
    "skill_run": "运行技能", "task": "管理任务",
    "workflow": "管理工作流", "text_to_speech": "生成语音",
}

_TOOL_VERB_EN = {
    "read_file": "Read", "write_file": "Write", "edit_file": "Edit",
    "patch": "Patch", "list_dir": "List", "search_files": "Search",
    "session_search": "Search sessions", "knowledge_search": "Search knowledge",
    "exec": "Execute", "process": "Run process", "web_fetch": "Fetch web page",
    "web_search": "Search web", "memory": "Memory", "todo": "Update tasks",
    "read_document": "Read document", "read_spill": "Read full result",
    "execute_code": "Run code", "browser": "Use browser", "clarify": "Ask",
    "cronjob": "Manage scheduled job", "delegate_task": "Delegate task",
    "spawn_task": "Start background task", "image_generate": "Generate image",
    "vision_analyze": "Analyze image", "knowledge_index": "Update knowledge",
    "message": "Send message", "notify": "Send notification", "send_file": "Send file",
    "skills_list": "List skills", "skill_view": "View skill", "skill_manage": "Manage skill",
    "skill_install": "Install skill", "skill_run": "Run skill", "task": "Manage task",
    "workflow": "Manage workflow", "text_to_speech": "Generate speech",
}
# Backward-compatible inspection export; rendering goes through humanize_tool.
_TOOL_VERB = _TOOL_VERB_ZH

# 每个工具用哪个参数当"操作对象"。缺省走兜底：第一个字符串参数。
_OBJECT_KEY = {
    "read_file": "path", "write_file": "path", "edit_file": "path",
    "patch": "path", "list_dir": "path", "exec": "command",
    "process": "command", "web_fetch": "url", "web_search": "query",
    "read_document": "path", "read_spill": "path",
    "execute_code": "language", "browser": "url", "clarify": "question",
    "cronjob": "name", "delegate_task": "goal", "spawn_task": "task",
    "image_generate": "prompt", "vision_analyze": "image",
    "knowledge_search": "query", "knowledge_index": "path",
    "message": "text", "notify": "text", "send_file": "path",
    "skill_view": "name", "skill_manage": "name", "skill_install": "name",
    "skill_run": "name", "task": "task_id", "workflow": "workflow_id",
    "text_to_speech": "text", "memory": "action", "todo": "action",
}

_ACTION_TOOLS = frozenset({"browser", "cronjob", "task", "workflow", "memory", "todo"})

_RISK_LABEL_ZH = {
    "read_only": "只读",
    "write": "会修改数据",
    "exec": "会执行代码或命令",
    "dangerous": "高风险操作",
}

_RISK_LABEL_EN = {
    "read_only": "read-only",
    "write": "modifies data",
    "exec": "executes code or commands",
    "dangerous": "high-risk operation",
}


def humanize_tool(name: str) -> str:
    """Tool id -> localized verb. Unknown tools fall back to the raw id."""
    labels = _TOOL_VERB_ZH if get_locale() == "zh" else _TOOL_VERB_EN
    return labels.get(name, name)


def humanize_risk(risk: str) -> str:
    """Turn an internal risk enum into language a user can decide from."""
    raw = str(risk or "").strip()
    labels = _RISK_LABEL_ZH if get_locale() == "zh" else _RISK_LABEL_EN
    return labels.get(raw.lower(), raw)


def fmt_duration_ms(ms: int | None) -> str:
    """Human duration for a tool line, or "" when it isn't worth a column.

    Anything under a second is dropped: on a transcript where most calls are
    instant reads, "0.1s" on every line is noise that hides the one call that
    actually took half a minute.
    """
    if ms is None:
        return ""
    try:
        value = float(ms)
    except (TypeError, ValueError):
        return ""
    if value < 1000:
        return ""
    seconds = value / 1000
    if seconds < 60:
        return f"{seconds:.1f}s"
    minutes, rest = divmod(int(seconds), 60)
    return f"{minutes}m {rest}s"


def pick_object(name: str, params: dict) -> str:
    """The primary argument shown as the tool's operand."""
    params = params or {}
    if name == "search_files":
        pat = params.get("pattern")
        compact = " ".join(str(pat).split()) if pat else ""
        return f'"{clip(compact, 46)}"' if compact else ""
    key = _OBJECT_KEY.get(name)
    val = params.get(key) if key else None
    if name in _ACTION_TOOLS:
        action = str(params.get("action", "")).strip()
        target = str(val or "").strip()
        if action and target and target != action:
            val = f"{action} {target}"
        elif action:
            val = action
    if val is None:
        # Fallback: first string-valued argument.
        val = next((v for v in params.values() if isinstance(v, str)), "")
    if name in ("read_file", "write_file", "edit_file", "patch") and val:
        val = os.path.basename(str(val))
    # A model can pass a multiline command/prompt. Process summaries must stay
    # one physical terminal row; expanded details retain the original payload.
    compact = " ".join(str(val).split()) if val else ""
    return clip(compact, 48) if compact else ""


def summarize_result(
    name: str, result_meta: dict | None, result_text: str, success: bool
) -> str:
    """Turn the producer-supplied count (result_meta) into Chinese words;
    fall back to a text preview. Never recount on the truncated result_text."""
    compact_result = " ".join(str(result_text or "").split())
    if not success:
        reason = clip(compact_result, 72)
        return t("attach.ui.failed_reason", reason=reason) if reason else t("attach.ui.failed")
    meta = result_meta or {}
    if name == "read_file" and "total_lines" in meta:
        return t("attach.ui.lines", count=meta["total_lines"])
    if name == "search_files" and "count" in meta:
        return t("attach.ui.matches", count=meta["count"])
    if name == "list_dir" and "count" in meta:
        return t("attach.ui.items", count=meta["count"])
    if name in ("exec", "process"):
        return t("attach.ui.done")
    preview = clip(compact_result, 40)
    return preview or t("attach.ui.done")
