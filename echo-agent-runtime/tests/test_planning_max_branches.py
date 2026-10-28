from echo_agent.agent.planning.strategies import TreeOfThoughtStrategy


def test_tot_branch_count_configurable():
    async def _fake_llm(**kwargs):  # pragma: no cover - not called
        raise NotImplementedError

    s = TreeOfThoughtStrategy(_fake_llm, max_branches=5)
    assert s._max_branches == 5

    s_default = TreeOfThoughtStrategy(_fake_llm)
    assert s_default._max_branches == 3
