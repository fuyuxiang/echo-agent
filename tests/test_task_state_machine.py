# tests/test_task_state_machine.py
import pytest
from echo_agent.tasks.models import TaskStatus, VALID_TASK_TRANSITIONS, TaskRecord


def test_blocked_state_exists():
    assert TaskStatus.BLOCKED == "blocked"


def test_review_state_exists():
    assert TaskStatus.REVIEW == "review"


def test_running_can_transition_to_review():
    assert TaskStatus.REVIEW in VALID_TASK_TRANSITIONS[TaskStatus.RUNNING]


def test_running_can_transition_to_blocked():
    assert TaskStatus.BLOCKED in VALID_TASK_TRANSITIONS[TaskStatus.RUNNING]


def test_blocked_can_transition_to_queued():
    assert TaskStatus.QUEUED in VALID_TASK_TRANSITIONS[TaskStatus.BLOCKED]


def test_blocked_can_transition_to_running():
    assert TaskStatus.RUNNING in VALID_TASK_TRANSITIONS[TaskStatus.BLOCKED]


def test_review_can_transition_to_success():
    assert TaskStatus.SUCCESS in VALID_TASK_TRANSITIONS[TaskStatus.REVIEW]


def test_review_can_transition_to_queued():
    assert TaskStatus.QUEUED in VALID_TASK_TRANSITIONS[TaskStatus.REVIEW]


def test_task_record_new_fields():
    task = TaskRecord(
        title="test",
        labels=["bug"],
        assignee="agent-1",
        source="human",
        session_id="sess_abc",
        blocked_reason="",
        review_summary="",
        board_id="default",
    )
    d = task.to_dict()
    assert d["labels"] == ["bug"]
    assert d["assignee"] == "agent-1"
    assert d["source"] == "human"
    assert d["board_id"] == "default"


def test_task_record_from_dict_new_fields():
    d = {
        "id": "t_123", "title": "test", "status": "blocked",
        "labels": ["feat"], "assignee": "user",
        "source": "agent", "session_id": "s1",
        "blocked_reason": "waiting for input",
        "review_summary": "", "board_id": "default",
    }
    task = TaskRecord.from_dict(d)
    assert task.status == TaskStatus.BLOCKED
    assert task.labels == ["feat"]
    assert task.blocked_reason == "waiting for input"
