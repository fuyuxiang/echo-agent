"""Audit every shipped and build dependency graph; never treat scan errors as clean."""
from __future__ import annotations

import argparse
import hashlib
from concurrent.futures import ThreadPoolExecutor
import json
import os
from pathlib import Path
import re
import subprocess
import tempfile
import tomllib
import urllib.request

ROOT = Path(__file__).resolve().parent.parent
SEVERITIES = ("info", "low", "moderate", "high", "critical")
LEGACY_MINIMATCH_INTEGRITIES = {
    "3.1.5": "sha512-VgjWUsnnT6n+NUk6eZq77zeFdpW2LWDzP6zFGrCbHXiYNul5Dzqk2HHQ5uFH2DNW5Xbp8+jVzaeNt94ssEEl4w==",
    "5.1.9": "sha512-7o1wEA2RyMP7Iu7GNba9vc0RWWGACJOCZBJX2GJWip0ikV+wcOsgVuY9uE8CPiyQhkGFSlhuSkZPavN7u1c2Fw==",
}


def reviewed_npm_exception(name, version, advisory, integrities):
    # The database range <11.0.1 also includes the old callable API. These
    # exact artifacts have no firstPhasePreProcess/secondPhasePreProcess or
    # optimizationLevel option, so the reported amplification cannot occur.
    # Keep them for legacy glob/archiver consumers; modern branches are upgraded.
    expected = LEGACY_MINIMATCH_INTEGRITIES.get(version)
    if (name == "minimatch" and advisory == "GHSA-g3hv-75wg-m249"
            and expected and integrities == {expected}):
        return {
            "reason": "This legacy release has no globparts preprocessing or optimizationLevel option involved in the advisory.",
            "source": f"https://registry.npmjs.org/minimatch/-/minimatch-{version}.tgz",
            "integrity": expected,
            "regression_test": "scripts/test-dependency-patches.mjs",
        }
    return None


def npm_lock_path(name):
    return ROOT / {"frontend": "pnpm-lock.yaml", "theia-source": "vendor/theia-platform/package-lock.json",
                   "theia-runtime": "scripts/theia-runtime-package-lock.json"}[name]


def npm_lock_packages(name, text=None):
    text = npm_lock_path(name).read_text() if text is None else text
    if name != "frontend":
        return json.loads(text)["packages"]
    # Only the two legacy minimatch identities need reviewing in the pnpm
    # graph. Read the packages section, never the version-only snapshots.
    # Unsupported resolution formats provide no exemption (fail closed).
    text = text.replace("\r\n", "\n").split("\nsnapshots:\n", 1)[0]
    packages = {}
    for match in re.finditer(r"^  minimatch@([^\n:]+):\n(.*?)(?=^  \S|\Z)", text, re.M | re.S):
        version, body = match.groups()
        integrity = re.search(r"^    resolution: \{integrity: ([^,}\s]+)\}", body, re.M)
        key = f"minimatch@{version}"
        if key in packages:
            raise RuntimeError(f"Duplicate pnpm package identity: {key}")
        packages[key] = {"name": "minimatch", "version": version,
                         "integrity": integrity.group(1) if integrity else None}
    return packages


