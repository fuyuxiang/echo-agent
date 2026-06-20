from echo_agent.a2a.models import AgentCard


def test_agentcard_accepts_capabilities():
    card = AgentCard(
        name="x", description="y", url="http://localhost",
        capabilities=["chat", "search"],
    )
    assert card.capabilities == ["chat", "search"]


def test_capabilities_reach_serialized_skills():
    # Guards against "fake capabilities": configured capability tags must be
    # visible in the serialized agent card (the only path to /.well-known/agent.json).
    d = AgentCard(capabilities=["chat", "search"]).to_dict()
    skill_ids = [s["id"] for s in d["skills"]]
    assert "chat" in skill_ids
    assert "search" in skill_ids
    # `capabilities` key stays the reserved A2A object, unchanged.
    assert d["capabilities"] == {"streaming": True, "pushNotifications": False}
