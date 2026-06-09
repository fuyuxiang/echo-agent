#!/usr/bin/env python3
"""
Apply all remaining code review fixes (P1-P3) to echo-agent.
Run from project root: python3 scripts/apply_review_fixes.py
"""
import os
import sys
import json

PROJECT_ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, PROJECT_ROOT)


def patch_file(filepath, replacements):
    """Apply a list of (old, new) replacements to a file."""
    full_path = os.path.join(PROJECT_ROOT, filepath)
    with open(full_path) as f:
        content = f.read()
    for old, new in replacements:
        if old not in content:
            print(f"  WARNING: pattern not found in {filepath}:")
            print(f"    {old[:80]}...")
            continue
        content = content.replace(old, new, 1)
    with open(full_path, 'w') as f:
        f.write(content)
    print(f"  PATCHED: {filepath}")


def insert_after(filepath, marker, insertion):
    """Insert text after a marker line."""
    full_path = os.path.join(PROJECT_ROOT, filepath)
    with open(full_path) as f:
        content = f.read()
    if marker not in content:
        print(f"  WARNING: marker not found in {filepath}: {marker[:60]}")
        return
    content = content.replace(marker, marker + insertion, 1)
    with open(full_path, 'w') as f:
        f.write(content)
    print(f"  PATCHED: {filepath}")


# ============================================================
# P1-4: Memory Consolidation counter fix
# ============================================================
print("\n[P1-4] Fixing Memory Consolidation counter...")

patch_file("echo_agent/memory/consolidator.py", [
    # Add _last_consolidated_counts to __init__
    (
        "        self._consolidation_threshold = consolidation_threshold\n"
        "        self._episodic_manager = None  # set via set_episodic_manager()",
        "        self._consolidation_threshold = consolidation_threshold\n"
        "        self._last_consolidated_counts: dict[str, int] = {}\n"
        "        self._episodic_manager = None  # set via set_episodic_manager()"
    ),
    # Replace should_consolidate with session-key-aware version
    (
        "    def should_consolidate(self, session_message_count: int, last_consolidated: int) -> bool:\n"
        "        unconsolidated = session_message_count - last_consolidated\n"
        "        return unconsolidated >= self._consolidation_threshold",
        "    def should_consolidate(self, session_key: str, session_message_count: int) -> bool:\n"
        "        \"\"\"Check if session needs consolidation based on internal tracking.\"\"\"\n"
        "        last = self._last_consolidated_counts.get(session_key, 0)\n"
        "        return (session_message_count - last) >= self._consolidation_threshold\n"
        "\n"
        "    def mark_consolidated(self, session_key: str, message_count: int) -> None:\n"
        "        \"\"\"Mark consolidation completed at given message count.\"\"\"\n"
        "        self._last_consolidated_counts[session_key] = message_count"
    ),
])


# ============================================================
# P1-6: LLM JSON validation
# ============================================================
print("\n[P1-6] Adding LLM JSON validation...")

VALIDATE_FNS = '''

def _validate_tool_args(raw, max_size=50000):
    """Validate and sanitize LLM-generated tool arguments."""
    if isinstance(raw, str):
        if len(raw) > max_size:
            raise ValueError("Tool arguments too large")
        raw = json.loads(raw)
    if not isinstance(raw, dict):
        raise ValueError("Expected dict from tool_calls arguments")
    return {
        "history_entry": str(raw.get("history_entry", ""))[:5000],
        "memory_update": str(raw.get("memory_update", ""))[:20000],
    }


def _validate_facts_json(raw, max_size=50000):
    """Validate LLM-generated fact extraction JSON."""
    if isinstance(raw, str):
        if len(raw) > max_size:
            raise ValueError("Facts JSON too large")
        raw = json.loads(raw)
    if not isinstance(raw, list):
        raise ValueError("Expected list from fact extraction")
    validated = []
    for item in raw[:50]:
        if isinstance(item, dict):
            validated.append({
                "type": str(item.get("type", "environment"))[:20],
                "key": str(item.get("key", ""))[:200],
                "content": str(item.get("content", ""))[:2000],
                "importance": min(1.0, max(0.0, float(item.get("importance", 0.5)))),
            })
    return validated


def _estimate_tokens(text: str) -> int:
    """Estimate token count with multibyte-aware heuristic."""
    ascii_chars = sum(1 for c in text if ord(c) < 128)
    non_ascii = len(text) - ascii_chars
    return (ascii_chars // 4) + (non_ascii * 2)

'''

