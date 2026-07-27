"""精排模型的自建镜像拉取:Gitee 分卷合并 + sha256 校验 + 原子落地。

Gitee 单个发布附件上限 100MiB,而精排包约 941MB,只能切成 10 卷托管;固定的 sha256
是"合并后整包"的摘要(分卷本身没有摘要)。所以这里固化三件事:分卷必须按序拼回原包、
摘要必须在合并之后校验、任何一卷缺失或拼错都必须整源失败而不是留下半成品缓存。

用本地 file:// URL 跑真实的 urlopen + tarfile 路径,不 mock 下载层,这样分卷顺序与
流式 hash 这两个真正容易错的点是被真实执行验证的。
"""

from __future__ import annotations

import hashlib
import io
import tarfile
import threading
import urllib.request
from pathlib import Path

import pytest

from echo_agent.memory import local_rerank
from echo_agent.memory.local_rerank import LocalReranker, _source_label

_REAL_URLOPEN = urllib.request.urlopen


def local_rerank_urlopen():
    """真实的 urlopen,供 monkeypatch 包装时回落使用(避免包到自己身上)。"""
    return _REAL_URLOPEN


def _make_model_tar(tmp_path: Path, subdir: str, onnx_bytes: bytes = b"onnx-weights") -> Path:
    """构造一个和真实发布包同布局的 tar.gz:顶层是 HF 缓存的 models--… 目录。"""
    tar_path = tmp_path / "pkg.tar.gz"
    with tarfile.open(tar_path, "w:gz") as tar:
        info = tarfile.TarInfo(f"{subdir}/snapshots/abc123/onnx/model.onnx")
        info.size = len(onnx_bytes)
        tar.addfile(info, io.BytesIO(onnx_bytes))
    return tar_path


def _split(tar_path: Path, out_dir: Path, count: int) -> list[str]:
    """把整包切成 count 卷,返回 file:// URL 列表(顺序即拼回顺序)。"""
    data = tar_path.read_bytes()
    size = len(data) // count + 1
    urls = []
    for i in range(count):
        part = out_dir / f"pkg.tar.gz.part-{i:02d}"
        part.write_bytes(data[i * size:(i + 1) * size])
        urls.append(part.as_uri())
    return urls


SUBDIR = "models--BAAI--bge-reranker-base"


@pytest.fixture
def pkg_env(tmp_path, monkeypatch):
    """一个 cache_dir + 一份可用发布包,_RELEASE_PACKAGES 指向本地 file:// 源。"""
    cache = tmp_path / "cache"
    cache.mkdir()
    src = tmp_path / "src"
    src.mkdir()
    tar_path = _make_model_tar(src, SUBDIR)
    digest = hashlib.sha256(tar_path.read_bytes()).hexdigest()

    def install(sources, sha256=digest):
        monkeypatch.setitem(
            local_rerank._RELEASE_PACKAGES,
            "test/model",
            {"cache_subdir": SUBDIR, "sha256": sha256, "sources": sources},
        )
        return LocalReranker(model_name="test/model", cache_dir=str(cache))

    return type(
        "Env",
        (),
        {
            "cache": cache,
            "src": src,
            "tar_path": tar_path,
            "digest": digest,
            "install": staticmethod(install),
        },
    )


