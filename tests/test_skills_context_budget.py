"""Tests for the skills-inventory token budget in the system prompt.

The skill list is injected into *every* turn's system prompt. The 35 bundled
skills already cost ~1.2k tokens unbounded; a user with a few hundred installed
would have handed a fixed five-figure token tax to every single request, and the
entries squeezed out would have been whatever sorted last — silently.

The budget degrades in two stages: truncate descriptions first (a shortened
description still identifies a skill well enough for the model to call
skill_view), then drop entries while *saying* how many were dropped, because an
unannounced omission would make the model confidently assert a skill does not
exist.
"""

from __future__ import annotations

import pathlib
from types import SimpleNamespace

import pytest

from echo_agent.agent.context import (
    _SKILLS_MAX_CHARS,
    build_skills_context,
)
from echo_agent.skills.store import SkillStore


def _store(n: int, *, desc_len: int = 60, name_len: int = 0):
    skills = [
        SimpleNamespace(
            name=f"skill-{'x' * name_len}-{i:04d}",
            category="utility",
            description="d" * desc_len,
            path="",
        )
        for i in range(n)
    ]
    return SimpleNamespace(list_all=lambda: skills)


class TestBudgetIsEnforced:
    @pytest.mark.parametrize("n", [0, 1, 35, 62, 63, 100, 300, 1000, 5000])
    def test_never_exceeds_budget(self, n):
        out = build_skills_context(_store(n))
        assert len(out) <= _SKILLS_MAX_CHARS, f"{n} skills produced {len(out)} chars"

    def test_pathological_descriptions_stay_bounded(self):
        out = build_skills_context(_store(50, desc_len=5000))
        assert len(out) <= _SKILLS_MAX_CHARS

    def test_pathological_names_stay_bounded(self):
        """Names are not truncated, so many long ones must be dropped instead."""
        out = build_skills_context(_store(200, desc_len=10, name_len=200))
        assert len(out) <= _SKILLS_MAX_CHARS

    def test_custom_budget_is_honored(self):
        out = build_skills_context(_store(100), max_chars=2000)
        assert len(out) <= 2000


class TestSmallInventoriesAreUntouched:
    """Guard against over-engineering: the common case must not regress."""

    def test_bundled_skills_listed_in_full(self):
        store = SkillStore(
            user_dir=pathlib.Path("/tmp/echo-nonexistent-user-dir-for-tests"),
            builtin_dir=pathlib.Path("skills"),
        )
        skills = store.list_all()
        assert len(skills) >= 30, "expected the bundled skill set"

        out = build_skills_context(store)
        assert out.count("\n  - ") == len(skills), "every bundled skill must be listed"
        assert "more skill(s) not listed" not in out
        assert "…" not in out, "descriptions must not be truncated at this size"

    def test_descriptions_intact_for_small_sets(self):
        out = build_skills_context(_store(5, desc_len=200))
        assert "d" * 200 in out

    def test_empty_store_says_so(self):
        out = build_skills_context(_store(0))
        assert "No skills available yet" in out

    def test_none_store_returns_empty(self):
        assert build_skills_context(None) == ""


class TestDegradationOrder:
    def test_descriptions_truncate_before_entries_drop(self):
        """Truncating text is cheaper than losing a skill's existence entirely."""
        out = build_skills_context(_store(40, desc_len=400))
        assert out.count("\n  - ") == 40, "should have kept all 40 by truncating"
        assert "…" in out

    def test_dropped_count_is_reported(self):
        out = build_skills_context(_store(500))
        assert "more skill(s) not listed" in out
        listed = out.count("\n  - ") - 1  # the notice line also starts with "  …"
        notice = [line for line in out.splitlines() if "more skill(s)" in line]
        assert notice, "omission must be announced"
        assert "skills_list" in notice[0], "must point at the tool holding the full list"

    def test_reported_count_matches_reality(self):
        total = 500
        out = build_skills_context(_store(total))
        notice = next(line for line in out.splitlines() if "more skill(s)" in line)
        dropped = int(notice.split("and")[1].split("more")[0].strip())
        listed = sum(
            1 for line in out.splitlines()
            if line.startswith("  - ")
        )
        assert listed + dropped == total

    def test_first_entries_are_the_ones_kept(self):
        """Dropping from the tail keeps the listing deterministic."""
        out = build_skills_context(_store(500))
        assert "skill--0000" in out
        assert "skill--0499" not in out


class TestFailureHandling:
    def test_store_error_degrades_to_empty(self):
        def boom():
            raise RuntimeError("disk gone")

        out = build_skills_context(SimpleNamespace(list_all=boom))
        assert out == ""

    def test_missing_description_does_not_crash(self):
        store = SimpleNamespace(list_all=lambda: [
            SimpleNamespace(name="bare", category="", description=None, path=""),
        ])
        out = build_skills_context(store)
        assert "bare" in out
