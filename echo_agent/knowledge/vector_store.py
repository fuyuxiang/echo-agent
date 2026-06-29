"""FAISS-backed vector store for the knowledge index with sidecar persistence.

Independent from echo_agent.memory.vectors.VectorIndex (which is bound to the
global SQLite vectors table). Knowledge vectors live in a sidecar file next to
the JSON index, keeping them physically isolated from memory vectors and
leaving StorageBackend untouched. Degrades to empty results when faiss/numpy
are unavailable.
"""

from __future__ import annotations

from pathlib import Path

from loguru import logger

try:
    import faiss
    import numpy as np
    _HAS_FAISS = True
except ImportError:
    faiss = None
    np = None
    _HAS_FAISS = False


class KnowledgeVectorStore:
    def __init__(self, sidecar_path: Path, dimensions: int = 1536):
        self._sidecar = Path(sidecar_path)
        self._dim = dimensions
        self._index = None
        self._chunk_ids: list[str] = []

    @property
    def available(self) -> bool:
        return _HAS_FAISS

    def load(self) -> dict[str, list[float]]:
        if not _HAS_FAISS or not self._sidecar.exists():
            return {}
        try:
            data = np.load(self._sidecar, allow_pickle=False)
            vectors = data["vectors"]
            chunk_ids = [str(c) for c in data["chunk_ids"]]
            if vectors.shape[1] != self._dim or vectors.shape[0] != len(chunk_ids):
                logger.warning("Knowledge sidecar shape mismatch; discarding {}", self._sidecar)
                return {}
            return {cid: vectors[i].tolist() for i, cid in enumerate(chunk_ids)}
        except Exception as e:
            logger.warning("Failed to load knowledge sidecar {}: {}", self._sidecar, e)
            return {}

    def build(self, ordered: list[tuple[str, list[float]]]) -> None:
        if not _HAS_FAISS or not ordered:
            self._index = None
            self._chunk_ids = []
            return
        matrix = np.array([v for _, v in ordered], dtype=np.float32)
        if matrix.shape[1] != self._dim:
            logger.warning("Knowledge vector dim mismatch {} vs {}", matrix.shape[1], self._dim)
            self._index = None
            self._chunk_ids = []
            return
        faiss.normalize_L2(matrix)
        index = faiss.IndexFlatIP(self._dim)
        index.add(matrix)
        self._index = index
        self._chunk_ids = [cid for cid, _ in ordered]

    def save(self, ordered: list[tuple[str, list[float]]]) -> None:
        if not _HAS_FAISS:
            return
        self._sidecar.parent.mkdir(parents=True, exist_ok=True)
        if not ordered:
            if self._sidecar.exists():
                self._sidecar.unlink()
            return
        matrix = np.array([v for _, v in ordered], dtype=np.float32)
        faiss.normalize_L2(matrix)
        chunk_ids = np.array([cid for cid, _ in ordered], dtype=str)
        np.savez(self._sidecar, vectors=matrix, chunk_ids=chunk_ids, dim=np.array([self._dim]))

    def search(self, query_vec: list[float], limit: int) -> list[tuple[str, float]]:
        if not _HAS_FAISS or self._index is None or self._index.ntotal == 0:
            return []
        arr = np.array(query_vec, dtype=np.float32).reshape(1, -1)
        if arr.shape[1] != self._dim:
            return []
        faiss.normalize_L2(arr)
        k = min(limit, self._index.ntotal)
        scores, indices = self._index.search(arr, k)
        out: list[tuple[str, float]] = []
        for score, idx in zip(scores[0], indices[0]):
            if 0 <= idx < len(self._chunk_ids):
                out.append((self._chunk_ids[idx], float(score)))
        return out