def npm_audit_report(data, packages):
    counts = data.get("metadata", {}).get("vulnerabilities")
    if data.get("error") or not isinstance(counts, dict):
        raise RuntimeError(f"incomplete audit: {data.get('error', 'missing vulnerability counts')}")
    if not all(type(counts.get(severity)) is int and counts[severity] >= 0 for severity in SEVERITIES):
        raise RuntimeError("incomplete vulnerability counts")
    if sum(key in data for key in ("advisories", "vulnerabilities")) != 1:
        raise RuntimeError("ambiguous or unrecognized audit report format")
    reported = sum(counts[severity] for severity in SEVERITIES)
    identities = {}
    for path, package in packages.items():
        name = package.get("name") or path.rsplit("node_modules/", 1)[-1]
        identities.setdefault((name, package.get("version")), set()).add(package.get("integrity"))
    findings, reviewed = [], []

    def inspect(name, version, advisory, paths):
        url = advisory.get("url", "")
        identifier = advisory.get("github_advisory_id") or url.rstrip("/").rsplit("/", 1)[-1]
        row = {"package": name, "version": version, "advisory": identifier,
               "summary": advisory.get("title", ""), "severity": advisory.get("severity"),
               "url": url, "paths": paths}
        review = reviewed_npm_exception(name, version, identifier, identities.get((name, version), set()))
        (reviewed if review else findings).append(dict(row, review=review) if review else row)
        return review is None

    if isinstance(data.get("advisories"), dict):
        # pnpm 10 uses advisories/findings; npm uses vulnerabilities/via.
        for advisory in data["advisories"].values():
            entries = advisory.get("findings")
            if not entries:
                raise RuntimeError("advisory has no affected package versions")
            for entry in entries:
                inspect(advisory["module_name"], entry["version"], advisory, entry.get("paths", []))
        affected = sorted({row["package"] + "@" + row["version"] for row in findings})
        entries_present = bool(data["advisories"])
    elif isinstance(data.get("vulnerabilities"), dict):
        vulnerabilities = data["vulnerabilities"]
        affected, edges, explained = set(), {}, set()
        for name, vulnerability in vulnerabilities.items():
            via, nodes = vulnerability.get("via"), vulnerability.get("nodes")
            if not isinstance(via, list) or not via or not isinstance(nodes, list) or not nodes:
                raise RuntimeError(f"incomplete vulnerability entry: {name}")
            edges[name] = []
            for advisory in via:
                if isinstance(advisory, str):
                    if advisory not in vulnerabilities:
                        raise RuntimeError(f"missing dependency advisory: {name} -> {advisory}")
                    edges[name].append(advisory)
                elif isinstance(advisory, dict):
                    explained.add(name)
                    for node in nodes:
                        package = packages.get(node, {})
                        if inspect(name, package.get("version", "unknown"), advisory, [node]):
                            affected.add(name)
                else:
                    raise RuntimeError(f"invalid dependency advisory: {name}")
        # Recompute meta-vulnerabilities after exact artifact reviews. A parent
        # is clear only if every advisory reachable from it has been reviewed.
        while True:
            parents = {name for name, children in edges.items() if any(child in affected for child in children)}
            if parents <= affected:
                break
            affected.update(parents)
        while True:
            parents = {name for name, children in edges.items() if any(child in explained for child in children)}
            if parents <= explained:
                break
            explained.update(parents)
        if explained != set(vulnerabilities):
            raise RuntimeError("dependency advisory graph has no root security advisory")
        affected = sorted(affected)
        entries_present = bool(vulnerabilities)
    else:
        raise RuntimeError("unrecognized audit report format")
    if bool(reported) != entries_present:
        raise RuntimeError("vulnerability counts disagree with audit entries")
    return {"reported_counts": counts, "affected_packages": affected,
            "security": findings, "not_affected": reviewed}


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
        lock_path = npm_lock_path(name)
        lock_bytes = lock_path.read_bytes()
        lock_text = lock_bytes.decode("utf-8")
        packages = npm_lock_packages(name, lock_text)
        env = dict(os.environ, npm_config_fetch_retries="1", npm_config_fetch_timeout="30000")
        result = subprocess.run(command, cwd=cwd, env=env, capture_output=True, text=True, timeout=240)
        (output / f"{name}.json").write_text(result.stdout)
        (output / f"{name}.log").write_text(result.stderr)
        if lock_path.read_bytes() != lock_bytes:
            raise RuntimeError(f"{name}: lockfile changed during the audit; rerun against a stable graph")
        data = json.loads(result.stdout)
        if result.returncode not in (0, 1):
            raise RuntimeError(f"{name}: incomplete audit: {data.get('error', result.stderr[-500:])}")
        try:
            report = npm_audit_report(data, packages)
            if result.returncode and not sum(report["reported_counts"][severity] for severity in SEVERITIES):
                raise RuntimeError("audit exited unsuccessfully without reporting vulnerabilities")
            report["lockfile"] = str(lock_path.relative_to(ROOT))
            report["lock_sha256"] = hashlib.sha256(lock_bytes).hexdigest()
        except (KeyError, TypeError, ValueError, RuntimeError) as error:
            raise RuntimeError(f"{name}: {error}") from error
        (output / f"{name}-reviewed.json").write_text(json.dumps(report, indent=2))
        print(f"{name}: {len(report['affected_packages'])} affected packages; "
              f"{len(report['not_affected'])} reviewed artifact/advisory entries not affected", flush=True)
        for row in report["security"]:
            print(f"{name}: {row['package']} {row['version']}: {row['summary']}: {row['url']}; "
                  f"dependency paths: {', '.join(row['paths'])}", flush=True)
        for row in report["not_affected"]:
            print(f"{name}: reviewed not affected: {row['package']} {row['version']}: "
                  f"{row['advisory']}: {row['review']['reason']}", flush=True)
        return len(report["affected_packages"])

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


