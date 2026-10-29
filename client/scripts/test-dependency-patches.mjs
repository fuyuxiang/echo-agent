import assert from "node:assert/strict";
import { createRequire } from "node:module";
import { cpSync, existsSync, mkdirSync, mkdtempSync, readFileSync, realpathSync, writeFileSync, rmSync } from "node:fs";
import { spawnSync } from "node:child_process";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { test } from "node:test";
import { createTheiaRuntimeManifest } from "./theia-runtime-manifest.mjs";
import { assertSupportedNodeVersion, assertSupportedNpmVersion } from "./node-runtime-version.mjs";

const require = createRequire(import.meta.url);
const braces = require(resolve("vendor/theia-platform/security/braces"));
const { sprintf } = require(resolve("vendor/theia-platform/security/sprintf-js"));
const deepmerge = require(resolve("vendor/theia-platform/security/deepmerge"));

test("deepmerge cannot poison results through either operand or null-prototype objects", () => {
  const poison = JSON.parse('{"__proto__":{"poisoned":true},"constructor":{"prototype":{"poisoned":true}},"prototype":{"poisoned":true},"safe":{"value":1}}');
  for (const input of [poison, Object.assign(Object.create(null), poison), { nested: poison }]) {
    for (const result of [deepmerge(input, {}), deepmerge({}, input), deepmerge.all([input, {}])]) {
      const object = result.nested ?? result;
      assert.equal(Object.getPrototypeOf(object), Object.prototype);
      assert.equal(object.poisoned, undefined);
      for (const key of ["__proto__", "constructor", "prototype"]) assert.equal(Object.hasOwn(object, key), false);
      assert.deepEqual(object.safe, { value: 1 });
    }
  }
  assert.equal({}.poisoned, undefined);
});

test("deepmerge preserves config merging, arrays, symbols and custom merge hooks", () => {
  const symbol = Symbol("config");
  const target = { nested: { left: true }, values: [1], [symbol]: { first: 1 } };
  const source = { nested: { right: true }, values: [2], [symbol]: { second: 2 } };
  assert.deepEqual(deepmerge(target, source), { nested: { left: true, right: true }, values: [1, 2], [symbol]: { first: 1, second: 2 } });
  assert.deepEqual(deepmerge(target, source, { arrayMerge: (_, value) => value }).values, [2]);
  assert.deepEqual(deepmerge(target, source, { customMerge: key => key === "nested" ? () => ({ custom: true }) : undefined }).nested, { custom: true });
  assert.deepEqual(target.nested, { left: true });
  assert.equal(deepmerge({ toString: "config value" }, {}).toString, "config value");
});

test("npm ci installs the reviewed deepmerge workspace for a registry version requirement", () => {
  const directory = mkdtempSync(join(tmpdir(), "echo-deepmerge-install-"));
  try {
    cpSync("vendor/theia-platform/security/deepmerge", join(directory, "security/deepmerge"), { recursive: true });
    writeFileSync(join(directory, "package.json"), JSON.stringify({ private: true, workspaces: ["security/*"], dependencies: { deepmerge: "^4.3.1" } }));
    const npm = process.platform === "win32" ? "npm.cmd" : "npm";
    for (const args of [["install", "--package-lock-only"], ["ci"]]) {
      const result = spawnSync(npm, [...args, "--ignore-scripts", "--offline", "--no-audit", "--no-fund"], {
        cwd: directory, encoding: "utf8", timeout: 20000, shell: process.platform === "win32",
        env: { ...process.env, npm_config_cache: join(directory, "cache") },
      });
      assert.equal(result.status, 0, result.error?.message || result.stderr);
    }
    assert.equal(realpathSync(join(directory, "node_modules/deepmerge")), realpathSync(join(directory, "security/deepmerge")));
    const installed = require(join(directory, "node_modules/deepmerge"));
    assert.equal(installed(JSON.parse('{"__proto__":{"poisoned":true}}'), {}).poisoned, undefined);
    for (const path of ["vendor/theia-platform/package-lock.json", "scripts/theia-runtime-package-lock.json"]) {
      const lock = JSON.parse(readFileSync(path, "utf8"));
      assert.deepEqual(lock.packages["node_modules/deepmerge"], { resolved: "security/deepmerge", link: true });
    }
  } finally { rmSync(directory, { recursive: true, force: true }); }
});

