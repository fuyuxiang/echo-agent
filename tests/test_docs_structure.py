"""CI guard: verify documentation structure consistency.

Checks that expected documentation files exist, Chinese/English pairs match,
and key content stays aligned with code.
"""
from __future__ import annotations

import re
from pathlib import Path

import pytest

DOCS_DIR = Path(__file__).resolve().parent.parent / "docs"

REQUIRED_SECTIONS = [
    "index.md",
    "getting-started/index.md",
    "getting-started/installation.md",
    "getting-started/quickstart.md",
    "guides/index.md",
    "guides/models/index.md",
    "concepts/index.md",
    "concepts/architecture.md",
    "integrations/index.md",
    "integrations/channels/index.md",
    "operations/index.md",
    "operations/troubleshooting.md",
    "reference/index.md",
    "reference/cli.md",
    "reference/configuration-guide.md",
    "reference/glossary.md",
    "development/index.md",
    "development/setup.md",
]


@pytest.mark.parametrize("rel_path", REQUIRED_SECTIONS)
def test_required_doc_exists(rel_path: str):
    path = DOCS_DIR / rel_path
    assert path.is_file(), f"必需文档缺失: docs/{rel_path}"


def _zh_en_pairs():
    """Yield (zh_path, en_path) for all .md files that should have .en.md counterparts."""
    for md in DOCS_DIR.rglob("*.md"):
        if md.name.endswith(".en.md"):
            continue
        if "superpowers" in md.parts or "includes" in md.parts:
            continue
        en = md.with_suffix("").with_suffix(".en.md")
        if en.exists():
            yield md, en


def test_channel_index_has_no_unimplemented_channels():
    """The overview must not advertise adapters that do not exist in code.

    An earlier revision listed SMS/Voice (Twilio) and a "Web Chat" channel that
    were never implemented, which is worse than an omission: readers configure
    them and get nothing.
    """
    from echo_agent.channels.manager import _CHANNEL_REGISTRY

    # Match on word boundaries: a bare "sms" substring also hits "mechanisms".
    phantom = ["twilio", "sms", "web chat"]
    known = set(_CHANNEL_REGISTRY)
    for index in ("index.md", "index.en.md"):
        path = DOCS_DIR / "integrations" / "channels" / index
        if not path.is_file():
            continue
        content = path.read_text(encoding="utf-8").lower()
        found = [
            name
            for name in phantom
            if name not in known and re.search(rf"\b{re.escape(name)}\b", content)
        ]
        assert not found, f"docs/integrations/channels/{index} 提到未实现的通道: {found}"


def _resolve_field(model: object, key: str):
    """Find a field by its snake_case name or its camelCase alias.

    The schema sets alias_generator=to_camel with populate_by_name, so both
    spellings are accepted at load time. Matching on field name alone would
    reject valid camelCase examples.
    """
    fields = getattr(model, "model_fields", None)
    if not fields:
        return None
    for name, field in fields.items():
        if key in (name, field.alias):
            return field
    return None


def _config_path_exists(parts: list[str]) -> bool:
    """Whether a dotted config path resolves against the real Config schema."""
    from echo_agent.config.schema import Config

    model: object = Config
    for index, part in enumerate(parts):
        field = _resolve_field(model, part)
        if field is None:
            return False
        annotation = field.annotation
        if hasattr(annotation, "model_fields"):
            model = annotation
        elif index < len(parts) - 1:
            return False  # more parts to walk but this one is a leaf
    return True


def _yaml_leaf_paths(data: dict, prefix: str = ""):
    for key, value in data.items():
        if isinstance(value, dict):
            yield from _yaml_leaf_paths(value, f"{prefix}{key}.")
        else:
            yield f"{prefix}{key}"


# Pages whose yaml blocks are checked against the schema. Restricted to pages
# that were rewritten from code so the guard cannot be silently weakened by
# adding a page full of invented keys; extend it as more pages are corrected.
# Config subtrees that are dicts keyed by user-chosen names, so their inner
# keys are data rather than schema fields.
FREEFORM_PREFIXES = (
    "cost.pricing_overrides.",
    "tools.mcp_servers.",
    "models.model_windows.",
    "gateway.platforms.",
)

SCHEMA_CHECKED_PAGES = [
    "guides/cost-control.md",
    "guides/cost-control.en.md",
    "guides/execution-backends.md",
    "guides/execution-backends.en.md",
    "reference/configuration-guide.md",
    "reference/configuration-guide.en.md",
    "reference/security-profile-matrix.md",
    "reference/security-profile-matrix.en.md",
    "reference/environment-variables.md",
    "reference/tools.md",
    "reference/tools.en.md",
    "concepts/security-model.md",
    "guides/sessions.md",
    "integrations/channels/index.md",
    "integrations/channels/index.en.md",
]


