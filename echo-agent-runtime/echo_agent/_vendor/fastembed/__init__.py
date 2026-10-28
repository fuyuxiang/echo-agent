# Echo Agent compatibility copy: private imports/version; see VENDOR.md.
from echo_agent._vendor.fastembed.image import ImageEmbedding
from echo_agent._vendor.fastembed.late_interaction import LateInteractionTextEmbedding
from echo_agent._vendor.fastembed.late_interaction_multimodal import LateInteractionMultimodalEmbedding
from echo_agent._vendor.fastembed.sparse import SparseEmbedding, SparseTextEmbedding
from echo_agent._vendor.fastembed.text import TextEmbedding

__version__ = version = "0.9.0+echo.1"
__all__ = [
    "TextEmbedding",
    "SparseTextEmbedding",
    "SparseEmbedding",
    "ImageEmbedding",
    "LateInteractionTextEmbedding",
    "LateInteractionMultimodalEmbedding",
]
