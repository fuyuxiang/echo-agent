import hashlib
import http.client
import importlib.util
import io
from pathlib import Path
import tempfile
import unittest
import urllib.error
import zipfile

spec = importlib.util.spec_from_file_location("setup_protoc", Path(__file__).with_name("setup-protoc.py"))
setup_protoc = importlib.util.module_from_spec(spec)
spec.loader.exec_module(setup_protoc)


class SetupProtocTests(unittest.TestCase):
    def test_pinned_assets_use_direct_downloads_for_each_ci_host(self):
        for system, machine in [("Linux", "x86_64"), ("Linux", "aarch64"),
                                ("Darwin", "arm64"), ("Darwin", "x86_64"), ("Windows", "AMD64")]:
            with self.subTest(system=system, machine=machine):
                url, digest = setup_protoc.asset_for(system, machine)
                self.assertIn("/releases/download/v29.3/protoc-29.3-", url)
                self.assertEqual(len(digest), 64)
        with self.assertRaisesRegex(ValueError, "Unsupported protoc host"):
            setup_protoc.asset_for("Windows", "arm64")

    def test_timeout_is_retried_before_validating_the_download(self):
        attempts, delays = [], []
        data = b"a complete download"

        def open_url(url, timeout):
            attempts.append((url, timeout))
            if len(attempts) == 1:
                raise TimeoutError("Request timeout")
            return io.BytesIO(data)

        with tempfile.TemporaryDirectory() as temp:
            destination = Path(temp) / "protoc.zip"
            setup_protoc.download("https://example.test/protoc.zip", destination,
                                  hashlib.sha256(data).hexdigest(), open_url=open_url, sleep=delays.append)
            self.assertEqual(destination.read_bytes(), data)
        self.assertEqual(len(attempts), 2)
        self.assertEqual(delays, [2])

    def test_failed_download_is_bounded_and_keeps_the_original_error(self):
        attempts, delays = [], []

        def open_url(url, timeout):
            attempts.append(url)
            raise urllib.error.URLError("connection reset")

        with self.assertRaisesRegex(RuntimeError, "failed after 3 attempts") as failure:
            setup_protoc.download("https://example.test/protoc.zip", Path("unused"), "unused",
                                  open_url=open_url, sleep=delays.append)
        self.assertIsInstance(failure.exception.__cause__, urllib.error.URLError)
        self.assertEqual(len(attempts), 3)
        self.assertEqual(delays, [2, 4])

    def test_missing_release_is_not_retried(self):
        def open_url(url, timeout):
            raise urllib.error.HTTPError(url, 404, "Not Found", {}, None)

        with self.assertRaises(urllib.error.HTTPError):
            setup_protoc.download("https://example.test/missing", Path("unused"), "unused",
                                  open_url=open_url, sleep=lambda _: self.fail("404 must not retry"))

    def test_rate_limits_server_failures_and_incomplete_responses_are_retried(self):
        url = "https://example.test/protoc.zip"
        for error in [urllib.error.HTTPError(url, 429, "Rate limited", {}, None),
                      urllib.error.HTTPError(url, 503, "Unavailable", {}, None),
                      http.client.IncompleteRead(b"partial")]:
            with self.subTest(error=error), tempfile.TemporaryDirectory() as temp:
                attempts = []

                def open_url(url, timeout):
                    attempts.append(url)
                    if len(attempts) == 1:
                        raise error
                    return io.BytesIO(b"complete")

                destination = Path(temp) / "protoc.zip"
                setup_protoc.download(url, destination, hashlib.sha256(b"complete").hexdigest(),
                                      open_url=open_url, sleep=lambda _: None)
                self.assertEqual(destination.read_bytes(), b"complete")
                self.assertEqual(len(attempts), 2)

    def test_checksum_failure_never_writes_an_archive(self):
        with tempfile.TemporaryDirectory() as temp:
            destination = Path(temp) / "protoc.zip"
            with self.assertRaisesRegex(ValueError, "SHA-256 mismatch"):
                setup_protoc.download("https://example.test/protoc.zip", destination, "0" * 64,
                                      open_url=lambda *args, **kwargs: io.BytesIO(b"truncated"),
                                      sleep=lambda _: None)
            self.assertFalse(destination.exists())

    def test_incomplete_success_response_is_retried_and_never_cached(self):
        attempts, delays = [], []
        data = b"complete pinned archive"

        def open_url(url, timeout):
            attempts.append(url)
            return io.BytesIO(b"truncated" if len(attempts) == 1 else data)

        with tempfile.TemporaryDirectory() as temp:
            destination = Path(temp) / "protoc.zip"
            setup_protoc.download("https://example.test/protoc.zip", destination,
                                  hashlib.sha256(data).hexdigest(), open_url=open_url, sleep=delays.append)
            self.assertEqual(destination.read_bytes(), data)
            self.assertEqual(list(Path(temp).iterdir()), [destination])
        self.assertEqual(len(attempts), 2)
        self.assertEqual(delays, [2])

    def test_verified_cache_needs_no_network_and_corrupt_cache_is_replaced(self):
        data = b"complete pinned archive"
        digest = hashlib.sha256(data).hexdigest()
        with tempfile.TemporaryDirectory() as temp:
            destination = Path(temp) / "protoc.zip"
            destination.write_bytes(data)
            setup_protoc.download("https://example.test/protoc.zip", destination, digest,
                                  open_url=lambda *args, **kwargs: self.fail("Verified cache must work offline"))
            destination.write_bytes(b"corrupt cache")
            setup_protoc.download("https://example.test/protoc.zip", destination, digest,
                                  open_url=lambda *args, **kwargs: io.BytesIO(data))
            self.assertEqual(destination.read_bytes(), data)

    def test_failed_replacement_cannot_reuse_corrupt_cache(self):
        with tempfile.TemporaryDirectory() as temp:
            destination = Path(temp) / "protoc.zip"
            destination.write_bytes(b"corrupt cache")
            with self.assertRaisesRegex(ValueError, "SHA-256 mismatch"):
                setup_protoc.download("https://example.test/protoc.zip", destination, "0" * 64,
                                      open_url=lambda *args, **kwargs: io.BytesIO(b"still corrupt"),
                                      sleep=lambda _: None)
            self.assertEqual(list(Path(temp).iterdir()), [])

    def test_extract_preserves_includes_and_rejects_path_escape(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            archive = root / "protoc.zip"
            with zipfile.ZipFile(archive, "w") as bundle:
                bundle.writestr("bin/protoc", b"compiler")
                bundle.writestr("include/google/protobuf/descriptor.proto", b"definitions")
            setup_protoc.extract(archive, root / "tools")
            self.assertTrue((root / "tools/include/google/protobuf/descriptor.proto").is_file())
            with zipfile.ZipFile(archive, "w") as bundle:
                bundle.writestr("../outside", b"unsafe")
            with self.assertRaisesRegex(ValueError, "Unsafe protoc archive path"):
                setup_protoc.extract(archive, root / "other")
            self.assertFalse((root / "outside").exists())


if __name__ == "__main__":
    unittest.main()
