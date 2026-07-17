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