class TestSplitVolumeFetch:
    def test_parts_are_joined_in_order_and_installed(self, pkg_env):
        """10 卷按序拼回整包 → 摘要通过 → 缓存落地为 HF 布局,可被命中。"""
        urls = _split(pkg_env.tar_path, pkg_env.src, 10)
        reranker = pkg_env.install([{"parts": urls}])

        assert reranker._fetch_release_package() is True
        onnx = list((pkg_env.cache / SUBDIR).glob("snapshots/*/onnx/model.onnx"))
        assert len(onnx) == 1 and onnx[0].stat().st_size > 0

    def test_second_call_is_a_cache_hit_without_download(self, pkg_env):
        """已就绪的缓存直接命中:把源指向不存在的 URL 也仍然返回 True。"""
        urls = _split(pkg_env.tar_path, pkg_env.src, 4)
        assert pkg_env.install([{"parts": urls}])._fetch_release_package() is True

        broken = pkg_env.install([{"url": "file:///nonexistent/pkg.tar.gz"}])
        assert broken._fetch_release_package() is True

    def test_missing_volume_aborts_source_and_leaves_no_cache(self, pkg_env):
        """中间一卷缺失 → 整源失败,且不留下半成品缓存(否则会被永久误判命中)。"""
        urls = _split(pkg_env.tar_path, pkg_env.src, 5)
        Path(urls[2][len("file://"):]).unlink()
        reranker = pkg_env.install([{"parts": urls}])

        assert reranker._fetch_release_package() is False
        assert not (pkg_env.cache / SUBDIR).exists()

    def test_out_of_order_join_is_rejected_by_sha256(self, pkg_env):
        """卷序错乱能拼成同样长度的文件,但摘要不符 → 必须拒绝。"""
        urls = _split(pkg_env.tar_path, pkg_env.src, 6)
        urls[0], urls[1] = urls[1], urls[0]
        reranker = pkg_env.install([{"parts": urls}])

        assert reranker._fetch_release_package() is False
        assert not (pkg_env.cache / SUBDIR).exists()

    def test_no_temp_files_left_behind_on_failure(self, pkg_env):
        """失败路径必须清干净临时文件与 staging,不能在缓存目录里堆垃圾。"""
        urls = _split(pkg_env.tar_path, pkg_env.src, 4)
        reranker = pkg_env.install([{"parts": urls}], sha256="00" * 32)

        assert reranker._fetch_release_package() is False
        assert list(pkg_env.cache.iterdir()) == []


class TestSourceFallback:
    def test_falls_back_to_whole_file_when_parts_fail(self, pkg_env):
        """分卷源坏掉时回退到整包源(对应线上 Gitee 失败 → GitHub)。"""
        reranker = pkg_env.install([
            {"parts": ["file:///nonexistent/a.part-00"]},
            {"url": pkg_env.tar_path.as_uri()},
        ])

        assert reranker._fetch_release_package() is True
        assert list((pkg_env.cache / SUBDIR).glob("snapshots/*/onnx/model.onnx"))

    def test_unknown_model_skips_release_path(self, pkg_env):
        """未打包的自定义模型不走自建镜像,直接交给 fastembed 的 HF 路径。"""
        reranker = LocalReranker(model_name="other/model", cache_dir=str(pkg_env.cache))
        assert reranker._fetch_release_package() is False

    def test_empty_cache_dir_skips_release_path(self, pkg_env):
        """cache_dir 为空时无处落地,必须直接放弃而不是往临时目录乱写。"""
        reranker = pkg_env.install([{"url": pkg_env.tar_path.as_uri()}])
        reranker._cache_dir = ""
        assert reranker._fetch_release_package() is False


