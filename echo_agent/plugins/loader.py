"""Plugin loader — multi-source discovery and module loading."""

from __future__ import annotations

import importlib
import importlib.metadata
import importlib.util
import sys
import types
from pathlib import Path
from typing import Any

from loguru import logger

from echo_agent.plugins.errors import PluginLoadError
from echo_agent.plugins.manifest import (
    PluginManifest,
    PluginRecord,
    parse_manifest,
)

ENTRY_POINTS_GROUP = "echo_agent.plugins"
_NS_PARENT = "echo_agent_plugins"


def discover_all(
    *,
    workspace: Path,
    extra_dirs: list[str] | None = None,
) -> list[PluginRecord]:
    """Discover plugins from all sources.

    Priority (later overrides earlier on name collision):
    1. pip entry_points
    2. User directory (~/.echo-agent/plugins/)
    3. Project directory ({workspace}/plugins/)
    4. Extra directories from config
    """
    records: dict[str, PluginRecord] = {}

    for record in _scan_entry_points():
        records[record.manifest.name] = record

    user_dir = Path.home() / ".echo-agent" / "plugins"
    for record in _scan_directory(user_dir, source="user"):
        records[record.manifest.name] = record

    project_dir = workspace / "plugins"
    for record in _scan_directory(project_dir, source="project"):
        records[record.manifest.name] = record

    for extra in extra_dirs or []:
        extra_path = Path(extra).expanduser().resolve()
        for record in _scan_directory(extra_path, source="extra"):
            records[record.manifest.name] = record

    return list(records.values())


def _scan_entry_points() -> list[PluginRecord]:
    """Discover plugins registered via pip entry_points."""
    records: list[PluginRecord] = []
    try:
        eps = importlib.metadata.entry_points(group=ENTRY_POINTS_GROUP)
    except TypeError:
        eps = importlib.metadata.entry_points().get(ENTRY_POINTS_GROUP, [])

    for ep in eps:
        try:
            module_or_dict = ep.load()
            manifest = _manifest_from_entrypoint(ep.name, module_or_dict)
            record = PluginRecord(
                manifest=manifest,
                source="entrypoint",
                path=None,
                module=module_or_dict,
            )
            records.append(record)
            logger.debug("Discovered entry_point plugin: {}", ep.name)
        except Exception as e:
            logger.warning("Failed to load plugin entry_point '{}': {}", ep.name, e)
    return records


def _manifest_from_entrypoint(name: str, module_or_dict: Any) -> PluginManifest:
    """Extract manifest from an entry_point target.

    The target can be:
    - A dict with 'activate'/'deactivate' keys (and optional 'manifest' dict)
    - A module with a 'plugin' dict attribute or 'activate' function
    """
    if isinstance(module_or_dict, dict):
        manifest_data = module_or_dict.get("manifest", {})
        if not manifest_data.get("name"):
            manifest_data["name"] = name
        return PluginManifest(**manifest_data)

    if isinstance(module_or_dict, types.ModuleType):
        plugin_attr = getattr(module_or_dict, "plugin", None)
        if isinstance(plugin_attr, dict):
            manifest_data = plugin_attr.get("manifest", {})
            if not manifest_data.get("name"):
                manifest_data["name"] = name
            return PluginManifest(**manifest_data)
        manifest_attr = getattr(module_or_dict, "MANIFEST", None)
        if isinstance(manifest_attr, dict):
            if not manifest_attr.get("name"):
                manifest_attr["name"] = name
            return PluginManifest(**manifest_attr)

    return PluginManifest(name=name)


def _scan_directory(directory: Path, source: str) -> list[PluginRecord]:
    """Scan a directory for plugin subdirectories containing plugin.yaml."""
    records: list[PluginRecord] = []
    if not directory.is_dir():
        return records

    for candidate in sorted(directory.iterdir()):
        if not candidate.is_dir():
            continue
        manifest_file = candidate / "plugin.yaml"
        if not manifest_file.exists():
            manifest_file = candidate / "plugin.yml"
        if not manifest_file.exists():
            continue
        try:
            manifest = parse_manifest(manifest_file)
            record = PluginRecord(
                manifest=manifest,
                source=source,
                path=candidate,
            )
            records.append(record)
            logger.debug("Discovered {} plugin: {} at {}", source, manifest.name, candidate)
        except Exception as e:
            logger.warning("Failed to parse manifest at {}: {}", manifest_file, e)
    return records