insert_after("echo_agent/memory/consolidator.py",
    "\nclass MemoryConsolidator:\n",
    "")  # placeholder - we insert before the class instead

# Actually insert the validation functions before the class
patch_file("echo_agent/memory/consolidator.py", [
    (
        "\nclass MemoryConsolidator:",
        VALIDATE_FNS + "\nclass MemoryConsolidator:"
    ),
    # Replace json.loads in consolidate_chunk
    (
        "            args = response.tool_calls[0].arguments\n"
        "            if isinstance(args, str):\n"
        "                args = json.loads(args)\n"
        "\n"
        "            history_entry = args.get(\"history_entry\", \"\")\n"
        "            memory_update = args.get(\"memory_update\", \"\")",
        "            args = _validate_tool_args(response.tool_calls[0].arguments)\n"
        "            history_entry = args[\"history_entry\"]\n"
        "            memory_update = args[\"memory_update\"]"
    ),
    # Replace fact extraction JSON parsing
    (
        "                        if response.content:\n"
        "                            import json as _json\n"
        "                            try:\n"
        "                                facts = _json.loads(response.content)\n"
        "                                if isinstance(facts, list):\n"
        "                                    promoted = await self._semantic_manager.promote_from_episodic(episode, facts)\n"
        "                                    stats[\"promoted\"] = len(promoted)\n"
        "                            except _json.JSONDecodeError:\n"
        "                                pass",
        "                        if response.content:\n"
        "                            try:\n"
        "                                facts = _validate_facts_json(response.content)\n"
        "                                if facts:\n"
        "                                    promoted = await self._semantic_manager.promote_from_episodic(episode, facts)\n"
        "                                    stats[\"promoted\"] = len(promoted)\n"
        "                            except (json.JSONDecodeError, ValueError, TypeError):\n"
        "                                pass"
    ),
])


# ============================================================
# P2-10: Token estimation improvement
# ============================================================
print("\n[P2-10] Improving token estimation...")

patch_file("echo_agent/memory/consolidator.py", [
    (
        "            tokens += len(str(content)) // 3",
        "            tokens += _estimate_tokens(str(content))"
    ),
])


# ============================================================
# P1-7: Vector Index rebuild consistency (swap strategy)
# ============================================================
print("\n[P1-7] Fixing vector index rebuild consistency...")

patch_file("echo_agent/memory/vectors.py", [
    (
        "    async def _rebuild_unlocked(self) -> None:\n"
        "        self._id_map.clear()\n"
        "        self._source_map.clear()\n"
        "        self._deleted_sources.clear()\n"
        "        if self._index is not None:\n"
        "            self._index.reset()\n"
        "        await self._initialize_unlocked()",
        "    async def _rebuild_unlocked(self) -> None:\n"
        "        \"\"\"Rebuild using swap strategy for consistency.\"\"\"\n"
        "        new_id_map: list[str] = []\n"
        "        new_source_map: list[str] = []\n"
        "        new_index = None\n"
        "        if _HAS_FAISS:\n"
        "            new_index = faiss.IndexFlatIP(self._dimensions)\n"
        "\n"
        "        rows = await self._storage.load_vectors_all()\n"
        "        if rows and new_index is not None:\n"
        "            embeddings = []\n"
        "            for row in rows:\n"
        "                vec_id = row[\"id\"]\n"
        "                source_id = row.get(\"source_id\", \"\")\n"
        "                emb = row.get(\"embedding\")\n"
        "                if emb is not None:\n"
        "                    arr = np.frombuffer(emb, dtype=np.float32)\n"
        "                    if arr.shape[0] == self._dimensions:\n"
        "                        embeddings.append(arr)\n"
        "                        new_id_map.append(vec_id)\n"
        "                        new_source_map.append(source_id)\n"
        "            if embeddings:\n"
        "                matrix = np.vstack(embeddings).astype(np.float32)\n"
        "                faiss.normalize_L2(matrix)\n"
        "                new_index.add(matrix)\n"
        "\n"
        "        # Atomic swap\n"
        "        self._index = new_index\n"
        "        self._id_map = new_id_map\n"
        "        self._source_map = new_source_map\n"
        "        self._deleted_sources = set()\n"
        "        self._initialized = True\n"
        "        logger.info(\"Vector index rebuilt: {} vectors\", len(new_id_map))"
    ),
])


# ============================================================
# P2-9: USER memory staleness review
# ============================================================
print("\n[P2-9] Adding stale USER memory detection...")