class TestDownloadIsCancellable:
    """close() 必须能中止正在进行的约 941MB 拉取,并且不留残留文件。

    原实现只有 close() 里的 pool.shutdown(cancel_futures=True),它只能丢掉排队中的
    任务,对已经进入 urlopen().read() 的线程无效:用户即使关掉精排或直接退出进程,
    这一 GB 也会照抄完。这里固化"分块循环里检查 _closed"这条唯一可中断点。
    """

    def test_close_aborts_an_in_flight_download(self, pkg_env):
        urls = _split(pkg_env.tar_path, pkg_env.src, 6)
        reranker = pkg_env.install([{"parts": urls}])
        reranker.close()

        assert reranker._fetch_release_package() is False
        assert not (pkg_env.cache / SUBDIR).exists()
        assert list(pkg_env.cache.iterdir()) == []

    def test_close_midway_stops_and_cleans_up(self, pkg_env, monkeypatch):
        """在第 2 卷时置 _closed:后续卷不再拉,临时文件被清掉。"""
        urls = _split(pkg_env.tar_path, pkg_env.src, 6)
        reranker = pkg_env.install([{"parts": urls}])
        opened: list[str] = []
        real_urlopen = local_rerank_urlopen()

        def counting_urlopen(url, *args, **kwargs):
            opened.append(url)
            if len(opened) == 2:
                reranker.close()
            return real_urlopen(url, *args, **kwargs)

        monkeypatch.setattr("urllib.request.urlopen", counting_urlopen)

        assert reranker._fetch_release_package() is False
        assert len(opened) == 2  # 第 3 卷已经不再请求
        assert list(pkg_env.cache.iterdir()) == []

    def test_time_budget_aborts_a_stalled_download(self, pkg_env, monkeypatch):
        """总时长预算到点即放弃:极慢但不断续的传输不能永久占着线程和带宽。"""
        urls = _split(pkg_env.tar_path, pkg_env.src, 4)
        reranker = pkg_env.install([{"parts": urls}])
        monkeypatch.setattr(local_rerank, "_DOWNLOAD_BUDGET_SECONDS", 0.0)

        assert reranker._fetch_release_package() is False
        assert list(pkg_env.cache.iterdir()) == []

    def test_closed_reranker_does_not_try_the_next_source(self, pkg_env, monkeypatch):
        """关闭后不该因为第一个源失败就再开一个整包源。"""
        reranker = pkg_env.install([
            {"parts": ["file:///nonexistent/a.part-00"]},
            {"url": pkg_env.tar_path.as_uri()},
        ])
        opened: list[str] = []
        real_urlopen = local_rerank_urlopen()

        def tracking_urlopen(url, *args, **kwargs):
            opened.append(url)
            reranker.close()
            return real_urlopen(url, *args, **kwargs)

        monkeypatch.setattr("urllib.request.urlopen", tracking_urlopen)

        assert reranker._fetch_release_package() is False
        assert len(opened) == 1
        assert not (pkg_env.cache / SUBDIR).exists()

    def test_load_runs_on_a_daemon_thread(self, pkg_env, monkeypatch):
        """加载线程必须是 daemon:否则 concurrent.futures 的 atexit 会等下载跑完
        才让进程退出,Ctrl-C 也救不回来。"""
        import asyncio

        reranker = pkg_env.install([{"url": pkg_env.tar_path.as_uri()}])
        seen: dict[str, bool] = {}

        def fake_load():
            seen["daemon"] = threading.current_thread().daemon
            return object()

        monkeypatch.setattr(reranker, "_load_model_sync", fake_load)
        monkeypatch.setattr(type(reranker), "available", property(lambda self: True))

        assert asyncio.run(reranker._ensure_model()) is not None
        assert seen["daemon"] is True


class TestShippedPackageDefinition:
    """固化线上包定义,避免改 URL/卷数时和 install.sh 失去同步。"""

    def test_default_model_has_gitee_parts_before_github_whole(self):
        pkg = local_rerank._RELEASE_PACKAGES["BAAI/bge-reranker-base"]
        first, second = pkg["sources"][0], pkg["sources"][1]

        assert len(first["parts"]) == 10
        assert all("gitee.com" in u for u in first["parts"])
        assert [u[-2:] for u in first["parts"]] == [f"{i:02d}" for i in range(10)]
        assert "github.com" in second["url"]
        assert pkg["cache_subdir"] == SUBDIR

    def test_source_label_describes_both_shapes(self):
        assert _source_label({"url": "https://x/y.tar.gz"}) == "https://x/y.tar.gz"
        assert _source_label({"parts": ["https://x/p-00", "https://x/p-01"]}) == (
            "2 parts from https://x/p-00"
        )
