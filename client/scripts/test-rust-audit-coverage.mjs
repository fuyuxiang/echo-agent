import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { test } from "node:test";

test("dependency audit covers every tracked Rust lockfile", () => {
  const result = spawnSync("git", ["ls-files", "-z", "--", "*Cargo.lock"], { encoding: "utf8" });
  assert.equal(result.status, 0, result.error?.message || result.stderr);
  const tracked = result.stdout.split("\0").filter(Boolean).sort();
  assert.ok(tracked.length > 0);
  const source = readFileSync(new URL("./audit-dependencies.py", import.meta.url), "utf8");
  const audited = [...source.matchAll(/"([^"\n]+\/Cargo\.lock)"/g)].map(match => match[1]).sort();
  assert.deepEqual(audited, tracked);
});

test("native recovery fixture exercises the production Tauri and WebView2 versions", () => {
  const versions = (path, name) => readFileSync(path, "utf8").split("[[package]]")
    .filter(block => block.match(/^name\s*=\s*"([^"]+)"/m)?.[1] === name)
    .map(block => block.match(/^version\s*=\s*"([^"]+)"/m)?.[1]).sort();
  for (const name of ["tauri", "tauri-build", "webview2-com"]) {
    const production = versions("src-tauri/Cargo.lock", name);
    assert.ok(production.length > 0, `Production dependency missing: ${name}`);
    assert.deepEqual(versions("scripts/stability-check/Cargo.lock", name), production,
      `Native recovery fixture must exercise the production ${name} version`);
  }
  const coreAbi = path => readFileSync(path, "utf8")
    .match(/^windows-core\s*=\s*"[=~^]?(\d+\.\d+)/m)?.[1];
  const productionAbi = coreAbi("src-tauri/Cargo.toml");
  assert.ok(productionAbi, "The production recovery callback needs a direct Windows core dependency");
  assert.equal(coreAbi("scripts/stability-check/Cargo.toml"), productionAbi,
    "The shared recovery callback needs the same Windows core ABI in its fixture");
});

test("Tauri frontend API and packaging CLI match the native SDK release line", () => {
  const native = readFileSync("src-tauri/Cargo.lock", "utf8").split("[[package]]")
    .find(block => /^name\s*=\s*"tauri"$/m.test(block))
    ?.match(/^version\s*=\s*"(\d+\.\d+)\./m)?.[1];
  assert.ok(native, "The native Tauri SDK must be locked");
  const lock = readFileSync("pnpm-lock.yaml", "utf8").replaceAll("\r\n", "\n");
  for (const name of ["@tauri-apps/api", "@tauri-apps/cli"]) {
    const escaped = name.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
    const resolved = lock.match(new RegExp(`^      '${escaped}':\\n        specifier: [^\\n]+\\n        version: (\\d+\\.\\d+)\\.`, "m"))?.[1];
    assert.equal(resolved, native,
      `${name} must match tauri ${native}: mismatched minor versions prevent packaging`);
  }
});