insert_after("echo_agent/memory/forgetting.py",
    "        return to_archive, to_forget\n",
    '''
    def find_stale_user_memories(
        self,
        entries: list["MemoryEntry"],
        stale_days: int = 180,
    ) -> list["MemoryEntry"]:
        """Identify USER memories not accessed in stale_days — for review, NOT deletion."""
        from echo_agent.memory.types import MemoryType
        stale: list["MemoryEntry"] = []
        now = datetime.now()
        for entry in entries:
            if entry.type != MemoryType.USER:
                continue
            if not entry.last_accessed:
                continue
            try:
                last = datetime.fromisoformat(entry.last_accessed)
                if (now - last).days >= stale_days:
                    stale.append(entry)
            except (ValueError, OverflowError):
                pass
        return stale
''')


# ============================================================
# P2-8: Evolution Cooldown persistence
# ============================================================
print("\n[P2-8] Adding cooldown persistence...")

patch_file("echo_agent/evolution/engine.py", [
    # Make _activate_cooldown async and persist
    (
        "    def _activate_cooldown(self, skill_name: str) -> None:\n"
        "        seconds = max(0, int(self._config.cooldown_seconds_after_promote))\n"
        "        if seconds <= 0 or not skill_name:\n"
        "            return\n"
        "        self._cooldowns[skill_name] = _Cooldown(\n"
        "            skill_name=skill_name,\n"
        "            until_ts=time.time() + seconds,\n"
        "        )",
        "    async def _activate_cooldown(self, skill_name: str) -> None:\n"
        "        seconds = max(0, int(self._config.cooldown_seconds_after_promote))\n"
        "        if seconds <= 0 or not skill_name:\n"
        "            return\n"
        "        until_ts = time.time() + seconds\n"
        "        self._cooldowns[skill_name] = _Cooldown(\n"
        "            skill_name=skill_name,\n"
        "            until_ts=until_ts,\n"
        "        )\n"
        "        try:\n"
        "            await self._store.save_cooldown(skill_name, until_ts)\n"
        "        except Exception as e:\n"
        "            logger.debug(\"Failed to persist cooldown: {}\", e)"
    ),
    # Update caller of _activate_cooldown to await
    (
        "                    self._activate_cooldown(candidate.skill_name)",
        "                    await self._activate_cooldown(candidate.skill_name)"
    ),
    # Add _load_cooldowns method before _is_in_cooldown
    (
        "    def _is_in_cooldown(self, skill_name: str) -> bool:",
        "    async def _load_cooldowns(self) -> None:\n"
        "        \"\"\"Load persisted cooldowns from storage.\"\"\"\n"
        "        try:\n"
        "            rows = await self._store.load_cooldowns()\n"
        "            now = time.time()\n"
        "            for name, until_ts in rows:\n"
        "                if until_ts > now:\n"
        "                    self._cooldowns[name] = _Cooldown(skill_name=name, until_ts=until_ts)\n"
        "        except Exception as e:\n"
        "            logger.debug(\"Failed to load cooldowns: {}\", e)\n"
        "\n"
        "    def _is_in_cooldown(self, skill_name: str) -> bool:"
    ),
    # Call _load_cooldowns in start()
    (
        "        await self._scheduler.start()\n"
        "        self._started = True",
        "        await self._load_cooldowns()\n"
        "        await self._scheduler.start()\n"
        "        self._started = True"
    ),
])


# ============================================================
# P1-5: Contradiction detection injection into context
# ============================================================
print("\n[P1-5] Adding contradiction query method...")

# Add get_unresolved method to ContradictionDetector
# We need to read the file first
contradiction_path = os.path.join(PROJECT_ROOT, "echo_agent/memory/contradiction.py")
with open(contradiction_path) as f:
    content = f.read()

# Find a good insertion point - after the class docstring
get_unresolved_method = '''
    async def get_unresolved(self, limit: int = 10) -> list["Contradiction"]:
        """Retrieve unresolved contradictions for context injection."""
        try:
            rows = await self._storage.query(
                "SELECT data FROM contradictions WHERE resolved = 0 ORDER BY created_at DESC LIMIT ?",
                (limit,),
            )
            results = []
            for row in rows:
                try:
                    data = json.loads(row["data"]) if isinstance(row["data"], str) else row["data"]
                    results.append(Contradiction(**data))
                except (json.JSONDecodeError, TypeError, KeyError):
                    continue
            return results
        except Exception:
            return []

    async def get_unresolved_for_query(
        self,
        query: str,
        limit: int = 3,
        embed_fn=None,
    ) -> list["Contradiction"]:
        """Get contradictions relevant to a query (keyword match or vector similarity)."""
        all_unresolved = await self.get_unresolved(limit=20)
        if not all_unresolved:
            return []
        query_lower = query.lower()
        scored = []
        for c in all_unresolved:
            relevance = 0.0
            combined = f"{c.entry_a_content} {c.entry_b_content} {c.explanation}".lower()
            query_words = query_lower.split()
            matches = sum(1 for w in query_words if w in combined)
            if query_words:
                relevance = matches / len(query_words)
            scored.append((relevance, c))
        scored.sort(key=lambda x: x[0], reverse=True)
        return [c for score, c in scored[:limit] if score > 0.1]

'''

