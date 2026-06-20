from echo_agent.a2a.models import AgentCard


def test_agentcard_accepts_capabilities():
    card = AgentCard(
        name="x", description="y", url="http://localhost",
        capabilities=["chat", "search"],
    )
    assert card.capabilities == ["chat", "search"]
