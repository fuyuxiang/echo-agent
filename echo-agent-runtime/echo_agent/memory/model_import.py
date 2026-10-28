"""Serialize cold imports shared by the two background model loaders.

FastEmbed's embedding and reranking packages import the same submodules in
different orders. Concurrent first imports can trip Python's module-lock
deadlock detection. Only imports share this lock; model inference stays parallel.
"""

import importlib
import threading
from types import ModuleType

_IMPORT_LOCK = threading.RLock()


def import_model_module(name: str) -> ModuleType:
    with _IMPORT_LOCK:
        return importlib.import_module(name)