def write_summary(output, error=None):
    lines = ["## Dependency security", "", "| Graph | Affected packages | Reviewed not affected |",
             "| --- | ---: | ---: |"]
    for name in ("frontend", "theia-source", "theia-runtime"):
        path = output / f"{name}-reviewed.json"
        if path.exists():
            report = json.loads(path.read_text())
            lines.append(f"| {name} | {len(report['affected_packages'])} | {len(report['not_affected'])} |")
        else:
            lines.append(f"| {name} | Not scanned | — |")
    path = output / "rust.json"
    if path.exists():
        report = json.loads(path.read_text())
        lines.append(f"| Rust | {len(report['security'])} | {len(report['not_affected'])} |")
        lines.extend(["", f"Rust maintenance notices: {len(report['maintenance'])} (reported separately)."])
    else:
        lines.append("| Rust | Not scanned | — |")
    if error:
        lines.extend(["", f"Audit incomplete: {error}"])
    lines.extend(["", "Raw scanner output and exact artifact reviews are attached in the dependency-security artifact.", ""])
    summary = "\n".join(lines)
    (output / "summary.md").write_text(summary)
    if os.environ.get("GITHUB_STEP_SUMMARY"):
        with open(os.environ["GITHUB_STEP_SUMMARY"], "a") as stream:
            stream.write(summary)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("kind", choices=["all", "npm", "rust"], nargs="?", default="all")
    parser.add_argument("--output", type=Path)
    args = parser.parse_args()
    output = args.output or Path(tempfile.mkdtemp(prefix="echo-dependency-audit-"))
    output.mkdir(parents=True, exist_ok=True)
    # An interrupted rerun must not present a previous successful scan as new
    # evidence. Only remove files owned by this audit, preserving other files.
    evidence = [f"{name}{suffix}" for name in ("frontend", "theia-source", "theia-runtime")
                for suffix in (".json", ".log", "-reviewed.json")]
    for name in [*evidence, "rust.json", "rust-advisories.json", "rust-packages.json", "summary.md"]:
        (output / name).unlink(missing_ok=True)
    print(f"Audit evidence: {output}", flush=True)
    error = None
    try:
        count = 0
        if args.kind in ("all", "npm"):
            count += npm_audits(output)
        if args.kind in ("all", "rust"):
            count += rust_audit(output)
        return 1 if count else 0
    except Exception as exception:
        error = str(exception)
        print(f"Audit incomplete: {error}", flush=True)
        return 2
    finally:
        write_summary(output, error)


if __name__ == "__main__":
    raise SystemExit(main())