@pytest.mark.parametrize("rel_path", SCHEMA_CHECKED_PAGES)
def test_yaml_examples_match_config_schema(rel_path: str):
    """Config keys shown in these docs must exist in the schema.

    Pydantic ignores unknown keys, so a doc example with an invented field
    silently does nothing when a reader copies it — the failure mode this
    guards against is a config that looks applied but is not.
    """
    import yaml

    path = DOCS_DIR / rel_path
    if not path.is_file():
        pytest.skip(f"{rel_path} not present")
    text = path.read_text(encoding="utf-8")
    unknown: list[str] = []
    for block in re.findall(r"```ya?ml\n(.*?)```", text, re.S):
        try:
            data = yaml.safe_load(block)
        except yaml.YAMLError:
            continue
        if not isinstance(data, dict) or "services" in data:
            continue  # not an echo-agent config block (e.g. docker-compose)
        for dotted in _yaml_leaf_paths(data):
            # Free-form dicts keyed by user-chosen names (model ids, server
            # names) cannot be walked field by field.
            if any(dotted.startswith(p) for p in FREEFORM_PREFIXES):
                continue
            if not _config_path_exists(dotted.split(".")):
                unknown.append(dotted)
    assert not unknown, f"docs/{rel_path} 引用了不存在的配置项: {sorted(set(unknown))}"


@pytest.mark.parametrize("rel_path", SCHEMA_CHECKED_PAGES)
def test_yaml_examples_are_accepted_by_config(rel_path: str):
    """Doc examples must also survive validation, not just name real fields.

    Catches type errors a path check misses — notably YAML parsing bare `yes`
    as a bool for string-enum fields such as remote_strict_host_key.
    """
    import yaml

    from echo_agent.config.schema import Config

    path = DOCS_DIR / rel_path
    if not path.is_file():
        pytest.skip(f"{rel_path} not present")
    rejected: list[str] = []
    for block in re.findall(r"```ya?ml\n(.*?)```", path.read_text(encoding="utf-8"), re.S):
        try:
            data = yaml.safe_load(block)
        except yaml.YAMLError:
            continue
        if not isinstance(data, dict) or "services" in data:
            continue
        # Interpolation placeholders are resolved at load time, not by the model.
        if "${" in block:
            continue
        try:
            Config(**data)
        except Exception as exc:  # pydantic ValidationError and friends
            first = str(exc).splitlines()[1].strip() if "\n" in str(exc) else str(exc)
            rejected.append(first[:100])
    assert not rejected, f"docs/{rel_path} 的配置示例无法通过校验: {rejected}"


def test_documented_tool_names_are_real():
    """Tool names in reference/tools.md must match the registered tools.

    An earlier revision documented module filenames (code_exec, shell, tts)
    instead of registered tool names (execute_code, exec, text_to_speech),
    so every documented call would have failed.
    """
    import importlib
    import inspect
    import pkgutil

    import echo_agent.agent.tools as tools_pkg
    from echo_agent.tools.base import Tool

    real: set[str] = set()
    for module in pkgutil.iter_modules(tools_pkg.__path__):
        mod = importlib.import_module(f"echo_agent.agent.tools.{module.name}")
        for _, obj in inspect.getmembers(mod, inspect.isclass):
            if issubclass(obj, Tool) and obj is not Tool and getattr(obj, "name", ""):
                if obj.__module__ == mod.__name__:
                    real.add(obj.name)

    # Names that exist only in the policy tables, with no implementation.
    policy_only = {"agents_list", "agents_route"}

    for rel_path in ("reference/tools.md", "reference/tools.en.md"):
        path = DOCS_DIR / rel_path
        if not path.is_file():
            continue
        documented = set(
            re.findall(r"^#### `?([a-z_][a-z0-9_]*)`?\s*$", path.read_text(encoding="utf-8"), re.M)
        )
        bogus = documented - real - policy_only
        assert not bogus, f"docs/{rel_path} 记录了不存在的工具: {sorted(bogus)}"
        missing = real - documented
        assert not missing, f"docs/{rel_path} 缺少已实现的工具: {sorted(missing)}"


# Config paths that are documented deliberately even though they do not exist
# in the schema, with the reason. Keep this list short and justified.
KNOWN_DOC_ONLY_PATHS = {
    # add-channel.md walks through adding a channel that the repo does not have;
    # the page says so in the sentence above the block.
    "channels.line",
}


