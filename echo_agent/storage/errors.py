"""Storage-layer error semantics.

Distinguishes three outcomes a read can have, so callers stop conflating them:

- NotFound — the row/file genuinely does not exist. Expressed by returning
  ``None`` (single) or ``[]`` (list), NOT by an exception. Callers may safely
  create a fresh record.
- StorageUnavailable — the backend could not be reached or an IO error occurred
  (connection lost, disk error). The data may well exist; the caller MUST NOT
  treat this as "absent" and overwrite it with an empty value.
- CorruptData — a row/file was read but could not be deserialized. The data is
  present but unusable; surfacing it lets callers quarantine rather than clobber.
"""

from __future__ import annotations


class StorageError(Exception):
    """Base class for all storage-layer failures."""


class StorageUnavailable(StorageError):
    """The backend could not be reached / an IO error occurred.

    Raised for ``aiosqlite.Error`` and ``OSError``. Signals a transient runtime
    fault — the underlying data likely still exists, so callers must never
    downgrade this to "not found" and overwrite it.
    """


class CorruptData(StorageError):
    """A record was fetched but could not be deserialized.

    Raised for ``json.JSONDecodeError`` / ``ValueError`` / ``KeyError`` /
    ``TypeError`` during row decoding. The data is present but unusable.
    """
