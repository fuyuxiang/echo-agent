"""Audit every shipped and build dependency graph; never treat scan errors as clean."""
from __future__ import annotations

import argparse
import hashlib
from concurrent.futures import ThreadPoolExecutor
import json
import os
from pathlib import Path
import subprocess
import tempfile
import tomllib
import urllib.request

ROOT = Path(__file__).resolve().parent.parent


def reviewed_rust_exception(name, version, advisory, identities):
    # The advisory's <0.12.1 range also matches 0.10.4, but that release stores
    # pos in a separate u8, not inside buffer, and has no ReadBuffer. Neither
    # callback in the advisory can overwrite it. Keep this review specific to
    # the immutable crates.io artifact, with panic regression tests in CI.
    if (name, version, advisory) == ("block-buffer", "0.10.4", "GHSA-qwgh-2vcv-g2f7") and identities == {
        ("registry+https://github.com/rust-lang/crates.io-index",
         "3078c7629b62d3f0439517fa394996acacc5cbc91c5a20d8c658e77abd503a71"),
    }:
        return {
            "reason": "0.10.4 stores the cursor outside the buffer and has no ReadBuffer; callbacks cannot corrupt the cursor as described in this advisory.",
            "source": "https://docs.rs/block-buffer/0.10.4/src/block_buffer/lib.rs.html",
            "regression_test": "src-tauri/tests/block_buffer_panic_safety.rs",
        }
    return None


def request(url, payload=None):
    data = None if payload is None else json.dumps(payload).encode()
    req = urllib.request.Request(url, data=data, headers={
        "Content-Type": "application/json", "User-Agent": "echoagent-dependency-audit",
    })
    with urllib.request.urlopen(req, timeout=60) as response:
        return json.load(response)


def npm_audits(output):
    for name in ["braces", "sprintf-js", "deepmerge"]:
        source = ROOT / "vendor/theia-platform/security" / name
        provenance = json.loads((source / "SOURCE.json").read_text())
        for file, expected in provenance["patchedSha256"].items():
            if hashlib.sha256((source / file).read_bytes()).hexdigest() != expected:
                raise RuntimeError(f"Security backport changed without review: {name}/{file}")
    for lock in [ROOT / "vendor/theia-platform/package-lock.json", ROOT / "scripts/theia-runtime-package-lock.json"]:
        packages = json.loads(lock.read_text())["packages"]
        for name in ["braces", "sprintf-js", "deepmerge"]:
            if packages.get(f"node_modules/{name}", {}).get("resolved") != f"security/{name}":
                raise RuntimeError(f"Security workspace is not active: {lock}/{name}")
    jobs = [
        ("frontend", ROOT, ["pnpm.cmd" if os.name == "nt" else "pnpm", "audit", "--json"]),
        ("theia-source", ROOT / "vendor/theia-platform", ["npm.cmd" if os.name == "nt" else "npm", "audit", "--json", "--package-lock-only", "--ignore-scripts"]),
        ("theia-runtime", ROOT, ["node", "scripts/audit-theia-runtime.mjs"]),
    ]

    def run(job):
        name, cwd, command = job
        env = dict(os.environ, npm_config_fetch_retries="1", npm_config_fetch_timeout="30000")
        result = subprocess.run(command, cwd=cwd, env=env, capture_output=True, text=True, timeout=240)
        (output / f"{name}.json").write_text(result.stdout)
        (output / f"{name}.log").write_text(result.stderr)
        data = json.loads(result.stdout)
        counts = data.get("metadata", {}).get("vulnerabilities")
        if data.get("error") or not isinstance(counts, dict) or result.returncode not in (0, 1):
            raise RuntimeError(f"{name}: incomplete audit: {data.get('error', result.stderr[-500:])}")
        if not all(isinstance(counts.get(severity), int) for severity in ["info", "low", "moderate", "high", "critical"]):
            raise RuntimeError(f"{name}: incomplete vulnerability counts")
        findings = sum(counts.get(severity, 0) for severity in ["info", "low", "moderate", "high", "critical"])
        print(f"{name}: {findings} affected dependency/advisory entries", flush=True)
        for package, vulnerability in data.get("vulnerabilities", {}).items():
            for advisory in vulnerability.get("via", []):
                if isinstance(advisory, dict):
                    print(f"{name}: {package}: {advisory.get('title')}: {advisory.get('url')}", flush=True)
        return findings

    with ThreadPoolExecutor(max_workers=3) as executor:
        return sum(executor.map(run, jobs))


