"""Render the config reference (annotated YAML + Markdown) from field metadata.

Pure functions: build strings from iter_fields(); no disk access. Only
fields with status == "effective" are rendered — dead fields never reach
user-facing docs.

iter_fields() yields three path shapes:
  1. plain dotted leaf, e.g. ``memory.archivalThreshold``;
  2. container field itself, e.g. ``models.providers`` (empty list/dict default);
  3. container element subfield, marked with ``[]``/``{}``,
     e.g. ``models.providers[].apiKey`` or ``tools.mcpServers{}.command``.

render_yaml skips class 3 (the markers break YAML indentation) and emits
classes 1 and 2 as indented key/value pairs, so the output always parses.
render_markdown renders all three classes as table rows using the full path.
"""
from __future__ import annotations

from echo_agent.config.metadata import FieldInfo, iter_fields
from echo_agent.config.schema import Config


def _desc(info: FieldInfo, lang: str) -> str:
    key = "desc_zh" if lang == "zh" else "desc_en"
    return str(info.extra.get(key) or info.extra.get("desc_en") or "")


def _fmt_default(default: object) -> str:
    if default == "":
        return '""'
    if default is None:
        return "null"
    # bool 必须在任何 int 判断之前(Python 中 bool 是 int 子类)。
    if isinstance(default, bool):
        return "true" if default else "false"
    if isinstance(default, (list, dict)) and not default:
        return "[]" if isinstance(default, list) else "{}"
    return str(default)


def _is_element_field(path: str) -> bool:
    """True for container element subfields (class 3)."""
    return "[]" in path or "{}" in path


def _group_of(path: str) -> str:
    """Top-level group: first segment before ``.``/``[``/``{``."""
    for sep in (".", "[", "{"):
        idx = path.find(sep)
        if idx != -1:
            path = path[:idx]
    return path


def _effective_fields() -> list[FieldInfo]:
    return [f for f in iter_fields(Config) if f.extra.get("status") == "effective"]


def render_yaml(lang: str = "zh") -> str:
    lines: list[str] = []
    if lang == "zh":
        lines.append("# Echo Agent 配置参考(自动生成,请勿手改)")
    else:
        lines.append("# Echo Agent configuration reference (auto-generated, do not edit)")
    last_top = None
    emitted: set[str] = set()  # ancestor prefixes already written as mapping keys
    for info in _effective_fields():
        # Class 3 element fields break YAML indentation; skip them here.
        if _is_element_field(info.path):
            continue
        parts = info.path.split(".")
        top = parts[0]
        if top != last_top:
            lines.append("")
            lines.append(f"# ── {top} ──")
            last_top = top
        # Emit any missing ancestor mapping keys so the leaf nests legally.
        for depth in range(len(parts) - 1):
            prefix = ".".join(parts[: depth + 1])
            if prefix not in emitted:
                lines.append(f"{'  ' * depth}{parts[depth]}:")
                emitted.add(prefix)
        indent = "  " * (len(parts) - 1)
        desc = _desc(info, lang)
        if lang == "zh":
            suffix = f" (默认: {_fmt_default(info.default)})"
        else:
            suffix = f" (default: {_fmt_default(info.default)})"
        if info.choices:
            choice_str = "|".join(info.choices)
            suffix += f" [{choice_str}]"
        lines.append(f"{indent}# {desc}{suffix}")
        lines.append(f"{indent}{parts[-1]}: {_fmt_default(info.default)}")
    return "\n".join(lines) + "\n"


def render_markdown(lang: str = "zh") -> str:
    header_field = "字段" if lang == "zh" else "Field"
    title = "配置参考" if lang == "zh" else "Configuration Reference"
    header = f"# Echo Agent {title}\n"
    by_group: dict[str, list[FieldInfo]] = {}
    for info in _effective_fields():
        by_group.setdefault(_group_of(info.path), []).append(info)
    desc_col = "说明" if lang == "zh" else "description"
    blocks: list[str] = [header]
    for group, infos in by_group.items():
        blocks.append(f"## {group}\n")
        blocks.append(f"| {header_field} | snake | type | default | choices | {desc_col} |")
        blocks.append("|---|---|---|---|---|---|")
        for info in infos:
            choices = "/".join(info.choices) if info.choices else "—"
            blocks.append(
                f"| `{info.path}` | `{info.snake_path}` | {info.type_str} | "
                f"`{_fmt_default(info.default)}` | {choices} | {_desc(info, lang)} |"
            )
        blocks.append("")
    return "\n".join(blocks) + "\n"


def render_backlog() -> str:
    groups: dict[str, list[FieldInfo]] = {"fix": [], "remove": [], "keep": []}
    for f in iter_fields(Config):
        if f.extra.get("status") != "dead":
            continue
        disp = f.extra.get("disposition", "keep")
        groups.setdefault(disp, []).append(f)
    titles = {
        "fix": "## fix —— 该接线的功能/真 bug(子项目 C 处理,安全相关走快车道)",
        "remove": "## remove —— 纯孤儿字段,建议删除",
        "keep": "## keep —— 有意保留",
    }
    lines = ["# 配置死字段处置 backlog(自动生成,请勿手改)", ""]
    for disp in ("fix", "remove", "keep"):
        infos = groups.get(disp) or []
        if not infos:
            continue
        lines.append(titles[disp])
        lines.append("")
        lines.append("| 字段(snake) | reason |")
        lines.append("|---|---|")
        for f in infos:
            lines.append(f"| `{f.snake_path}` | {f.extra.get('reason','')} |")
        lines.append("")
    return "\n".join(lines) + "\n"
