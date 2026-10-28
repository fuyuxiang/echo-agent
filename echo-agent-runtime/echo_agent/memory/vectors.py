"""numpy vector index — embedding storage, similarity search, row-level delete.

Replaces the FAISS backend for memory vectors: at memory scale (~1500 entries)
a normalized dot product over an in-memory float32 matrix is milliseconds, and
row-level deletes eliminate the tombstone/rebuild machinery entirely. Vectors
persist in the SQLite ``vectors`` table stamped with the embedding model id and
dimension, so a model switch is detected at load time (stale_source_ids)."""

from __future__ import annotations

import asyncio
import uuid
from typing import TYPE_CHECKING

import numpy as np
from loguru import logger

if TYPE_CHECKING:
    from echo_agent.storage.backend import StorageBackend


class VectorIndex:
    """numpy-backed vector index with SQLite persistence.

    dimensions=0 means "adopt the dimension of the first vector seen".
    ``model_id`` stamps every stored vector; rows whose stamp differs from the
    current model_id are NOT loaded into the matrix — their source_ids surface
    in ``stale_source_ids`` for the startup re-embed queue."""

    def __init__(self, storage: StorageBackend, dimensions: int = 0, model_id: str = ""):
        self._storage = storage
        self._dimensions = dimensions
        self._model_id = model_id
        self._matrix: np.ndarray | None = None  # (N, dim) L2-normalized float32
        self._id_map: list[str] = []
        self._source_map: list[str] = []
        self._stale_source_ids: set[str] = set()
        self._initialized = False
        self._lock = asyncio.Lock()

    @property
    def available(self) -> bool:
        return True

    @property
    def count(self) -> int:
        return 0 if self._matrix is None else self._matrix.shape[0]

    @property
    def dimensions(self) -> int:
        return self._dimensions

    @property
    def stale_source_ids(self) -> set[str]:
        return set(self._stale_source_ids)

    @property
    def loaded_vec_ids(self) -> set[str]:
        """vec_ids currently present in the matrix. Lets the backfill queue
        detect dangling entry.embedding_id references (e.g. the vectors DB was
        deleted while the authoritative JSON memory files survived)."""
        return set(self._id_map)

    @staticmethod
    def _normalize(arr: np.ndarray) -> np.ndarray:
        norms = np.linalg.norm(arr, axis=1, keepdims=True)
        norms[norms == 0] = 1.0
        return arr / norms

    async def initialize(self) -> None:
        async with self._lock:
            await self._initialize_unlocked()

    async def _initialize_unlocked(self) -> None:
        self._id_map.clear()
        self._source_map.clear()
        self._stale_source_ids.clear()
        self._matrix = None
        rows = await self._storage.load_vectors_all()
        vectors: list[np.ndarray] = []
        for row in rows:
            model = row.get("model", "") or ""
            if model != self._model_id:
                self._stale_source_ids.add(row.get("source_id", ""))
                continue
            emb = row.get("embedding")
            if emb is None:
                continue
            arr = np.frombuffer(emb, dtype=np.float32)
            if arr.shape[0] == 0:
                # Corrupted/zero-byte blob: never adopt 0 as the index
                # dimension (a later vstack would crash startup); route the
                # entry through the re-embed queue instead.
                self._stale_source_ids.add(row.get("source_id", ""))
                continue
            if self._dimensions == 0:
                self._dimensions = arr.shape[0]
            if arr.shape[0] != self._dimensions:
                self._stale_source_ids.add(row.get("source_id", ""))
                continue
            vectors.append(arr)
            self._id_map.append(row["id"])
            self._source_map.append(row.get("source_id", ""))
        self._stale_source_ids.discard("")
        if vectors:
            self._matrix = self._normalize(np.vstack(vectors).astype(np.float32))
        self._initialized = True
        logger.info(
            "Vector index loaded: {} vectors, {} dims, {} stale (model mismatch)",
            self.count, self._dimensions, len(self._stale_source_ids),
        )

    async def add(self, memory_id: str, embedding: list[float]) -> str:
        async with self._lock:
            arr = np.array(embedding, dtype=np.float32).reshape(1, -1)
            if arr.shape[1] == 0:
                logger.warning("Rejected empty embedding for {}", memory_id)
                return ""
            if self._dimensions == 0:
                self._dimensions = arr.shape[1]
            if arr.shape[1] != self._dimensions:
                logger.warning(
                    "Embedding dimension mismatch: {} vs {}", arr.shape[1], self._dimensions,
                )
                return ""
            vec_id = uuid.uuid4().hex[:12]
            await self._storage.store_vector(
                vec_id, memory_id, arr.tobytes(), {},
                model=self._model_id, dim=self._dimensions,
            )
            normalized = self._normalize(arr)
            if self._matrix is None:
                self._matrix = normalized
            else:
                self._matrix = np.vstack([self._matrix, normalized])
            self._id_map.append(vec_id)
            self._source_map.append(memory_id)
            return vec_id

    async def search(self, query_embedding: list[float], limit: int = 10) -> list[tuple[str, float]]:
        """Search for similar vectors. Returns (source_id, score) pairs."""
        async with self._lock:
            if self._matrix is None or self.count == 0:
                return []
            arr = np.array(query_embedding, dtype=np.float32).reshape(1, -1)
            if arr.shape[1] != self._dimensions:
                return []
            query = self._normalize(arr)[0]
            scores = self._matrix @ query
            k = min(limit, scores.shape[0])
            top = np.argsort(scores)[::-1][:k]
            return [(self._source_map[i], float(scores[i])) for i in top]

    async def remove(self, vec_id: str) -> None:
        async with self._lock:
            if vec_id in self._id_map:
                idx = self._id_map.index(vec_id)
                self._id_map.pop(idx)
                self._source_map.pop(idx)
                if self._matrix is not None:
                    self._matrix = np.delete(self._matrix, idx, axis=0)
                    if self._matrix.shape[0] == 0:
                        self._matrix = None
            await self._storage.delete_vector(vec_id)

    async def rebuild(self) -> None:
        async with self._lock:
            dims = self._dimensions
            await self._initialize_unlocked()
            if self._dimensions == 0:
                self._dimensions = dims