test("dependency audit covers every tracked Rust lockfile, including the native harness", () => {
  const result = spawnSync("git", ["ls-files", "-z", "--", "*Cargo.lock"], { encoding: "utf8" });
  assert.equal(result.status, 0, result.stderr);
  const tracked = result.stdout.split("\0").filter(Boolean).sort();
  assert.ok(tracked.length > 0);
  const source = readFileSync("scripts/audit-dependencies.py", "utf8");
  const audited = [...source.matchAll(/"([^"\n]+\/Cargo\.lock)"/g)].map(match => match[1]).sort();
  assert.deepEqual(audited, tracked);
});

test("both IDE body parsers survive late compressed tails, drain requests and preserve other consumers", () => {
  const theiaRequire = createRequire(resolve("vendor/theia-platform/package.json"));
  const sdkRequire = createRequire(theiaRequire.resolve("@modelcontextprotocol/sdk/server/streamableHttp.js"));
  const implementations = [theiaRequire.resolve("body-parser"), sdkRequire.resolve("body-parser")];
  assert.equal(new Set(implementations).size, 2, "both installed parser implementations must be exercised");
  for (const [index, implementation] of implementations.entries()) {
    const version = createRequire(implementation)("./package.json").version;
    assert.equal(Number(version.split(".")[0]), index + 1, `unexpected parser API: ${version}`);
    const result = spawnSync(process.execPath, ["scripts/fixtures/body-parser-recovery.mjs", implementation], {
      encoding: "utf8", timeout: 20_000,
    });
    assert.equal(result.status, 0, result.error?.message || result.stderr || `parser process failed: ${result.signal}`);
    assert.deepEqual(JSON.parse(result.stdout), { normalJson: true, validGzip: true, invalidGzipRejected: true,
      trailingGzip: true, lateDeflate: true, otherPipePreserved: true, subsequentRequests: true });
  }
});

test("normal brace globs keep expansion and compilation behavior", () => {
  assert.deepEqual(braces.expand("src/{main,test}.{ts,tsx}"), ["src/main.ts", "src/main.tsx", "src/test.ts", "src/test.tsx"]);
  assert.equal(braces.compile("src/{main,test}.ts"), "src/(main|test).ts");
});

test("deep brace patterns fail through a controlled depth guard before recursive walking", () => {
  for (const [open, close] of [["{", "}"], ["(", ")"], ["({", "})"]]) {
    const input = open.repeat(2000) + "a,b" + close.repeat(2000);
    for (const operation of [braces.parse, braces.compile, braces.expand, braces.stringify]) {
      assert.throws(() => operation(input), { name: "SyntaxError", message: /nesting exceeds/ });
    }
  }
});

test("caller-created brace ASTs cannot bypass the depth guard", () => {
  let ast = { type: "text", value: "x" };
  for (let index = 0; index < 1000; index++) ast = { type: "brace", nodes: [ast] };
  for (const operation of [braces.compile, braces.expand, braces.stringify]) {
    assert.throws(() => operation(ast), { name: "SyntaxError", message: /nesting exceeds/ });
  }
  const cycle = { type: "brace", nodes: [] };
  cycle.nodes.push(cycle);
  assert.throws(() => braces.stringify(cycle), /nesting exceeds/);
});

test("normal CLI number, name and padding formats remain intact", () => {
  assert.equal(sprintf("%.2f %04d %s", 1.234, 7, "ready"), "1.23 0007 ready");
  assert.equal(sprintf("%.0f", 1.234), "1");
  assert.equal(sprintf("%(name)s", { name: "中文" }), "中文");
});

test("bundled Node versions must meet the security toolchain minimum", () => {
  for (const version of ["22.23.4", "22.24.0", "24.22.1"]) assert.doesNotThrow(() => assertSupportedNodeVersion(version));
  for (const version of ["20.19.0", "22.14.0", "22.23.3", "24.17.0", "24.22.0", "25.0.0"]) assert.throws(() => assertSupportedNodeVersion(version), /requires Node.js/);
});