def rust_audit(output):
    manifests = ["scripts/desktop-native-check/Cargo.lock", "scripts/stability-check/Cargo.lock",
                 "src-tauri/Cargo.lock", "vendor/echo-agent-build/Cargo.lock",
                 "vendor/echo-agent-build/crates/codegen/echo-agent-markdown/fuzz/Cargo.lock",
                 "vendor/nucleo/Cargo.lock"]
    packages, identities = {}, {}
    for manifest in manifests:
        for pkg in tomllib.loads((ROOT / manifest).read_text())["package"]:
            if pkg.get("source", "").startswith("registry+"):
                packages.setdefault((pkg["name"], pkg["version"]), []).append(manifest)
                identities.setdefault((pkg["name"], pkg["version"]), set()).add((pkg["source"], pkg.get("checksum")))
    # Local GLib is not a registry dependency: verify its exact backport instead
    # of adding a global ignore for the advisory.
    tauri = tomllib.loads((ROOT / "src-tauri/Cargo.toml").read_text())
    if tauri.get("patch", {}).get("crates-io", {}).get("glib", {}).get("path") != "../vendor/glib":
        raise RuntimeError("The GTK3 GLib security backport is not active")
    variant = (ROOT / "vendor/glib/src/variant_iter.rs").read_text()
    if "let mut p: *mut libc::c_char" not in variant or "                &mut p," not in variant:
        raise RuntimeError("GLib VariantStrIter backport is missing")
    runtime = tomllib.loads((ROOT / "vendor/echo-agent-build/Cargo.toml").read_text())
    for name, path in [("lru-0-12", "../lru-0.12"), ("lru-0-16", "../lru-0.16")]:
        if runtime.get("patch", {}).get("crates-io", {}).get(name, {}).get("path") != path:
            raise RuntimeError(f"Runtime LRU security backport is not active: {name}")
    fuzz = tomllib.loads((ROOT / "vendor/echo-agent-build/crates/codegen/echo-agent-markdown/fuzz/Cargo.toml").read_text())
    if fuzz.get("patch", {}).get("crates-io", {}).get("lru", {}).get("path") != "../../../../../lru-0.12":
        raise RuntimeError("Markdown fuzz LRU security backport is not active")
    for directory in ["vendor/glib", "vendor/lru-0.12", "vendor/lru-0.16"]:
        source = ROOT / directory
        provenance = json.loads((source / "SOURCE.json").read_text())
        for file, expected in provenance["patchedSha256"].items():
            if hashlib.sha256((source / file).read_bytes()).hexdigest() != expected:
                raise RuntimeError(f"Security backport changed without review: {directory}/{file}")
    items = list(packages)

    def batch(offset):
        subset = items[offset:offset + 200]
        data = request("https://api.osv.dev/v1/querybatch", {"queries": [
            {"package": {"name": name, "ecosystem": "crates.io"}, "version": version}
            for name, version in subset]})
        if len(data.get("results", [])) != len(subset):
            raise RuntimeError("OSV returned an incomplete batch")
        return [(pkg, result["vulns"]) for pkg, result in zip(subset, data["results"]) if result.get("vulns")]

    with ThreadPoolExecutor(max_workers=3) as executor:
        matches = [item for group in executor.map(batch, range(0, len(items), 200)) for item in group]
    identifiers = sorted({v["id"] for _, vulns in matches for v in vulns})
    with ThreadPoolExecutor(max_workers=4) as executor:
        records = list(executor.map(lambda identifier: request("https://api.osv.dev/v1/vulns/" + identifier), identifiers))
    advisories = {record["id"]: record for record in records}
    (output / "rust-advisories.json").write_text(json.dumps(advisories, indent=2))
    (output / "rust-packages.json").write_text(json.dumps([
        {"name": name, "version": version, "manifests": locations}
        for (name, version), locations in packages.items()
    ], indent=2))
    findings, maintenance, not_affected = [], [], []
    for (name, version), vulns in matches:
        seen = set()
        for vuln in vulns:
            advisory = advisories[vuln["id"]]
            if advisory.get("withdrawn"):
                continue
            canonical = next((i for i in [advisory["id"], *advisory.get("aliases", [])] if i.startswith("RUSTSEC-")), advisory["id"])
            if canonical in seen:
                continue
            seen.add(canonical)
            row = {"package": name, "version": version, "advisory": canonical,
                   "summary": advisory.get("summary", ""), "manifests": packages[(name, version)]}
            review = reviewed_rust_exception(name, version, canonical, identities[(name, version)])
            if review:
                not_affected.append(dict(row, review=review))
                continue
            summary = row["summary"].lower()
            title = summary.replace("`", "").rstrip(".")
            # OSV's RustSec export may omit the informational field. Recognize
            # only pure maintenance titles; mixed vulnerability titles still fail.
            unmaintained = advisory.get("database_specific", {}).get("informational") == "unmaintained" or title in {
                f"{name} is unmaintained", f"{name} - no longer maintained",
                f"{name} has been discontinued", f"{name} is no longer maintained",
            }
            (maintenance if unmaintained else findings).append(row)
    (output / "rust.json").write_text(json.dumps({"security": findings, "maintenance": maintenance,
                                                "not_affected": not_affected}, indent=2))
    for row in not_affected:
        print(f"rust: reviewed not affected: {row['package']} {row['version']}: {row['advisory']}: {row['review']['reason']}", flush=True)
    for row in findings:
        print(f"rust: {row['package']} {row['version']}: {row['advisory']}: {row['summary']}", flush=True)
    print(f"rust: {len(items)} registry package versions; {len(findings)} security findings; {len(maintenance)} maintenance notices; {len(not_affected)} reviewed not affected", flush=True)
    return len(findings)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("kind", choices=["all", "npm", "rust"], nargs="?", default="all")
    parser.add_argument("--output", type=Path)
    args = parser.parse_args()
    output = args.output or Path(tempfile.mkdtemp(prefix="echo-dependency-audit-"))
    output.mkdir(parents=True, exist_ok=True)
    print(f"Audit evidence: {output}", flush=True)
    try:
        count = 0
        if args.kind in ("all", "npm"):
            count += npm_audits(output)
        if args.kind in ("all", "rust"):
            count += rust_audit(output)
        return 1 if count else 0
    except Exception as error:
        print(f"Audit incomplete: {error}", flush=True)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
