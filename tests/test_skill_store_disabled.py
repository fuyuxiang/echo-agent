"""Regression tests for disabled-skill enforcement and store robustness.

Disabling a skill used to only *hide* it: list_all() filtered the disabled set
but _find_skill_dir did not, so read_skill, read_file and skill_run all still
resolved it. That also defeated the evolution gate's persist_disable() — the
mechanism for taking a misbehaving skill out of service could not actually take
it out of service. And one malformed SKILL.md anywhere under a skills root threw
AttributeError out of list_all(), making *every* skill vanish at once.
"""

from __future__ import annotations

import pytest

from echo_agent.skills.store import SkillStore, parse_frontmatter


def _write_skill(root, name, *, description="d", body="body", category=""):
    parent = root / category if category else root
    d = parent / name
    d.mkdir(parents=True, exist_ok=True)
    (d / "SKILL.md").write_text(
        f"---\nname: {name}\ndescription: {description}\n---\n\n{body}\n",
        encoding="utf-8",
    )
    return d


@pytest.fixture
def user_dir(tmp_path):
    d = tmp_path / "user"
    d.mkdir()
    return d


class TestDisabledEnforcement:
    def test_disabled_skill_is_not_listed(self, user_dir):
        _write_skill(user_dir, "banned")
        store = SkillStore(user_dir=user_dir, disabled=["banned"])
        assert [m.name for m in store.list_all()] == []

    def test_disabled_skill_does_not_resolve(self, user_dir):
        """The core fix: resolution is where every read and run passes through."""
        _write_skill(user_dir, "banned")
        store = SkillStore(user_dir=user_dir, disabled=["banned"])
        assert store.find_skill_dir("banned") is None

    def test_disabled_skill_content_is_not_readable(self, user_dir):
        _write_skill(user_dir, "banned", body="SECRET BODY")
        store = SkillStore(user_dir=user_dir, disabled=["banned"])
        assert store.read_skill("banned") is None

    def test_disabled_skill_support_files_are_not_readable(self, user_dir):
        d = _write_skill(user_dir, "banned")
        (d / "scripts").mkdir()
        (d / "scripts" / "run.py").write_text("print('ran')\n")
        store = SkillStore(user_dir=user_dir, disabled=["banned"])
        assert store.read_file("banned", "scripts/run.py") is None
        assert store.list_files("banned") == []

    def test_is_disabled_reports_state(self, user_dir):
        _write_skill(user_dir, "banned")
        _write_skill(user_dir, "fine")
        store = SkillStore(user_dir=user_dir, disabled=["banned"])
        assert store.is_disabled("banned") is True
        assert store.is_disabled("fine") is False

    def test_enabled_skill_still_works(self, user_dir):
        """Guard against over-blocking: normal skills must be unaffected."""
        _write_skill(user_dir, "fine", body="USABLE")
        store = SkillStore(user_dir=user_dir, disabled=["banned"])
        assert store.find_skill_dir("fine") is not None
        assert "USABLE" in (store.read_skill("fine") or "")
        assert [m.name for m in store.list_all()] == ["fine"]


class TestPersistedDisable:
    def test_persist_disable_survives_reload(self, user_dir):
        _write_skill(user_dir, "bad")
        store = SkillStore(user_dir=user_dir)
        store.persist_disable("bad")
        assert SkillStore(user_dir=user_dir).find_skill_dir("bad") is None

    def test_persist_enable_restores(self, user_dir):
        _write_skill(user_dir, "bad")
        store = SkillStore(user_dir=user_dir)
        store.persist_disable("bad")
        store.persist_enable("bad")
        assert store.find_skill_dir("bad") is not None
        assert SkillStore(user_dir=user_dir).find_skill_dir("bad") is not None

    def test_disable_covers_every_alias(self, user_dir):
        """A skill whose directory name differs from its frontmatter name answers
        to both, so disabling one name must not leave the other runnable."""
        d = user_dir / "my-alias"
        d.mkdir()
        (d / "SKILL.md").write_text(
            "---\nname: original-name\ndescription: d\n---\n\nbody\n", encoding="utf-8",
        )
        store = SkillStore(user_dir=user_dir)
        store.persist_disable("my-alias")
        assert store.find_skill_dir("my-alias") is None
        assert store.find_skill_dir("original-name") is None

    def test_delete_clears_disable_entries(self, user_dir):
        """Otherwise the name stays poisoned and a future skill installed under
        it would be silently disabled with no directory left to explain why."""
        _write_skill(user_dir, "gone")
        store = SkillStore(user_dir=user_dir)
        store.persist_disable("gone")
        assert store.delete_skill("gone") is None
        assert store.is_disabled("gone") is False

        _write_skill(user_dir, "gone", body="REINSTALLED")
        assert store.find_skill_dir("gone") is not None


