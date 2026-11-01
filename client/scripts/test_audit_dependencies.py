"""Keep reviewed audit exceptions exact and visible in the saved evidence."""
import importlib.util
from contextlib import redirect_stdout
import io
import json
from pathlib import Path
import tempfile
import subprocess
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location("audit_dependencies", Path(__file__).with_name("audit-dependencies.py"))
audit = importlib.util.module_from_spec(spec)
spec.loader.exec_module(audit)

ADVISORY = "GHSA-qwgh-2vcv-g2f7"
IDENTITY = ("registry+https://github.com/rust-lang/crates.io-index",
            "3078c7629b62d3f0439517fa394996acacc5cbc91c5a20d8c658e77abd503a71")
MINIMATCH_ADVISORY = "GHSA-g3hv-75wg-m249"


def counts(total):
    return {"info": 0, "low": 0, "moderate": total, "high": 0, "critical": 0}


def npm_fixture(version="3.1.5", identifier=MINIMATCH_ADVISORY):
    advisory = {"title": "globparts amplification", "url": "https://github.com/advisories/" + identifier,
                "severity": "moderate"}
    data = {"metadata": {"vulnerabilities": counts(3)}, "vulnerabilities": {
        "minimatch": {"via": [advisory], "nodes": ["node_modules/minimatch"]},
        "glob": {"via": ["minimatch"], "nodes": ["node_modules/glob"]},
        "archiver": {"via": ["glob"], "nodes": ["node_modules/archiver"]},
    }}
    packages = {"node_modules/minimatch": {"version": version,
                "integrity": audit.LEGACY_MINIMATCH_INTEGRITIES.get(version)}}
    return data, packages


