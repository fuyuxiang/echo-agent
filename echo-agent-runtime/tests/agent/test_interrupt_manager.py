from echo_agent.agent.interrupt_manager import InterruptManager


def test_interrupt_flags_running_turn():
    m = InterruptManager()
    m.request("s1", "e1")
    assert m.is_interrupted("s1") is False
    assert m.interrupt("s1") is True
    assert m.is_interrupted("s1") is True


def test_interrupt_idle_session_is_noop():
    m = InterruptManager()
    assert m.interrupt("s1") is False      # 无运行中 turn
    assert m.is_interrupted("s1") is False


def test_interrupt_is_idempotent():
    m = InterruptManager()
    m.request("s1")
    assert m.interrupt("s1") is True
    assert m.interrupt("s1") is True       # 二次仍 True，不报错
    assert m.is_interrupted("s1") is True


def test_per_session_isolation():
    m = InterruptManager()
    m.request("s1")
    m.request("s2")
    m.interrupt("s1")
    assert m.is_interrupted("s1") is True
    assert m.is_interrupted("s2") is False  # 只影响目标会话


def test_clear_removes_flag():
    m = InterruptManager()
    m.request("s1")
    m.interrupt("s1")
    m.clear("s1")
    assert m.is_interrupted("s1") is False


def test_fresh_request_resets_stale_interrupt():
    # 上一轮被中断后 clear，再开新 turn 必须从未中断态开始，
    # 陈旧标志不能渗进下一轮。
    m = InterruptManager()
    m.request("s1")
    m.interrupt("s1")
    m.clear("s1")
    m.request("s1")                         # 新 turn
    assert m.is_interrupted("s1") is False


def test_targeted_interrupt_is_consumed_when_turn_registers_later():
    m = InterruptManager()
    m.admit("s1", "event-later")
    assert m.interrupt("s1", "event-later") is True

    m.request("s1", "event-later")

    assert m.is_interrupted("s1") is True


def test_unscoped_interrupt_binds_to_oldest_admitted_turn():
    m = InterruptManager()
    m.admit("s1", "event-first")
    m.admit("s1", "event-second")

    assert m.interrupt("s1") is True
    m.request("s1", "event-first")

    assert m.is_interrupted("s1") is True


def test_pending_target_never_interrupts_a_different_turn():
    m = InterruptManager()
    m.admit("s1", "event-old")
    assert m.interrupt("s1", "event-old") is True

    m.request("s1", "event-new")

    assert m.is_interrupted("s1") is False


def test_running_scope_distinguishes_current_from_queued_target():
    m = InterruptManager()
    m.request("s1", "event-current")
    m.admit("s1", "event-queued")

    assert m.targets_running("s1", "event-current") is True
    assert m.targets_running("s1") is True
    assert m.targets_running("s1", "event-queued") is False


def test_unadmitted_target_is_not_retained():
    m = InterruptManager()
    assert m.interrupt("s1", "forged-event") is False
    assert m._pending_targets == set()


def test_rejected_admission_discards_pending_stop():
    m = InterruptManager()
    m.admit("s1", "event-rejected")
    assert m.interrupt("s1", "event-rejected") is True

    m.discard("s1", "event-rejected")

    assert not m._admitted
    assert m._pending_targets == set()
