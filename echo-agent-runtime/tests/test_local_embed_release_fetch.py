"""LocalEmbedder._fetch_release_package 自有 release 源下载测试（不真实联网）。"""
import hashlib
import io
import tarfile
from unittest.mock import MagicMock, patch

from echo_agent.memory import local_embed
from echo_agent.memory.local_embed import LocalEmbedder


def _make_tar_bytes(subdir: str) -> bytes:
    """构造一个合法的小 tar.gz：顶层目录 == subdir，内含 model_optimized.onnx。"""
    buf = io.BytesIO()
    with tarfile.open(fileobj=buf, mode="w:gz") as tar:
        payload = b"fake-onnx-weights"
        info = tarfile.TarInfo(name=f"{subdir}/model_optimized.onnx")
        info.size = len(payload)
        tar.addfile(info, io.BytesIO(payload))
    return buf.getvalue()


def test_cache_hit_skips_download(tmp_path):
    """缓存已就绪（关键文件非零）时不发起下载，直接返回 True。"""
    cache_dir = tmp_path / "fastembed"
    target = cache_dir / "fast-bge-small-zh-v1.5"
    target.mkdir(parents=True)
    (target / "model_optimized.onnx").write_bytes(b"fake-onnx-weights")

    e = LocalEmbedder("BAAI/bge-small-zh-v1.5", cache_dir=str(cache_dir))
    with patch("urllib.request.urlopen") as mock_open:
        assert e._fetch_release_package() is True
        mock_open.assert_not_called()


def test_empty_marker_is_not_cache_hit(tmp_path, monkeypatch):
    """半成品缓存(存在但空的 model_optimized.onnx)不算命中:应重新下载并原子落地,
    而非因空壳存在就永久跳过下载(P6)。"""
    cache_dir = tmp_path / "fastembed"
    subdir = "fast-bge-small-zh-v1.5"
    # 预置一个空壳,模拟上次解压中断留下的半成品。
    stale = cache_dir / subdir
    stale.mkdir(parents=True)
    (stale / "model_optimized.onnx").write_bytes(b"")

    tar_bytes = _make_tar_bytes(subdir)
    digest = hashlib.sha256(tar_bytes).hexdigest()
    monkeypatch.setitem(
        local_embed._RELEASE_PACKAGES,
        "BAAI/bge-small-zh-v1.5",
        {"cache_subdir": subdir, "sha256": digest, "urls": ["https://mirror.invalid/pkg.tar.gz"]},
    )

    def fake_urlopen(url, timeout=None):
        resp = MagicMock()
        resp.__enter__.return_value = io.BytesIO(tar_bytes)
        resp.__exit__.return_value = False
        return resp

    e = LocalEmbedder("BAAI/bge-small-zh-v1.5", cache_dir=str(cache_dir))
    with patch("urllib.request.urlopen", side_effect=fake_urlopen) as mock_open:
        assert e._fetch_release_package() is True
        mock_open.assert_called_once()
    # 空壳被替换成完整非零文件,且无 staging 残留。
    onnx = cache_dir / subdir / "model_optimized.onnx"
    assert onnx.read_bytes() == b"fake-onnx-weights"
    leftovers = [p.name for p in cache_dir.iterdir() if p.name.startswith(".staging-")]
    assert leftovers == []


def test_sha256_mismatch_tries_all_and_leaves_no_residue(tmp_path):
    """sha256 不匹配时拒绝、尝试下一源，最终返回 False 且不留解压产物/临时文件。"""
    cache_dir = tmp_path / "fastembed"
    e = LocalEmbedder("BAAI/bge-small-zh-v1.5", cache_dir=str(cache_dir))

    def fake_urlopen(url, timeout=None):
        resp = MagicMock()
        resp.__enter__.return_value = io.BytesIO(b"garbage-content")
        resp.__exit__.return_value = False
        return resp

    with patch("urllib.request.urlopen", side_effect=fake_urlopen) as mock_open:
        assert e._fetch_release_package() is False
        # 两个 URL 都尝试过
        assert mock_open.call_count == 2

    # 未解压出目标目录，且无 .part/临时残留
    assert not (cache_dir / "fast-bge-small-zh-v1.5").exists()
    leftovers = list(cache_dir.iterdir()) if cache_dir.exists() else []
    assert leftovers == []


def test_first_source_fails_second_succeeds(tmp_path, monkeypatch):
    """首源抛异常、次源返回合法 tar.gz：返回 True 且文件落位。"""
    cache_dir = tmp_path / "fastembed"
    subdir = "fast-bge-small-zh-v1.5"
    tar_bytes = _make_tar_bytes(subdir)
    digest = hashlib.sha256(tar_bytes).hexdigest()

    # 用实时算出的 sha256 覆盖包定义，避免依赖真实模型哈希
    monkeypatch.setitem(
        local_embed._RELEASE_PACKAGES,
        "BAAI/bge-small-zh-v1.5",
        {
            "cache_subdir": subdir,
            "sha256": digest,
            "urls": ["https://first.invalid/pkg.tar.gz", "https://second.invalid/pkg.tar.gz"],
        },
    )

    calls = []

    def fake_urlopen(url, timeout=None):
        calls.append(url)
        if url.startswith("https://first"):
            raise OSError("connection refused")
        resp = MagicMock()
        resp.__enter__.return_value = io.BytesIO(tar_bytes)
        resp.__exit__.return_value = False
        return resp

    e = LocalEmbedder("BAAI/bge-small-zh-v1.5", cache_dir=str(cache_dir))
    with patch("urllib.request.urlopen", side_effect=fake_urlopen):
        assert e._fetch_release_package() is True

    assert len(calls) == 2
    assert (cache_dir / subdir / "model_optimized.onnx").exists()


def test_unpackaged_model_returns_false_without_download(tmp_path):
    """非打包模型直接返回 False，不发起下载。"""
    cache_dir = tmp_path / "fastembed"
    e = LocalEmbedder("BAAI/bge-small-en-v1.5", cache_dir=str(cache_dir))
    with patch("urllib.request.urlopen") as mock_open:
        assert e._fetch_release_package() is False
        mock_open.assert_not_called()


def test_no_cache_dir_returns_false(tmp_path):
    """未配置 cache_dir 时返回 False（无处落地缓存）。"""
    e = LocalEmbedder("BAAI/bge-small-zh-v1.5", cache_dir="")
    with patch("urllib.request.urlopen") as mock_open:
        assert e._fetch_release_package() is False
        mock_open.assert_not_called()