class NpmAuditReviewTests(unittest.TestCase):
    def test_only_the_exact_advisory_and_official_legacy_artifacts_are_reviewed(self):
        for version, integrity in audit.LEGACY_MINIMATCH_INTEGRITIES.items():
            review = audit.reviewed_npm_exception("minimatch", version, MINIMATCH_ADVISORY, {integrity})
            self.assertEqual(review["integrity"], integrity)
            for identities in [set(), {None}, {"changed"}, {integrity, "changed"}]:
                self.assertIsNone(audit.reviewed_npm_exception("minimatch", version, MINIMATCH_ADVISORY, identities))
            self.assertIsNone(audit.reviewed_npm_exception("minimatch", version, "GHSA-another", {integrity}))
            self.assertIsNone(audit.reviewed_npm_exception("another-package", version, MINIMATCH_ADVISORY, {integrity}))
        for version in ["3.1.4", "5.1.8", "9.0.9", "10.2.6", "11.0.0"]:
            self.assertIsNone(audit.reviewed_npm_exception("minimatch", version, MINIMATCH_ADVISORY, {"any"}))

    def test_pnpm_reports_advisory_versions_and_dependency_paths(self):
        data, packages = npm_fixture()
        advisory = data["vulnerabilities"]["minimatch"]["via"][0]
        advisory.update(module_name="minimatch", github_advisory_id=MINIMATCH_ADVISORY,
                        findings=[{"version": "3.1.5", "paths": [".>exceljs>archiver>glob>minimatch"]}])
        data = {"metadata": {"vulnerabilities": counts(1)}, "advisories": {"123": advisory}}
        report = audit.npm_audit_report(data, packages)
        self.assertEqual(report["affected_packages"], [])
        self.assertEqual(report["not_affected"][0]["paths"], advisory["findings"][0]["paths"])
        advisory["github_advisory_id"] = "GHSA-new-finding"
        report = audit.npm_audit_report(data, packages)
        self.assertEqual(report["affected_packages"], ["minimatch@3.1.5"])
        self.assertEqual(report["security"][0]["advisory"], "GHSA-new-finding")

    def test_npm_recomputes_parent_packages_and_preserves_review_evidence(self):
        data, packages = npm_fixture()
        report = audit.npm_audit_report(data, packages)
        self.assertEqual(report["affected_packages"], [])
        self.assertEqual(len(report["not_affected"]), 1)
        self.assertEqual(report["reported_counts"], counts(3))
        packages["node_modules/minimatch"]["integrity"] = "changed"
        report = audit.npm_audit_report(data, packages)
        self.assertEqual(report["affected_packages"], ["archiver", "glob", "minimatch"])
        self.assertEqual(report["not_affected"], [])

    def test_modern_or_mixed_artifacts_and_other_advisories_remain_failures(self):
        for version in ["9.0.9", "10.2.6"]:
            data, packages = npm_fixture(version)
            self.assertEqual(len(audit.npm_audit_report(data, packages)["affected_packages"]), 3)
        data, packages = npm_fixture(identifier="GHSA-new-finding")
        self.assertEqual(len(audit.npm_audit_report(data, packages)["security"]), 1)
        data, packages = npm_fixture()
        packages["node_modules/consumer/node_modules/minimatch"] = {"version": "3.1.5", "integrity": "changed"}
        self.assertEqual(len(audit.npm_audit_report(data, packages)["affected_packages"]), 3)
        data, packages = npm_fixture()
        data["vulnerabilities"]["glob"]["via"].append({
            "title": "another issue", "url": "https://github.com/advisories/GHSA-new-finding"})
        report = audit.npm_audit_report(data, packages)
        self.assertEqual(report["affected_packages"], ["archiver", "glob"])
        self.assertEqual(report["security"][0]["advisory"], "GHSA-new-finding")

    def test_incomplete_or_inconsistent_scans_never_pass(self):
        examples = [{}, {"error": "registry unavailable"},
                    {"metadata": {"vulnerabilities": counts(0)}},
                    {"metadata": {"vulnerabilities": counts(1)}, "vulnerabilities": {}},
                    {"metadata": {"vulnerabilities": counts(0)}, "advisories": {}, "vulnerabilities": {"unexpected": {}}},
                    {"metadata": {"vulnerabilities": counts(True)}, "vulnerabilities": {}},
                    {"metadata": {"vulnerabilities": counts(-1)}, "vulnerabilities": {}}]
        data, _ = npm_fixture()
        data["vulnerabilities"]["glob"]["via"] = ["missing"]
        examples.append(data)
        examples.append({"metadata": {"vulnerabilities": counts(1)}, "vulnerabilities": {
            "cycle": {"via": ["cycle"], "nodes": ["node_modules/cycle"]}}})
        for data in examples:
            with self.subTest(data=data), self.assertRaises(RuntimeError):
                audit.npm_audit_report(data, {})
        report = audit.npm_audit_report({"metadata": {"vulnerabilities": counts(0)}, "vulnerabilities": {}}, {})
        self.assertEqual(report["affected_packages"], [])

    def test_pnpm_reads_integrity_from_packages_and_never_from_snapshots(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            integrity = audit.LEGACY_MINIMATCH_INTEGRITIES["3.1.5"]
            path = root / "pnpm-lock.yaml"
            path.write_text(f"packages:\n  minimatch@3.1.5:\n    resolution: {{integrity: {integrity}}}\n\n"
                            "snapshots:\n  minimatch@3.1.5:\n    dependencies: {}\n")
            with patch.object(audit, "ROOT", root):
                packages = audit.npm_lock_packages("frontend")
                self.assertEqual(packages["minimatch@3.1.5"]["integrity"], integrity)
                self.assertEqual(audit.npm_lock_packages("frontend", path.read_text().replace("\n", "\r\n")), packages)
                path.write_text(path.read_text().replace(f"integrity: {integrity}", "tarball: https://example.com/other.tgz"))
                self.assertIsNone(audit.npm_lock_packages("frontend")["minimatch@3.1.5"]["integrity"])

    def test_incomplete_audit_exits_with_error_and_writes_a_summary(self):
        with tempfile.TemporaryDirectory() as directory, patch("sys.argv", ["audit-dependencies.py", "--output", directory]), \
                patch.object(audit, "npm_audits", side_effect=RuntimeError("registry unavailable")), redirect_stdout(io.StringIO()):
            self.assertEqual(audit.main(), 2)
            summary = (Path(directory) / "summary.md").read_text()
            self.assertIn("Audit incomplete: registry unavailable", summary)
            self.assertIn("Not scanned", summary)

    def test_a_scan_cannot_review_a_different_lockfile_than_the_one_scanned(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            paths = {name: root / (name + ".lock") for name in ("frontend", "theia-source", "theia-runtime")}
            for path in paths.values():
                path.write_text('{"packages":{}}')

            def run(command, **_kwargs):
                name = "frontend" if command[0].startswith("pnpm") else "theia-source" if command[0].startswith("npm") else "theia-runtime"
                paths[name].write_text("changed")
                return subprocess.CompletedProcess(command, 0, json.dumps({
                    "metadata": {"vulnerabilities": counts(0)}, "vulnerabilities": {}}), "")

            with patch.object(audit, "npm_lock_path", side_effect=paths.__getitem__), \
                    patch.object(audit.subprocess, "run", side_effect=run), self.assertRaisesRegex(RuntimeError, "lockfile changed"):
                audit.npm_audits(root)

    def test_failed_reruns_do_not_reuse_previous_success_evidence(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "frontend-reviewed.json").write_text(json.dumps({"affected_packages": [], "not_affected": []}))
            (root / "rust.json").write_text(json.dumps({"security": [], "not_affected": [], "maintenance": []}))
            (root / "keep.txt").write_text("unrelated")
            with patch("sys.argv", ["audit-dependencies.py", "--output", directory]), \
                    patch.object(audit, "npm_audits", side_effect=RuntimeError("registry unavailable")), redirect_stdout(io.StringIO()):
                self.assertEqual(audit.main(), 2)
            self.assertFalse((root / "frontend-reviewed.json").exists())
            self.assertFalse((root / "rust.json").exists())
            self.assertEqual((root / "keep.txt").read_text(), "unrelated")


class RustAuditReviewTests(unittest.TestCase):
    def test_review_is_specific_to_the_verified_release_and_advisory(self):
        review = audit.reviewed_rust_exception("block-buffer", "0.10.4", ADVISORY, {IDENTITY})
        self.assertIn("outside the buffer", review["reason"])
        self.assertTrue((audit.ROOT / review["regression_test"]).is_file())
        for name, version, advisory in [
            ("block-buffer", "0.12.0", ADVISORY),
            ("block-buffer", "0.10.3", ADVISORY),
            ("block-buffer", "0.10.4", "GHSA-another-advisory"),
            ("another-package", "0.10.4", ADVISORY),
        ]:
            with self.subTest(name=name, version=version, advisory=advisory):
                self.assertIsNone(audit.reviewed_rust_exception(name, version, advisory, {IDENTITY}))

    def test_unverified_artifacts_are_never_exempted(self):
        for identities in [set(), {(IDENTITY[0], None)}, {(IDENTITY[0], "changed")},
                           {("registry+https://example.com/index", IDENTITY[1])},
                           {IDENTITY, (IDENTITY[0], "changed")}]:
            with self.subTest(identities=identities):
                self.assertIsNone(audit.reviewed_rust_exception("block-buffer", "0.10.4", ADVISORY, identities))

    def test_report_retains_review_and_other_security_findings(self):
        other = "GHSA-test-unreviewed"
        records = {
            ADVISORY: {"id": ADVISORY, "summary": "panic corrupts inline buffer position"},
            other: {"id": other, "summary": "A separate security finding"},
        }

        def request(url, payload=None):
            if payload is None:
                return records[url.rsplit("/", 1)[-1]]
            return {"results": [
                {"vulns": [{"id": ADVISORY}, {"id": other}]}
                if query["package"]["name"] == "block-buffer" and query["version"] == "0.10.4" else {}
                for query in payload["queries"]
            ]}

        with tempfile.TemporaryDirectory() as directory, patch.object(audit, "request", side_effect=request):
            output = Path(directory)
            self.assertEqual(audit.rust_audit(output), 1)
            report = json.loads((output / "rust.json").read_text())
            self.assertEqual([row["advisory"] for row in report["not_affected"]], [ADVISORY])
            self.assertEqual([row["advisory"] for row in report["security"]], [other])
            self.assertEqual(report["maintenance"], [])
            self.assertIn("src-tauri/Cargo.lock", report["not_affected"][0]["manifests"])
            self.assertEqual(set(json.loads((output / "rust-advisories.json").read_text())), set(records))


if __name__ == "__main__":
    unittest.main()
