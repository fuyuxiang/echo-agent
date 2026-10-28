"""Cost enforcement integration at the inference stage gate."""

from __future__ import annotations

import pytest

from echo_agent.cost.budget import CostTracker, BudgetExceeded


def test_tracker_enforce_blocks_when_over_hard():
    t = CostTracker(storage=None, enabled=True, daily_budget_usd=1.0)
    t._spent_usd = 1.5
    with pytest.raises(BudgetExceeded):
        t.enforce()


def test_tracker_enforce_passes_when_under():
    t = CostTracker(storage=None, enabled=True, daily_budget_usd=1.0)
    t._spent_usd = 0.5
    t.enforce()  # no raise


def test_tracker_enforce_noop_when_disabled():
    t = CostTracker(storage=None, enabled=False, daily_budget_usd=1.0)
    t._spent_usd = 99.0
    t.enforce()  # no raise
