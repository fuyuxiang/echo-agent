"""Tests for echo_agent/skills/store.py.

The SkillManager tests that used to live here were removed with the class: it was
a second, parallel skill model (manifest.json / .status / config.json) that
production never constructed — app.py passed skill_manager=None unconditionally
and no manifest.json ever existed on disk. Its tests were the only callers, which
is what made "82 tests green" say nothing about that code path working.
"""

from __future__ import annotations

from pathlib import Path


from echo_agent.skills.store import SkillStore, parse_frontmatter


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

_VALID_SKILL_CONTENT = """\
---
name: my-skill
description: A test skill
version: "1.0.0"
---

# My Skill

Body text here.
"""


def _make_skill_on_disk(root: Path, name: str, description: str = "A skill", category: str = "") -> Path:
    """Create a minimal skill directory with SKILL.md."""
    parent = root / category / name if category else root / name
    parent.mkdir(parents=True, exist_ok=True)
    content = f"---\nname: {name}\ndescription: {description}\n---\n\nBody.\n"
    (parent / "SKILL.md").write_text(content, encoding="utf-8")
    return parent


# ---------------------------------------------------------------------------
# parse_frontmatter
# ---------------------------------------------------------------------------

class TestParseFrontmatter:
    def test_valid_frontmatter(self):
        fm, body = parse_frontmatter("---\nname: foo\n---\n\nHello")
        assert fm["name"] == "foo"
        assert "Hello" in body

    def test_missing_frontmatter(self):
        fm, body = parse_frontmatter("Just plain text")
        assert fm == {}
        assert body == "Just plain text"

    def test_invalid_yaml(self):
        fm, body = parse_frontmatter("---\n: [invalid\n---\n\nBody")
        assert fm == {}
        assert "Body" in body

    def test_no_closing_fence(self):
        fm, body = parse_frontmatter("---\nname: bar\nno closing")
        assert fm == {}


# ---------------------------------------------------------------------------
# SkillStore
# ---------------------------------------------------------------------------

class TestSkillStore:
    def test_create_and_list(self, tmp_path: Path):
        store = SkillStore(user_dir=tmp_path / "skills")
        err = store.create_skill("hello", _VALID_SKILL_CONTENT.replace("my-skill", "hello"))
        assert err is None
        skills = store.list_all()
        assert any(s.name == "hello" for s in skills)

    def test_read_skill(self, tmp_path: Path):
        store = SkillStore(user_dir=tmp_path / "skills")
        store.create_skill("reader", _VALID_SKILL_CONTENT.replace("my-skill", "reader"))
        content = store.read_skill("reader")
        assert content is not None
        assert "reader" in content

    def test_delete_skill(self, tmp_path: Path):
        store = SkillStore(user_dir=tmp_path / "skills")
        store.create_skill("doomed", _VALID_SKILL_CONTENT.replace("my-skill", "doomed"))
        err = store.delete_skill("doomed")
        assert err is None
        assert store.read_skill("doomed") is None

    def test_delete_nonexistent(self, tmp_path: Path):
        store = SkillStore(user_dir=tmp_path / "skills")
        err = store.delete_skill("ghost")
        assert err is not None

    def test_invalid_name_rejected(self, tmp_path: Path):
        store = SkillStore(user_dir=tmp_path / "skills")
        err = store.create_skill("INVALID NAME!", _VALID_SKILL_CONTENT)
        assert err is not None

    def test_duplicate_name_rejected(self, tmp_path: Path):
        store = SkillStore(user_dir=tmp_path / "skills")
        store.create_skill("dup", _VALID_SKILL_CONTENT.replace("my-skill", "dup"))
        err = store.create_skill("dup", _VALID_SKILL_CONTENT.replace("my-skill", "dup"))
        assert err is not None
        assert "already exists" in err

    def test_path_traversal_blocked_read_file(self, tmp_path: Path):
        store = SkillStore(user_dir=tmp_path / "skills")
        store.create_skill("safe", _VALID_SKILL_CONTENT.replace("my-skill", "safe"))
        assert store.read_file("safe", "../../etc/passwd") is None
        assert store.read_file("safe", "/etc/passwd") is None

    def test_path_traversal_blocked_write_file(self, tmp_path: Path):
        store = SkillStore(user_dir=tmp_path / "skills")
        store.create_skill("safe2", _VALID_SKILL_CONTENT.replace("my-skill", "safe2"))
        err = store.write_file("safe2", "../evil.txt", "bad")
        assert err is not None

    def test_list_excludes_disabled(self, tmp_path: Path):
        user_dir = tmp_path / "skills"
        _make_skill_on_disk(user_dir, "enabled-skill")
        _make_skill_on_disk(user_dir, "disabled-skill")
        store = SkillStore(user_dir=user_dir, disabled=["disabled-skill"])
        names = [s.name for s in store.list_all()]
        assert "enabled-skill" in names
        assert "disabled-skill" not in names

    def test_create_missing_description(self, tmp_path: Path):
        store = SkillStore(user_dir=tmp_path / "skills")
        content = "---\nname: nodesc\n---\n\nBody"
        err = store.create_skill("nodesc", content)
        assert err is not None
        assert "description" in err
