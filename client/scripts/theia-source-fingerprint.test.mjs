import { cpSync, existsSync, mkdirSync, mkdtempSync, readFileSync, renameSync, rmSync, utimesSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { createHash } from "node:crypto";
import { spawnSync } from "node:child_process";
import { createRequire } from "node:module";
import { expect, it, vi } from "vitest";
import { clearTheiaBuildOutputs, theiaSourceFingerprint } from "./theia-source-fingerprint.mjs";

it("invalidates the staged IDE by source content and path even when edit timestamps are older", () => {
  const root = mkdtempSync(join(tmpdir(), "echo-theia-build-inputs-"));
  const source = join(root, "vendor/theia-platform");
  const old = 1_700_000_000;
  const put = (path) => {
    const file = join(root, path);
    mkdirSync(dirname(file), { recursive: true });
    writeFileSync(file, "fixture");
    utimesSync(file, old, old);
    return file;
  };
  try {
    for (const area of ["packages", "dev-packages", "security", "examples/browser", "examples/echo-coding-bridge"]) {
      put(`vendor/theia-platform/${area}/input`);
    }
    for (const name of ["package.json", "package-lock.json", "lerna.json"]) put(`vendor/theia-platform/${name}`);
    put("vendor/theia-language-plugins/input");
    put("vendor/theia-vsix/manifest.json");
    put("vendor/nodejs/input");
    for (const name of [
      "build-theia.mjs", "prepare-theia.mjs", "stage-theia-runtime.mjs", "theia-runtime-manifest.mjs",
      "node-runtime-version.mjs", "patch-theia-scanoss.mjs", "patch-theia-node-pty.mjs",
      "patch-theia-windowless-helpers.mjs", "patch-theia-git-windowless.mjs", "manage-theia-vsix.mjs",
      "theia-plugin-validation.mjs", "theia-runtime-package-lock.json", "theia-source-fingerprint.mjs",
    ]) put(`scripts/${name}`);
    const config = put("vendor/theia-platform/configs/base.tsconfig.json");
    const compileReferences = put("vendor/theia-platform/scripts/compile-references.js");
    utimesSync(compileReferences, old + 10, old + 10);
    const initial = theiaSourceFingerprint(source);
    writeFileSync(config, "changed");
    utimesSync(config, old - 1, old - 1);
    const edited = theiaSourceFingerprint(source);
    expect(edited).not.toBe(initial);

    const renamed = join(dirname(config), "renamed.tsconfig.json");
    renameSync(config, renamed);
    const afterRename = theiaSourceFingerprint(source);
    expect(afterRename).not.toBe(edited);
    rmSync(renamed);
    const afterRemoval = theiaSourceFingerprint(source);
    expect(afterRemoval).not.toBe(afterRename);

    put("vendor/theia-platform/examples/browser/lib/backend/main.js");
    put("vendor/theia-platform/examples/browser/src-gen/index.js");
    put("vendor/theia-platform/examples/browser/gen-esbuild.browser.mjs");
    expect(theiaSourceFingerprint(source)).toBe(afterRemoval);
    put("vendor/theia-platform/security/braces/lib/parse.js");
    expect(theiaSourceFingerprint(source)).not.toBe(afterRemoval);
    const afterSecurityInput = theiaSourceFingerprint(source);
    put("vendor/theia-platform/packages/fixture/src/lib/index.ts");
    expect(theiaSourceFingerprint(source)).not.toBe(afterSecurityInput);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

it("rebuilds older edits and stages no output from renamed or deleted sources", { timeout: 15000 }, () => {
  const source = mkdtempSync(join(tmpdir(), "echo-theia-compiler-"));
  const workspace = join(source, "examples", "echo-coding-bridge");
  const entry = join(workspace, "src", "index.ts");
  const output = join(workspace, "lib", "index.js");
  const oldSource = join(workspace, "src", "old.ts");
  const removed = join(workspace, "src", "removed.ts");
  const staged = join(source, "staged", "lib");
  const tsc = createRequire(import.meta.url).resolve("typescript/lib/tsc.js");
  const compile = () => {
    const result = spawnSync(process.execPath, [tsc, "--build"], { cwd: workspace, encoding: "utf8" });
    expect(result.status, result.error?.message || (result.stdout + result.stderr)).toBe(0);
  };
  try {
    mkdirSync(join(source, "packages"));
    mkdirSync(join(source, "dev-packages"));
    mkdirSync(dirname(entry), { recursive: true });
    writeFileSync(join(workspace, "tsconfig.json"), JSON.stringify({
      compilerOptions: { composite: true, rootDir: "src", outDir: "lib", types: [], skipLibCheck: true },
      include: ["src"],
    }));
    writeFileSync(entry, 'export const value = "before";');
    writeFileSync(oldSource, 'export const file = "renamed";');
    writeFileSync(removed, 'export const file = "removed";');
    compile();
    expect(readFileSync(output, "utf8")).toContain('"before"');
    expect(existsSync(join(workspace, "lib", "old.js"))).toBe(true);
    expect(existsSync(join(workspace, "lib", "removed.js"))).toBe(true);
    mkdirSync(join(workspace, "src-gen"));
    writeFileSync(join(workspace, "src-gen", "obsolete.js"), "old bundle input");
    const vendoredLib = join(source, "security", "braces", "lib");
    mkdirSync(vendoredLib, { recursive: true });
    writeFileSync(join(vendoredLib, "parse.js"), "vendored JavaScript source");

    writeFileSync(entry, 'export const value = "after";');
    utimesSync(entry, 1_700_000_000, 1_700_000_000);
    renameSync(oldSource, join(workspace, "src", "renamed.ts"));
    rmSync(removed);
    clearTheiaBuildOutputs(source);
    compile();
    cpSync(join(workspace, "lib"), staged, { recursive: true });
    expect(readFileSync(join(staged, "index.js"), "utf8")).toContain('"after"');
    expect(readFileSync(join(staged, "renamed.js"), "utf8")).toContain('"renamed"');
    expect(existsSync(join(staged, "old.js"))).toBe(false);
    expect(existsSync(join(staged, "removed.js"))).toBe(false);
    expect(existsSync(join(workspace, "src-gen", "obsolete.js"))).toBe(false);
    expect(existsSync(join(vendoredLib, "parse.js"))).toBe(true);
  } finally {
    rmSync(source, { recursive: true, force: true });
  }
});

it.each([
  { sameSource: true, sameInstall: true },
  { sameSource: false, sameInstall: true },
  { sameSource: true, sameInstall: false },
])("compiles and stamps current inputs: %o", async ({ sameSource, sameInstall }) => {
  vi.resetModules();
  const manifest = "{}";
  const fingerprint = createHash("sha256").update(manifest).update(manifest)
    .update(`${process.platform}/${process.arch}/${process.version}/${process.versions.modules}`).digest("hex");
  const npmCalls = [];
  const environments = [];
  const invalidate = vi.fn();
  const write = vi.fn();
  const sourceFingerprint = vi.fn(() => "source-fingerprint");
  const clearOutputs = vi.fn();
  const mock = {
    "node:fs": {
      readFileSync: path => {
        if (path.endsWith(".echoagent-install-fingerprint")) return sameInstall ? fingerprint : "old-install";
        if (path === "build-stamp") return sameSource ? "source-fingerprint\n" : "old-source-fingerprint\n";
        return manifest;
      },
      existsSync: () => true, copyFileSync: () => {}, rmSync: invalidate, writeFileSync: write,
    },
    "node:child_process": { spawnSync: (_command, args, { env }) => {
      npmCalls.push(args);
      environments.push(env);
      return { status: 0, stdout: "11.21.1" };
    } },
    "./node-runtime-version.mjs": { assertSupportedNodeVersion: () => {}, assertSupportedNpmVersion: () => {} },
    "./patch-theia-node-pty.mjs": {
      patchNodePtyConsoleHelper: () => {}, patchNodePtyPipeErrors: () => {},
      ensureNodePtyConptySourceUnmodified: () => {}, syncNodePtyPrebuilds: () => {}, markNodePtyConptyRebuilt: () => {},
    },
    "./patch-theia-windowless-helpers.mjs": { patchTheiaWindowlessHelpers: () => {}, isTheiaWindowlessRuntime: () => true },
    "./patch-theia-scanoss.mjs": { patchTheiaScanoss: () => {} },
    "./theia-source-fingerprint.mjs": {
      clearTheiaBuildOutputs: clearOutputs,
      theiaBuildStampPath: () => "build-stamp", theiaSourceFingerprint: sourceFingerprint,
    },
  };
  try {
    for (const [id, exports] of Object.entries(mock)) vi.doMock(id, () => exports);
    await import("./build-theia.mjs");
    expect(npmCalls.some(args => args[0] === "ci")).toBe(!sameInstall);
    const compile = npmCalls.findIndex(args => args[0] === "run" && args[1] === "compile");
    expect(compile).toBeGreaterThan(0);
    expect(npmCalls[compile - 1]).toEqual(["run", "compute-references"]);
    expect(invalidate).toHaveBeenCalledWith("build-stamp", { force: true });
    const rebuild = !sameSource || !sameInstall;
    expect(clearOutputs).toHaveBeenCalledTimes(rebuild ? 1 : 0);
    if (rebuild) expect(environments[compile].NX_SKIP_NX_CACHE).toBe("true");
    expect(sourceFingerprint).toHaveBeenCalledTimes(2);
    expect(write).toHaveBeenCalledWith("build-stamp", "source-fingerprint\n");
  } finally {
    for (const id of Object.keys(mock)) vi.doUnmock(id);
  }
});

it("refuses to stage earlier compiler output without touching an existing staged runtime", async () => {
  const remove = vi.fn();
  const mock = {
    "node:fs": { existsSync: () => true, readFileSync: () => "old-source-fingerprint\n", rmSync: remove },
    "./node-runtime-version.mjs": { assertSupportedNodeVersion: () => {} },
    "./theia-source-fingerprint.mjs": {
      theiaBuildStampPath: () => "build-stamp", theiaSourceFingerprint: () => "new-source-fingerprint",
    },
  };
  try {
    for (const [id, exports] of Object.entries(mock)) vi.doMock(id, () => exports);
    await expect(import("./stage-theia-runtime.mjs")).rejects.toThrow("Theia build is missing or out of date");
    expect(remove).not.toHaveBeenCalled();
  } finally {
    for (const id of Object.keys(mock)) vi.doUnmock(id);
  }
});
