# Echo Agent compatibility copy: private imports/version; see VENDOR.md.
from typing import Any, Iterable, Type

from echo_agent._vendor.fastembed.common.types import NumpyArray
from echo_agent._vendor.fastembed.common.onnx_model import OnnxOutputContext
from echo_agent._vendor.fastembed.text.onnx_embedding import OnnxTextEmbedding, OnnxTextEmbeddingWorker
from echo_agent._vendor.fastembed.common.model_description import DenseModelDescription, ModelSource

supported_clip_models: list[DenseModelDescription] = [
    DenseModelDescription(
        model="Qdrant/clip-ViT-B-32-text",
        dim=512,
        description=(
            "Text embeddings, Multimodal (text&image), English, 77 input tokens truncation, "
            "Prefixes for queries/documents: not necessary, 2021 year"
        ),
        license="mit",
        size_in_GB=0.25,
        sources=ModelSource(hf="Qdrant/clip-ViT-B-32-text"),
        model_file="model.onnx",
    ),
]


class CLIPOnnxEmbedding(OnnxTextEmbedding):
    @classmethod
    def _get_worker_class(cls) -> Type[OnnxTextEmbeddingWorker]:
        return CLIPEmbeddingWorker

    @classmethod
    def _list_supported_models(cls) -> list[DenseModelDescription]:
        """Lists the supported models.

        Returns:
            list[DenseModelDescription]: A list of DenseModelDescription objects containing the model information.
        """
        return supported_clip_models

    def _post_process_onnx_output(
        self, output: OnnxOutputContext, **kwargs: Any
    ) -> Iterable[NumpyArray]:
        return output.model_output


class CLIPEmbeddingWorker(OnnxTextEmbeddingWorker):
    def init_embedding(
        self,
        model_name: str,
        cache_dir: str,
        **kwargs: Any,
    ) -> OnnxTextEmbedding:
        return CLIPOnnxEmbedding(
            model_name=model_name,
            cache_dir=cache_dir,
            threads=1,
            **kwargs,
        )
