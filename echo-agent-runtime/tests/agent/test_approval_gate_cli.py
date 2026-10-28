from echo_agent.agent.approval_gate import ApprovalGate


def test_gateway_cli_is_treated_as_interactive_channel():
    # gateway:cli 必须被视为可交互（可等待人工决策），不能落入无人值守拒绝
    # 或自动放行分支。用暴露的判断辅助 _is_interactive_channel 断言。
    assert ApprovalGate._is_interactive_channel("gateway:cli") is True
    assert ApprovalGate._is_interactive_channel("cli") is True
    assert ApprovalGate._is_interactive_channel("cron") is False
    assert ApprovalGate._is_interactive_channel("gateway:wechat") is False
