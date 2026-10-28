"""Contract tests for optional/default storage backend capabilities.

The abstract CRUD methods contain no production logic; concrete backends are
covered separately.  These tests pin the fallback semantics that callers rely
on when a backend does not implement optional vector, SQL, or archive features.
"""

from __future__ import annotations

import pytest

from echo_agent.storage.backend import StorageBackend


def test_storage_backend_cannot_be_instantiated_without_core_crud() -> None:
    with pytest.raises(TypeError):
        StorageBackend()


@pytest.mark.asyncio
async def test_optional_capabilities_have_safe_empty_defaults() -> None:
    # Invoke the stateless default implementations directly: constructing the
    # abstract interface is deliberately forbidden by the test above.
    backend = None
    assert await StorageBackend.list_sessions(backend) == []  # type: ignore[arg-type]
    assert await StorageBackend.load_vectors_all(backend) == []  # type: ignore[arg-type]
    assert await StorageBackend.load_vector_by_source(backend, "source") is None  # type: ignore[arg-type]
    assert await StorageBackend.delete_vector(backend, "vector") is None  # type: ignore[arg-type]
    assert await StorageBackend.execute_sql(backend, "UPDATE x SET y = 1") is None  # type: ignore[arg-type]
    assert await StorageBackend.fetch_sql(backend, "SELECT 1") == []  # type: ignore[arg-type]
    assert await StorageBackend.archive_messages(backend, "session", []) is None  # type: ignore[arg-type]
    assert await StorageBackend.load_archived_messages(backend, "session") == []  # type: ignore[arg-type]