def test_all_docs_config_keys_exist():
    """Every config key shown anywhere in docs/ must exist in the schema.

    Unlike test_yaml_examples_match_config_schema, this walks all pages rather
    than a curated list, so a newly added page cannot ship invented keys. The
    failure mode it guards against: pydantic ignores unknown keys, so a reader
    who copies the example gets a config that looks applied but silently is not.
    """
    import yaml

    from echo_agent.config.schema import Config

    sections = set(Config.model_fields) | {
        f.alias for f in Config.model_fields.values() if f.alias
    }

    def walk(model, data: dict, prefix: str) -> list[str]:
        bad: list[str] = []
        for key, value in data.items():
            field = _resolve_field(model, key)
            if field is None:
                bad.append(f"{prefix}{key}")
                continue
            if isinstance(value, dict) and hasattr(field.annotation, "model_fields"):
                bad += walk(field.annotation, value, f"{prefix}{key}.")
        return bad

    failures: list[str] = []
    for md in sorted(DOCS_DIR.rglob("*.md")):
        if md.name.startswith("configuration."):
            continue  # generated by docgen
        text = md.read_text(encoding="utf-8", errors="replace")
        position = 0
        for block in re.findall(r"```ya?ml\n(.*?)```", text, re.S):
            index = text.index(block, position)
            position = index + len(block)
            line = text[:index].count("\n") + 2
            try:
                data = yaml.safe_load(block)
            except yaml.YAMLError:
                continue
            # Treat a block as echo-agent config only when every top-level key
            # is a real section: keeps skill manifests and compose files out.
            if not isinstance(data, dict) or not data or not set(data) <= sections:
                continue
            unknown = [
                path for path in walk(Config, data, "")
                if path not in KNOWN_DOC_ONLY_PATHS
            ]
            if unknown:
                failures.append(f"{md.relative_to(DOCS_DIR)}:{line} {sorted(set(unknown))}")
    assert not failures, "以下配置示例引用了不存在的配置项:\n" + "\n".join(failures)


def test_all_docs_config_blocks_validate():
    """Every echo-agent config block in docs/ must pass schema validation.

    Covers all pages, not just SCHEMA_CHECKED_PAGES: a block is treated as
    echo-agent config only when all its top-level keys are real config
    sections, which keeps skill manifests and compose files out of scope.
    """
    import yaml

    from echo_agent.config.schema import Config

    sections = set(Config.model_fields)
    failures: list[str] = []
    for md in sorted(DOCS_DIR.rglob("*.md")):
        if md.name.startswith("configuration."):
            continue  # generated by docgen
        text = md.read_text(encoding="utf-8", errors="replace")
        position = 0
        for block in re.findall(r"```ya?ml\n(.*?)```", text, re.S):
            index = text.index(block, position)
            position = index + len(block)
            line = text[:index].count("\n") + 2
            if "${" in block:
                continue  # compose-style interpolation, expanded by the caller
            try:
                data = yaml.safe_load(block)
            except yaml.YAMLError:
                continue
            if not isinstance(data, dict) or not data or not set(data) <= sections:
                continue
            try:
                Config(**data)
            except Exception as exc:
                detail = str(exc)
                first = detail.splitlines()[1].strip() if "\n" in detail else detail
                failures.append(f"{md.relative_to(DOCS_DIR)}:{line} {first[:80]}")
    assert not failures, "以下配置示例无法通过 schema 校验:\n" + "\n".join(failures)


def test_documented_cli_commands_are_real():
    """`echo-agent <cmd>` in shell blocks must be a registered subcommand.

    Only shell-fenced blocks are checked: prose and yaml legitimately mention
    the word "echo-agent" followed by other words.
    """
    main_src = (
        Path(__file__).resolve().parent.parent / "echo_agent" / "__main__.py"
    ).read_text(encoding="utf-8")
    real = set(re.findall(r'add_parser\(\s*"([a-z][a-z0-9_-]*)"', main_src))
    assert real, "could not parse any subcommand from __main__.py"

    bogus: list[str] = []
    for md in sorted(DOCS_DIR.rglob("*.md")):
        text = md.read_text(encoding="utf-8", errors="replace")
        for block in re.finditer(r"```(?:bash|sh|shell|console)\n(.*?)```", text, re.S):
            base = text[: block.start()].count("\n") + 1
            for offset, raw in enumerate(block.group(1).splitlines()):
                line = raw.strip().lstrip("$ ").strip()
                found = re.match(r"^echo-agent\s+([a-z][a-z0-9_-]*)", line)
                if found and found.group(1) not in real:
                    rel = md.relative_to(DOCS_DIR)
                    bogus.append(f"{rel}:{base + offset + 1} -> {found.group(1)}")
    assert not bogus, f"文档使用了不存在的 CLI 子命令: {bogus}"


def test_zh_en_pairs_have_matching_h1():
    """Chinese and English docs should both have a top-level heading."""
    missing_h1 = []
    for zh, en in _zh_en_pairs():
        for path in (zh, en):
            content = path.read_text(encoding="utf-8")
            if not any(line.startswith("# ") for line in content.splitlines()[:5]):
                missing_h1.append(str(path.relative_to(DOCS_DIR)))
    assert not missing_h1, f"以下文档缺少 H1 标题: {missing_h1}"


def test_channel_count_consistent():
    """Channel index should mention every adapter in the real registry.

    The expected set is read from _CHANNEL_REGISTRY rather than hardcoded, so
    adding or removing an adapter fails this test until the docs follow.
    """
    from echo_agent.channels.manager import _CHANNEL_REGISTRY

    channel_index = DOCS_DIR / "integrations" / "channels" / "index.md"
    if not channel_index.is_file():
        pytest.skip("channels index not yet created")
    content = channel_index.read_text(encoding="utf-8").lower()
    missing = [ch for ch in sorted(_CHANNEL_REGISTRY) if ch not in content]
    assert not missing, f"Channel 总览页缺少: {missing}"
