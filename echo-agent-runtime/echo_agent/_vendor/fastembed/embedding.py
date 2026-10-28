# Echo Agent compatibility copy: private imports/version; see VENDOR.md.
from typing import Any

from loguru import logger

from echo_agent._vendor.fastembed import TextEmbedding

logger.warning(
    "DefaultEmbedding, FlagEmbedding, JinaEmbedding are deprecated."
    "Use from fastembed import TextEmbedding instead."
)

DefaultEmbedding = TextEmbedding
FlagEmbedding = TextEmbedding


class JinaEmbedding(TextEmbedding):
    def __init__(
        self,
        model_name: str = "jinaai/jina-embeddings-v2-base-en",
        cache_dir: str | None = None,
        threads: int | None = None,
        **kwargs: Any,
    ):
        super().__init__(model_name, cache_dir, threads, **kwargs)