def load_plugin_module(record: PluginRecord) -> Any:
    """Load the plugin module and return the activate/deactivate callables.

    Returns a dict with 'activate' and optional 'deactivate' keys.
    """
    if record.source == "entrypoint":
        return _resolve_plugin_interface(record.manifest.name, record.module)

    if record.path is None:
        raise PluginLoadError(record.manifest.name, "No path for directory plugin")

    init_file = record.path / "__init__.py"
    if not init_file.exists():
        raise PluginLoadError(
            record.manifest.name,
            f"Missing __init__.py in {record.path}",
        )

    module = _load_directory_module(record.manifest.name, record.path)
    record.module = module
    return _resolve_plugin_interface(record.manifest.name, module)


def _load_directory_module(plugin_name: str, plugin_dir: Path) -> types.ModuleType:
    """Import a directory plugin as a Python module."""
    if _NS_PARENT not in sys.modules:
        ns_pkg = types.ModuleType(_NS_PARENT)
        ns_pkg.__path__ = []
        ns_pkg.__package__ = _NS_PARENT
        sys.modules[_NS_PARENT] = ns_pkg

    slug = plugin_name.replace("-", "_").replace("/", "__")
    module_name = f"{_NS_PARENT}.{slug}"

    if module_name in sys.modules:
        return sys.modules[module_name]

    init_file = plugin_dir / "__init__.py"
    spec = importlib.util.spec_from_file_location(
        module_name,
        init_file,
        submodule_search_locations=[str(plugin_dir)],
    )
    if spec is None or spec.loader is None:
        raise PluginLoadError(plugin_name, f"Cannot create module spec from {init_file}")

    module = importlib.util.module_from_spec(spec)
    module.__package__ = module_name
    module.__path__ = [str(plugin_dir)]
    sys.modules[module_name] = module

    try:
        spec.loader.exec_module(module)
    except Exception as e:
        sys.modules.pop(module_name, None)
        raise PluginLoadError(plugin_name, f"Failed to execute module: {e}") from e

    return module


def _resolve_plugin_interface(plugin_name: str, target: Any) -> dict[str, Any]:
    """Resolve the plugin interface (activate/deactivate) from a loaded target."""
    if isinstance(target, dict):
        activate = target.get("activate")
        deactivate = target.get("deactivate")
    elif isinstance(target, types.ModuleType):
        plugin_dict = getattr(target, "plugin", None)
        if isinstance(plugin_dict, dict):
            activate = plugin_dict.get("activate")
            deactivate = plugin_dict.get("deactivate")
        else:
            activate = getattr(target, "activate", None)
            deactivate = getattr(target, "deactivate", None)
    else:
        raise PluginLoadError(plugin_name, f"Unexpected plugin target type: {type(target)}")

    if activate is None:
        raise PluginLoadError(plugin_name, "No activate() function found")

    return {"activate": activate, "deactivate": deactivate}


def topological_sort(records: list[PluginRecord]) -> list[PluginRecord]:
    """Sort plugins by depends_on relationships. Cycles are broken with a warning."""
    name_to_record = {r.manifest.name: r for r in records}
    visited: set[str] = set()
    result: list[PluginRecord] = []
    in_progress: set[str] = set()

    def visit(name: str) -> None:
        if name in visited:
            return
        if name in in_progress:
            logger.warning("Circular plugin dependency detected involving '{}'", name)
            return
        in_progress.add(name)
        record = name_to_record.get(name)
        if record:
            for dep in record.manifest.depends_on:
                if dep in name_to_record:
                    visit(dep)
            visited.add(name)
            in_progress.discard(name)
            result.append(record)

    for r in records:
        visit(r.manifest.name)

    return result
