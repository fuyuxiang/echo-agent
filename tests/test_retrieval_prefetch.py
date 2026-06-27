import time

from echo_agent.memory.prefetch import RetrievalCacheEntry, query_tokens, is_fresh


def _entry(text, scored=None, created=None):
    return RetrievalCacheEntry(
        query_text=text, query_tokens=query_tokens(text),
        scored=scored or [], created_at=created if created is not None else time.time(),
    )


def test_fresh_when_recent_and_similar():
    e = _entry("how to deploy the gateway service")
    assert is_fresh(e, "deploy gateway service steps", now=time.time(), ttl=60.0, jaccard_min=0.3)


def test_stale_when_expired():
    e = _entry("deploy gateway", created=time.time() - 120)
    assert not is_fresh(e, "deploy gateway", now=time.time(), ttl=60.0, jaccard_min=0.3)


def test_miss_when_topic_shifts():
    e = _entry("how to deploy the gateway service")
    assert not is_fresh(e, "what is my cat's name", now=time.time(), ttl=60.0, jaccard_min=0.3)


def test_empty_tokens_not_fresh():
    # Empty query tokens must not divide by zero and must miss.
    e = _entry("deploy gateway")
    assert not is_fresh(e, "", now=time.time(), ttl=60.0, jaccard_min=0.3)
    empty = _entry("")
    assert not is_fresh(empty, "deploy gateway", now=time.time(), ttl=60.0, jaccard_min=0.3)


def test_cjk_query_tokens():
    # Chinese queries tokenize via cjk_tokens (chars + bigrams), so a repeated
    # CJK query stays fresh.
    e = _entry("如何部署网关服务")
    assert is_fresh(e, "如何部署网关服务", now=time.time(), ttl=60.0, jaccard_min=0.3)
