from echo_agent.models.provider import LLMProvider


class _Dummy(LLMProvider):
    async def chat(self, **kwargs):  # pragma: no cover - not invoked here
        raise NotImplementedError

    async def chat_stream(self, **kwargs):  # pragma: no cover
        raise NotImplementedError

    def get_default_model(self) -> str:  # pragma: no cover
        return ""


def test_retry_delays_track_max_retries():
    p = _Dummy()
    p.max_retries = 5
    assert p._retry_delays() == [1, 2, 4, 8, 16]

    p.max_retries = 1
    assert p._retry_delays() == [1]
