"""Configured caches are pinned once and reused for offline model loading."""
from types import SimpleNamespace

import pytest

from echo_agent.memory import local_embed, local_rerank
from echo_agent.runtime_paths import normalize_cache_dir


@pytest.mark.parametrize('value', ['', '~/.echo-agent/models', 'relative/models', '/absolute/models'])
def test_normalize_cache_dir(value, tmp_path, monkeypatch):
    monkeypatch.setenv('HOME', str(tmp_path / 'service-home'))
    monkeypatch.chdir(tmp_path)
    expected = {
        '': '', '~/.echo-agent/models': str(tmp_path / 'service-home/.echo-agent/models'),
        'relative/models': str(tmp_path / 'relative/models'), '/absolute/models': '/absolute/models',
    }[value]
    assert normalize_cache_dir(value) == expected


@pytest.mark.parametrize('module,cls,constructor', [
    (local_embed, local_embed.LocalEmbedder, 'TextEmbedding'),
    (local_rerank, local_rerank.LocalReranker, 'TextCrossEncoder'),
])
@pytest.mark.parametrize('value', ['', '~/.echo-agent/models', 'relative/models', '/absolute/models'])
def test_model_constructor_uses_pinned_cache(module, cls, constructor, value, tmp_path, monkeypatch):
    monkeypatch.setenv('HOME', str(tmp_path / 'service-home'))
    monkeypatch.chdir(tmp_path)
    model = cls(cache_dir=value)
    expected = normalize_cache_dir(value)
    monkeypatch.chdir(tmp_path.parent)
    monkeypatch.setattr(model, '_fetch_release_package', lambda: False)
    calls = []
    sentinel = object()
    def create(**kwargs):
        calls.append(kwargs)
        return sentinel
    monkeypatch.setattr(module, 'import_model_module', lambda _: SimpleNamespace(**{constructor: create}))
    try:
        assert model._load_model_sync() is sentinel
        assert calls[0].get('cache_dir', '') == expected
        assert ('cache_dir' in calls[0]) == bool(value)
    finally:
        model.close()


@pytest.mark.parametrize('module,cls,constructor,marker', [
    (local_embed, local_embed.LocalEmbedder, 'TextEmbedding', 'model_optimized.onnx'),
    (local_rerank, local_rerank.LocalReranker, 'TextCrossEncoder', 'snapshots/test/onnx/model.onnx'),
])
@pytest.mark.parametrize('value', ['~/.echo-agent/models', 'relative/models'])
def test_ready_cache_loads_without_any_download(module, cls, constructor, marker, value, tmp_path, monkeypatch):
    monkeypatch.setenv('HOME', str(tmp_path / 'service-home'))
    monkeypatch.chdir(tmp_path)
    model = cls(cache_dir=value)
    root = tmp_path / 'service-home/.echo-agent/models' if value.startswith('~') else tmp_path / value
    subdir = module._RELEASE_PACKAGES[model._model_name]['cache_subdir']
    weights = root / subdir / marker
    weights.parent.mkdir(parents=True)
    weights.write_bytes(b'controlled-offline-model')
    def no_network(*args, **kwargs):
        pytest.fail('a ready local cache must not be downloaded again')
    monkeypatch.setattr('urllib.request.urlopen', no_network)
    calls = []
    monkeypatch.setattr(module, 'import_model_module', lambda _: SimpleNamespace(**{
        constructor: lambda **kwargs: calls.append(kwargs) or object(),
    }))
    try:
        assert model._load_model_sync() is not None
        assert calls[0]['cache_dir'] == str(root)
        assert calls[0]['local_files_only'] is True
    finally:
        model.close()