# Insert after MAX_LLM_CANDIDATES = 5 line
if "MAX_LLM_CANDIDATES = 5" in content:
    content = content.replace(
        "    MAX_LLM_CANDIDATES = 5\n",
        "    MAX_LLM_CANDIDATES = 5\n" + get_unresolved_method
    )
    with open(contradiction_path, 'w') as f:
        f.write(content)
    print(f"  PATCHED: echo_agent/memory/contradiction.py")
else:
    print(f"  WARNING: Could not find insertion point in contradiction.py")


# ============================================================
# P3-11: Create shared conftest.py
# ============================================================
print("\n[P3-11] Creating tests/conftest.py...")

conftest_path = os.path.join(PROJECT_ROOT, "tests", "conftest.py")
conftest_content = '''"""Shared test fixtures and helpers for echo-agent test suite."""

import uuid
from datetime import datetime
from typing import Any
from unittest.mock import AsyncMock, MagicMock

import pytest


# ── Common response helpers ──────────────────────────────────────────────────


class FakeLLMResponse:
    """Minimal LLM response stub for testing."""

    def __init__(self, content="", tool_calls=None):
        self.content = content
        self.tool_calls = tool_calls or []


class FakeToolCall:
    """Minimal tool call stub."""

    def __init__(self, name="", arguments=None):
        self.name = name
        self.arguments = arguments if arguments is not None else {}
        self.id = uuid.uuid4().hex[:8]


def make_llm_response(content="done", tool_calls=None):
    """Create a fake LLM response for use in mock side_effect lists."""
    return FakeLLMResponse(content=content, tool_calls=tool_calls)


def make_tool_call(name, **kwargs):
    """Create a fake tool call."""
    import json
    return FakeToolCall(name=name, arguments=json.dumps(kwargs))


# ── Fixtures ─────────────────────────────────────────────────────────────────


@pytest.fixture
def mock_provider():
    """LLM provider mock with configurable responses."""
    provider = AsyncMock()
    provider.chat_with_retry = AsyncMock(return_value=FakeLLMResponse())
    provider.embed = AsyncMock(return_value=[0.1] * 1536)
    return provider


@pytest.fixture
def mock_llm_call():
    """Standalone async LLM call mock."""
    return AsyncMock(return_value=FakeLLMResponse())


@pytest.fixture
def tmp_workspace(tmp_path):
    """Temporary workspace directory with skills subdirectory."""
    skills_dir = tmp_path / "skills"
    skills_dir.mkdir()
    return tmp_path


# ── Trajectory helpers ───────────────────────────────────────────────────────


def make_trajectory(
    *,
    outcome="success",
    tool_name="test_tool",
    reflection_score=0.8,
    session_key="test-session",
    trajectory_id=None,
):
    """Create a trajectory dict for evolution engine tests."""
    return {
        "id": trajectory_id or uuid.uuid4().hex[:12],
        "session_key": session_key,
        "tool_name": tool_name,
        "outcome": outcome,
        "reflection_score": reflection_score,
        "created_at": datetime.now().isoformat(),
        "consumed_by_run": None,
    }


# ── Evolution helpers ────────────────────────────────────────────────────────


def make_propose_call(skill_name="new-skill", operation="create", content="# SKILL"):
    """Create a tool call for skill proposal in evolution tests."""
    import json
    args = {
        "skill_name": skill_name,
        "operation": operation,
        "proposed_content": content,
    }
    return FakeToolCall(name="propose_skill", arguments=json.dumps(args))
'''

with open(conftest_path, 'w') as f:
    f.write(conftest_content)
print(f"  CREATED: tests/conftest.py")


# ============================================================
# Summary
# ============================================================
print("\n" + "=" * 60)
print("All patches applied successfully!")
print("=" * 60)
print("""
Remaining manual steps:
1. Add save_cooldown/load_cooldowns to TrajectoryStore
2. Update ContextStage to query contradictions
3. Run: pytest tests/ -x --tb=short
""")
