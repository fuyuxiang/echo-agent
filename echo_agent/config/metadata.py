"""Traversal utilities over the Config schema and its field metadata.

Pure functions: read the Pydantic model definition, yield per-field info.
No side effects, no disk access.
"""
from __future__ import annotations

from collections.abc import Iterator
from dataclasses import dataclass, field
from typing import Any, Literal, get_args, get_origin

from pydantic import BaseModel
from pydantic.alias_generators import to_camel

from echo_agent.config.schema import Config, _Base


@dataclass
class FieldInfo:
    path: str
    snake_path: str
    type_str: str
    default: Any
    choices: list[str] | None
    extra: dict[str, Any] = field(default_factory=dict)


def _is_base_submodel(annotation: Any) -> type[BaseModel] | None:
    """Return the model class if annotation is a _Base subclass, else None."""
    if isinstance(annotation, type) and issubclass(annotation, _Base):
        return annotation
    return None


def _container_submodel(annotation: Any) -> tuple[type[BaseModel], str] | None:
    """Return (element model, path marker) for a container of _Base submodels.

    Recognizes ``list[_Base]`` (marker ``[]``) and ``dict[str, _Base]``
    (marker ``{}``). Containers whose element is not a _Base subclass (e.g.
    ``list[str]``, ``dict[str, str]``) return None and stay leaf fields.
    """
    origin = get_origin(annotation)
    args = get_args(annotation)
    if origin is list and args:
        elem = args[0]
        if isinstance(elem, type) and issubclass(elem, _Base):
            return elem, "[]"
    elif origin is dict and len(args) == 2:
        val = args[1]
        if isinstance(val, type) and issubclass(val, _Base):
            return val, "{}"
    return None


def _literal_choices(annotation: Any) -> list[str] | None:
    if get_origin(annotation) is Literal:
        return [str(a) for a in get_args(annotation)]
    return None


def _type_str(annotation: Any) -> str:
    name = getattr(annotation, "__name__", None)
    if name:
        return name
    return str(annotation).replace("typing.", "")


def iter_fields(
    model: type[BaseModel] = Config,
    prefix: str = "",
    snake_prefix: str = "",
) -> Iterator[FieldInfo]:
    for name, fld in model.model_fields.items():
        camel = to_camel(name)
        path = f"{prefix}.{camel}" if prefix else camel
        snake_path = f"{snake_prefix}.{name}" if snake_prefix else name
        annotation = fld.annotation
        submodel = _is_base_submodel(annotation)
        if submodel is not None:
            yield from iter_fields(submodel, path, snake_path)
            continue
        extra = fld.json_schema_extra if isinstance(fld.json_schema_extra, dict) else {}
        yield FieldInfo(
            path=path,
            snake_path=snake_path,
            type_str=_type_str(annotation),
            default=fld.get_default(call_default_factory=True),
            choices=_literal_choices(annotation),
            extra=dict(extra),
        )
        container = _container_submodel(annotation)
        if container is not None:
            elem_model, marker = container
            yield from iter_fields(
                elem_model,
                f"{path}{marker}",
                f"{snake_path}{marker}",
            )
