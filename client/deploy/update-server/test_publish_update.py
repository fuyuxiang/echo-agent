from __future__ import annotations

import importlib.util
from pathlib import Path
from contextlib import redirect_stderr, redirect_stdout
from io import StringIO
import json
import sys
import tempfile
import unittest
from unittest import mock


MODULE_PATH = Path(__file__).with_name("publish-update.py")
SPEC = importlib.util.spec_from_file_location("echoagent_publish_update", MODULE_PATH)
assert SPEC and SPEC.loader
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


class SemverTests(unittest.TestCase):
    def test_stable_sorts_after_prerelease(self) -> None:
        self.assertLess(MODULE.semver_key("1.2.3-rc.1"), MODULE.semver_key("1.2.3"))

    def test_numeric_prerelease_sorts_before_text(self) -> None:
        self.assertLess(MODULE.semver_key("1.2.3-1"), MODULE.semver_key("1.2.3-alpha"))

    def test_invalid_leading_zero_is_rejected(self) -> None:
        for version in ("01.2.3", "1.2.3-01", "1.2.3-alpha..1", "1.2.3+meta..1"):
            with self.subTest(version=version), self.assertRaises(ValueError):
                MODULE.semver_key(version)

    def test_core_version_rejects_non_ascii_digits(self) -> None:
        for version in ("1۲.2.3", "1.2۲.3", "1.2.3۲"):
            with self.subTest(version=version), self.assertRaises(ValueError):
                MODULE.semver_key(version)


class PublishingTests(unittest.TestCase):
    def initialize_root(self, root: Path) -> Path:
        stable = root / "updates" / "stable"
        initial = stable / "generations" / "initial"
        initial.mkdir(parents=True)
        (stable / "current").symlink_to("generations/initial")
        return stable

    def invoke(self, arguments: list[str]) -> int:
        output = StringIO()
        with mock.patch.object(sys, "argv", [str(MODULE_PATH), *arguments]), redirect_stdout(output), redirect_stderr(output):
            return MODULE.main()

    def test_same_version_cannot_be_replaced_with_different_bytes(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            stable = self.initialize_root(root)
            artifact = root / "EchoAgent-v1.2.3-windows-x86_64-setup.exe"
            signature = root / f"{artifact.name}.sig"
            artifact.write_bytes(b"first")
            signature.write_text("A" * 100, encoding="utf-8")
            arguments = [
                "--version",
                "1.2.3",
                "--target",
                "windows-x86_64",
                "--artifact",
                str(artifact),
                "--signature",
                str(signature),
                "--root",
                str(root / "updates"),
            ]

            self.assertEqual(self.invoke(arguments), 0)
            manifest_path = stable / "current" / "windows-x86_64.json"
            original = json.loads(manifest_path.read_text(encoding="utf-8"))

            artifact.write_bytes(b"second")
            self.assertEqual(self.invoke(arguments), 2)
            current = json.loads(manifest_path.read_text(encoding="utf-8"))
            self.assertEqual(current["sha256"], original["sha256"])

            artifact.write_bytes(b"first")
            signature.write_text("B" * 100, encoding="utf-8")
            self.assertEqual(self.invoke(arguments), 2)
            current = json.loads(manifest_path.read_text(encoding="utf-8"))
            self.assertEqual(current["signature"], original["signature"])

    def test_signature_change_after_read_refuses_to_switch_manifest(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            stable = self.initialize_root(root)
            target = "windows-x86_64"
            artifact = root / MODULE.artifact_name("1.2.3", target)
            signature = root / f"{artifact.name}.sig"
            artifact.write_bytes(b"signed fixture")
            signature.write_text("A" * 100, encoding="utf-8")
            read_signature = MODULE.read_signature

            def change_signature_after_read(path: Path):
                original = read_signature(path)
                path.write_text("B" * 100, encoding="utf-8")
                return original

            with mock.patch.object(MODULE, "read_signature", side_effect=change_signature_after_read):
                self.assertEqual(self.invoke([
                    "--version", "1.2.3", "--target", target,
                    "--artifact", str(artifact), "--signature", str(signature),
                    "--root", str(root / "updates"),
                ]), 2)
            self.assertEqual((stable / "current").readlink(), Path("generations/initial"))
            self.assertFalse((stable / "current" / f"{target}.json").exists())

    def test_batch_switches_all_targets_at_once_and_failure_keeps_old_generation(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            stable = self.initialize_root(root)
            batch = root / "batch"
            batch.mkdir()

            def write_artifacts(version: str) -> None:
                for target in MODULE.TARGETS:
                    name = MODULE.artifact_name(version, target)
                    (batch / name).write_bytes(f"{version}/{target}".encode())
                    (batch / f"{name}.sig").write_text("A" * 100, encoding="utf-8")

            def publish(version: str) -> int:
                return self.invoke([
                    "--version", version, "--batch-dir", str(batch),
                    "--root", str(root / "updates"),
                ])

            write_artifacts("1.2.3")
            self.assertEqual(publish("1.2.3"), 0)
            first_generation = (stable / "current").readlink()
            for target in MODULE.TARGETS:
                manifest = json.loads((stable / "current" / f"{target}.json").read_text())
                self.assertEqual(manifest["version"], "1.2.3")

            write_artifacts("1.2.4")
            real_copy = MODULE.atomic_copy
            copy_count = 0

            def fail_second_copy(*args, **kwargs):
                nonlocal copy_count
                copy_count += 1
                if copy_count == 2:
                    raise OSError("simulated upload failure")
                return real_copy(*args, **kwargs)

            with mock.patch.object(MODULE, "atomic_copy", side_effect=fail_second_copy):
                self.assertEqual(publish("1.2.4"), 2)
            self.assertEqual((stable / "current").readlink(), first_generation)
            self.assertTrue(all(
                json.loads((stable / "current" / f"{target}.json").read_text())["version"] == "1.2.3"
                for target in MODULE.TARGETS
            ))

            self.assertEqual(publish("1.2.4"), 0)
            self.assertNotEqual((stable / "current").readlink(), first_generation)
            self.assertTrue(all(
                json.loads((stable / "current" / f"{target}.json").read_text())["version"] == "1.2.4"
                for target in MODULE.TARGETS
            ))


if __name__ == "__main__":
    unittest.main()
