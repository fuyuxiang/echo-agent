import io
from pathlib import Path
import tarfile
import tempfile
import unittest

from extract_meeting_speech import extract


class SpeechArchiveTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.archive = self.root / "archive.tar.bz2"
        self.output = self.root / "output"
        self.output.mkdir()

    def write_archive(self, files):
        with tarfile.open(self.archive, "w:bz2") as stream:
            for name, content in files:
                member = tarfile.TarInfo(name)
                member.size = len(content)
                stream.addfile(member, io.BytesIO(content))

    def test_streams_real_bzip2_members_and_preserves_bytes(self):
        data = bytes(range(256)) * 8192
        self.write_archive([("runtime/lib/core.lib", data), ("runtime/LICENSE", b"license")])
        self.assertEqual(extract(self.archive, self.output), 2)
        self.assertEqual((self.output / "runtime/lib/core.lib").read_bytes(), data)

    def test_extracts_only_requested_model_members_and_requires_them(self):
        self.write_archive([("models/model.onnx", b"model"), ("models/unneeded.txt", b"extra")])
        self.assertEqual(extract(self.archive, self.output, ["models/model.onnx"]), 1)
        self.assertFalse((self.output / "models/unneeded.txt").exists())
        with self.assertRaisesRegex(ValueError, "Missing archive members"):
            extract(self.archive, self.output, ["models/missing.onnx"])

    def test_rejects_traversal_absolute_drive_and_alternate_stream_paths(self):
        for name in ["../outside", "/absolute", "D:/outside", "safe/../../outside", "safe:stream", "..\\outside"]:
            with self.subTest(name=name):
                self.write_archive([(name, b"unsafe")])
                with self.assertRaisesRegex(ValueError, "Unsafe archive member"):
                    extract(self.archive, self.output)
        self.assertFalse((self.root / "outside").exists())

    def test_rejects_links_and_special_files(self):
        for kind in [tarfile.SYMTYPE, tarfile.LNKTYPE, tarfile.FIFOTYPE]:
            with self.subTest(kind=kind):
                with tarfile.open(self.archive, "w:bz2") as stream:
                    member = tarfile.TarInfo("link")
                    member.type = kind
                    member.linkname = "../outside"
                    stream.addfile(member)
                with self.assertRaisesRegex(ValueError, "Unsupported archive member type"):
                    extract(self.archive, self.output)

    def test_refuses_overwriting_an_existing_file(self):
        self.write_archive([("model.onnx", b"replacement")])
        previous = self.output / "model.onnx"
        previous.write_bytes(b"original")
        with self.assertRaises(FileExistsError):
            extract(self.archive, self.output)
        self.assertEqual(previous.read_bytes(), b"original")

    def test_refuses_extracting_through_a_preexisting_symbolic_directory(self):
        outside = self.root / "outside"
        outside.mkdir()
        (self.output / "runtime").symlink_to(outside, target_is_directory=True)
        self.write_archive([("runtime/core.lib", b"unsafe")])
        with self.assertRaisesRegex(ValueError, "Symbolic directory"):
            extract(self.archive, self.output)
        self.assertEqual(list(outside.iterdir()), [])

    def test_refuses_duplicate_members_and_empty_archives(self):
        self.write_archive([("model", b"first"), ("model", b"second")])
        with self.assertRaisesRegex(ValueError, "Duplicate archive member"):
            extract(self.archive, self.output)
        self.write_archive([])
        with self.assertRaisesRegex(ValueError, "no requested regular files"):
            extract(self.archive, self.output)


if __name__ == "__main__":
    unittest.main()
