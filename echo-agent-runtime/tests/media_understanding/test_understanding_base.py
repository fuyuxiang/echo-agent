from echo_agent.agent.media.understanding.base import (
    MediaUnderstanding,
    UnderstandResult,
)


def test_understand_result_defaults():
    r = UnderstandResult(text="hi", kind="transcript")
    assert r.text == "hi"
    assert r.kind == "transcript"
    assert r.metadata == {}


def test_result_metadata_is_independent_per_instance():
    a = UnderstandResult(text="", kind="transcript")
    b = UnderstandResult(text="", kind="transcript")
    a.metadata["lang"] = "zh"
    assert b.metadata == {}  # no shared mutable default


def test_protocol_runtime_checkable_accepts_conforming_object():
    class _Ok:
        def can_handle(self, block):
            return True

        async def understand(self, path, block):
            return UnderstandResult(text="x", kind="transcript")

    assert isinstance(_Ok(), MediaUnderstanding)


def test_protocol_rejects_missing_method():
    class _Bad:
        def can_handle(self, block):
            return True

    assert not isinstance(_Bad(), MediaUnderstanding)
