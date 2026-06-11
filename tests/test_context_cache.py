"""Tests for context cache and memory content sanitization."""


from echo_agent.agent.context import _sanitize_memory_content, build_memory_context
from echo_agent.agent.context_cache import ContextCache


class TestSanitizeMemoryContent:
    def test_escapes_curly_braces(self):
        assert _sanitize_memory_content("use {name}") == "use {{name}}"

    def test_no_change_without_braces(self):
        assert _sanitize_memory_content("plain text") == "plain text"

    def test_nested_braces(self):
        assert _sanitize_memory_content("{a{b}}") == "{{a{{b}}}}"

    def test_empty_string(self):
        assert _sanitize_memory_content("") == ""


class TestContextCache:
    def setup_method(self):
        self.cache = ContextCache(max_size=3)

    def test_put_and_get(self):
        self.cache.put("s1", "skills_a", "caps_a", "cached_value")
        result = self.cache.get("s1", "skills_a", "caps_a")
        assert result == "cached_value"

    def test_miss_on_different_skills(self):
        self.cache.put("s1", "skills_a", "caps_a", "cached_value")
        result = self.cache.get("s1", "skills_b", "caps_a")
        assert result is None

    def test_miss_on_different_capabilities(self):
        self.cache.put("s1", "skills_a", "caps_a", "cached_value")
        result = self.cache.get("s1", "skills_a", "caps_b")
        assert result is None

    def test_lru_eviction(self):
        self.cache.put("s1", "sk", "ca", "v1")
        self.cache.put("s2", "sk", "ca", "v2")
        self.cache.put("s3", "sk", "ca", "v3")
        self.cache.put("s4", "sk", "ca", "v4")
        assert self.cache.get("s1", "sk", "ca") is None
        assert self.cache.get("s4", "sk", "ca") == "v4"

    def test_invalidate_session(self):
        self.cache.put("s1", "sk1", "ca", "v1")
        self.cache.put("s1", "sk2", "ca", "v2")
        self.cache.put("s2", "sk1", "ca", "v3")
        self.cache.invalidate("s1")
        assert self.cache.get("s1", "sk1", "ca") is None
        assert self.cache.get("s1", "sk2", "ca") is None
        assert self.cache.get("s2", "sk1", "ca") == "v3"

    def test_clear(self):
        self.cache.put("s1", "sk", "ca", "v1")
        self.cache.clear()
        assert self.cache.get("s1", "sk", "ca") is None


class TestBuildMemoryContextSanitization:
    def test_snapshot_is_sanitized(self):
        result = build_memory_context(None, snapshot="user prefers {format}")
        assert "{{format}}" in result

    def test_working_memory_is_sanitized(self):
        result = build_memory_context(None, working_memory="key={value}")
        assert "{{value}}" in result