class TestManagementOfDisabledSkills:
    """Disabling must stop a skill from running, not lock the operator out of
    fixing or removing it."""

    def test_can_delete_disabled_skill(self, user_dir):
        _write_skill(user_dir, "bad")
        store = SkillStore(user_dir=user_dir, disabled=["bad"])
        assert store.delete_skill("bad") is None

    def test_can_patch_disabled_skill(self, user_dir):
        _write_skill(user_dir, "bad", body="BROKEN")
        store = SkillStore(user_dir=user_dir, disabled=["bad"])
        assert store.patch_skill("bad", "BROKEN", "FIXED") is None
        assert "FIXED" in (store.read_skill("bad", include_disabled=True) or "")

    def test_can_update_disabled_skill(self, user_dir):
        _write_skill(user_dir, "bad")
        store = SkillStore(user_dir=user_dir, disabled=["bad"])
        new = "---\nname: bad\ndescription: repaired\n---\n\nnew body\n"
        assert store.update_skill("bad", new) is None

    def test_admin_listing_can_include_disabled(self, user_dir):
        _write_skill(user_dir, "banned")
        store = SkillStore(user_dir=user_dir, disabled=["banned"])
        names = [m.name for m in store.list_all(include_disabled=True)]
        assert names == ["banned"]

    def test_create_does_not_shadow_disabled_skill(self, user_dir):
        """A disabled skill still occupies its name on disk."""
        _write_skill(user_dir, "taken")
        store = SkillStore(user_dir=user_dir, disabled=["taken"])
        err = store.create_skill(
            "taken", "---\nname: taken\ndescription: other\n---\n\nx\n",
        )
        assert err is not None and "already exists" in err


class TestMalformedFrontmatter:
    def test_non_mapping_frontmatter_parses_as_empty(self):
        """Valid YAML is not necessarily a mapping; callers all use .get()."""
        fm, body = parse_frontmatter("---\n- not-a-mapping\n---\n\nbody\n")
        assert fm == {}
        assert "body" in body

        fm, _ = parse_frontmatter("---\njust a string\n---\n\nbody\n")
        assert fm == {}

    def test_one_bad_skill_does_not_hide_all_skills(self, user_dir):
        """The failure that made the whole skills context disappear."""
        _write_skill(user_dir, "good")
        bad = user_dir / "bad"
        bad.mkdir()
        (bad / "SKILL.md").write_text("---\n- not-a-mapping\n---\n\nbody\n", encoding="utf-8")

        store = SkillStore(user_dir=user_dir)
        names = [m.name for m in store.list_all()]
        assert "good" in names

    def test_lookup_survives_bad_skill(self, user_dir):
        _write_skill(user_dir, "good")
        bad = user_dir / "bad"
        bad.mkdir()
        (bad / "SKILL.md").write_text("---\n- not-a-mapping\n---\n\nbody\n", encoding="utf-8")

        store = SkillStore(user_dir=user_dir)
        assert store.find_skill_dir("good") is not None

    def test_undecodable_skill_md_is_skipped(self, user_dir):
        """A non-UTF-8 SKILL.md must not abort discovery for everything else."""
        _write_skill(user_dir, "good")
        bad = user_dir / "binary"
        bad.mkdir()
        (bad / "SKILL.md").write_bytes(b"\x89PNG\r\n\x1a\n\xff\xfe")

        store = SkillStore(user_dir=user_dir)
        assert store.find_skill_dir("good") is not None
        assert "good" in [m.name for m in store.list_all()]


class TestBinarySupportFiles:
    def test_write_file_bytes_roundtrip(self, user_dir):
        """assets/ exists for images and fonts, but the only writer was text-only."""
        _write_skill(user_dir, "s")
        store = SkillStore(user_dir=user_dir)
        png = b"\x89PNG\r\n\x1a\n" + b"\x00" * 32
        assert store.write_file_bytes("s", "assets/logo.png", png) is None
        assert (user_dir / "s" / "assets" / "logo.png").read_bytes() == png

    def test_binary_file_listed_but_not_text_readable(self, user_dir):
        _write_skill(user_dir, "s")
        store = SkillStore(user_dir=user_dir)
        store.write_file_bytes("s", "assets/logo.png", b"\x89PNG\r\n\x1a\n\xff")
        assert "assets/logo.png" in store.list_files("s")
        # read_file is the text tier; it declines rather than raising.
        assert store.read_file("s", "assets/logo.png") is None

    def test_oversized_binary_rejected(self, user_dir):
        _write_skill(user_dir, "s")
        store = SkillStore(user_dir=user_dir)
        err = store.write_file_bytes("s", "assets/big.bin", b"\x00" * (1_048_576 + 1))
        assert err is not None and "1 MiB" in err

    def test_subdir_restriction_still_applies(self, user_dir):
        _write_skill(user_dir, "s")
        store = SkillStore(user_dir=user_dir)
        err = store.write_file_bytes("s", "evil/x.bin", b"\x00")
        assert err is not None