test("npm must support reproducible workspace installs with overrides", () => {
  assert.throws(() => assertSupportedNpmVersion("10.9.2"), /requires npm/);
  assert.throws(() => assertSupportedNpmVersion("11.21.0"), /requires npm/);
  assert.doesNotThrow(() => assertSupportedNpmVersion("11.21.1"));
});

test("untrusted precision cannot trigger a native number-format RangeError", () => {
  for (const conversion of ["e", "f", "g"]) {
    for (const precision of ["0", "101", "9999999999999999999999999999999999999999"]) {
      const result = sprintf(`%.${precision}${conversion}`, 1.234);
      assert.ok(result.length <= 110);
    }
  }
  assert.ok(sprintf("%999999999999999999999s", "x").length <= 1000000);
});

test("source overrides reach the packaged runtime without build scripts or dev dependencies", () => {
  const manifest = createTheiaRuntimeManifest({ dependencies: { core: "1" }, scripts: { postinstall: "build" }, devDependencies: { test: "1" } }, { overrides: { dompurify: "^3.4.16" } });
  assert.deepEqual(manifest.overrides, { dompurify: "^3.4.16" });
  assert.deepEqual(manifest.dependencies, { core: "1" });
  assert.deepEqual(manifest.scripts, {});
  assert.deepEqual(manifest.devDependencies, {});
});

test("packaged runtime retains both allowed and denied dependency installation policies", () => {
  const source = { allowScripts: { "node-pty": true, drivelist: true, "@google/genai": false } };
  const manifest = createTheiaRuntimeManifest({ allowScripts: { drivelist: false } }, source);
  assert.deepEqual(manifest.allowScripts, { "node-pty": true, drivelist: false, "@google/genai": false });
  assert.equal(source.allowScripts.drivelist, true);
});

test("npm ci runs approved native setup and blocks explicitly denied scripts in the staged manifest", { timeout: 60000 }, () => {
  const directory = mkdtempSync(join(tmpdir(), "echo-runtime-install-policy-"));
  const npm = process.platform === "win32" ? "npm.cmd" : "npm";
  const run = (args, cwd) => {
    const result = spawnSync(npm, args, {
      cwd, encoding: "utf8", timeout: 20000, shell: process.platform === "win32",
      env: { ...process.env, npm_config_cache: join(directory, "cache"), ECHO_INSTALL_POLICY_FIXTURE: directory },
    });
    assert.equal(result.status, 0, result.error?.message || result.stderr);
    return result.stdout;
  };
  try {
    assertSupportedNpmVersion(run(["--version"], directory));
    for (const name of ["echo-policy-allowed", "echo-policy-denied"]) {
      const pkg = join(directory, name);
      mkdirSync(pkg);
      writeFileSync(join(pkg, "package.json"), JSON.stringify({ name, version: "1.0.0", scripts: { install: "node install.cjs" } }));
      writeFileSync(join(pkg, "install.cjs"), `require('node:fs').writeFileSync(require('node:path').join(process.env.ECHO_INSTALL_POLICY_FIXTURE, ${JSON.stringify(name + ".ran")}), 'installed');`);
      run(["pack", "--ignore-scripts", "--json", "--pack-destination", ".."], pkg);
    }
    const runtime = join(directory, "runtime");
    mkdirSync(runtime);
    // npm identifies file dependencies by their saved source spec, rather
    // than trusting the name inside a tarball's package.json.
    writeFileSync(join(runtime, "package.json"), JSON.stringify(createTheiaRuntimeManifest({ dependencies: {
      "echo-policy-allowed": "file:../echo-policy-allowed-1.0.0.tgz",
      "echo-policy-denied": "file:../echo-policy-denied-1.0.0.tgz",
    } }, { allowScripts: { "file:../echo-policy-allowed-1.0.0.tgz": true, "file:../echo-policy-denied-1.0.0.tgz": false } })));
    run(["install", "--package-lock-only", "--ignore-scripts", "--offline", "--no-audit", "--no-fund"], runtime);
    run(["ci", "--omit=dev", "--offline", "--no-audit", "--no-fund"], runtime);
    assert.equal(existsSync(join(directory, "echo-policy-allowed.ran")), true, "approved setup did not run");
    assert.equal(existsSync(join(directory, "echo-policy-denied.ran")), false, "denied setup ran");
  } finally { rmSync(directory, { recursive: true, force: true }); }
});
