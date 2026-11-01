import { mkdtempSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterEach, expect, it } from "vitest";
import { isTheiaWindowlessRuntime, patchTheiaWindowlessHelpers } from "./patch-theia-windowless-helpers.mjs";

const sourceFiles = [
  "packages/core/src/node/messaging/ipc-connection-provider.ts",
  "packages/plugin-ext/src/hosted/node/hosted-plugin-process.ts",
];
const runtimeFiles = [
  "packages/core/lib/node/messaging/ipc-connection-provider.js",
  "packages/plugin-ext/lib/hosted/node/hosted-plugin-process.js",
];
const directories = [];

function fixture(eol = "\n") {
  const root = mkdtempSync(join(tmpdir(), "echo-windowless-theia-"));
  directories.push(root);
  for (const file of sourceFiles) {
    const path = join(root, file);
    mkdirSync(join(path, ".."), { recursive: true });
    writeFileSync(path, "const options = {\n            windowsHide: process.platform === 'win32',\n};\n".replace(/\n/g, eol));
  }
  return root;
}

afterEach(() => {
  for (const root of directories.splice(0)) rmSync(root, { recursive: true, force: true });
});

it.each(["\n", "\r\n"])("patches both background forks once and preserves %j line endings", (eol) => {
  const root = fixture(eol);
  patchTheiaWindowlessHelpers(root);
  for (const file of sourceFiles) {
    const source = readFileSync(join(root, file), "utf8");
    expect(source).toContain("detached: process.platform === 'win32'");
    expect(source.replaceAll(eol, "")).not.toMatch(/[\r\n]/);
    patchTheiaWindowlessHelpers(root);
    expect(readFileSync(join(root, file), "utf8")).toBe(source);
  }
  writeFileSync(join(root, sourceFiles[1]), "const options = { windowsHide: false };\n");
  expect(() => patchTheiaWindowlessHelpers(root)).toThrow(/fork changed/);
});

it.each(["\n", "\r\n"])("accepts the actual patched repository sources with %j line endings", (eol) => {
  const root = fixture();
  const originals = sourceFiles.map(file => {
    const source = readFileSync(new URL(`../vendor/theia-platform/${file}`, import.meta.url), "utf8")
      .replace(/\r\n/g, "\n").replace(/\n/g, eol);
    writeFileSync(join(root, file), source);
    return source;
  });
  patchTheiaWindowlessHelpers(root);
  patchTheiaWindowlessHelpers(root);
  expect(sourceFiles.map(file => readFileSync(join(root, file), "utf8"))).toEqual(originals);
});

it.each(["\n", "\r\n"])("still rejects altered or duplicated detached options with %j line endings", (eol) => {
  const root = fixture(eol);
  patchTheiaWindowlessHelpers(root);
  const file = join(root, sourceFiles[0]);
  const patched = readFileSync(file, "utf8");
  writeFileSync(file, patched.replace("detached: process.platform === 'win32'", "detached: true"));
  expect(() => patchTheiaWindowlessHelpers(root)).toThrow(/fork changed/);
  writeFileSync(file, `${patched}${eol}            detached: process.platform === 'win32',${eol}`);
  expect(() => patchTheiaWindowlessHelpers(root)).toThrow(/fork changed/);
});

it("rejects a staged runtime missing either worker's windowless option", () => {
  const root = fixture();
  const cleanupPath = join(root, "packages/core/lib/node/process-utils.js");
  mkdirSync(join(cleanupPath, ".."), { recursive: true });
  writeFileSync(cleanupPath, "cp.spawnSync('taskkill.exe', ['/f', '/t', '/pid', ppid.toString(10)], { encoding: 'utf8', windowsHide: true });\n");
  for (const file of runtimeFiles) {
    const path = join(root, file);
    mkdirSync(join(path, ".."), { recursive: true });
    writeFileSync(path, "const options = { windowsHide: process.platform === 'win32', detached: process.platform === 'win32' }; cp.fork('worker', [], options);\n");
  }
  expect(isTheiaWindowlessRuntime(root)).toBe(true);
  writeFileSync(join(root, runtimeFiles[1]), "cp.fork('worker', [], { windowsHide: true });\n");
  expect(isTheiaWindowlessRuntime(root)).toBe(false);
});

it("rejects a runtime whose cleanup still opens a Windows console", () => {
  const root = fixture();
  for (const file of runtimeFiles) {
    const path = join(root, file);
    mkdirSync(join(path, ".."), { recursive: true });
    writeFileSync(path, "const options = { windowsHide: process.platform === 'win32', detached: process.platform === 'win32' }; cp.fork('worker', [], options);\n");
  }
  const cleanupPath = join(root, "packages/core/lib/node/process-utils.js");
  writeFileSync(cleanupPath, "cp.spawnSync('taskkill.exe', ['/f', '/t', '/pid', ppid.toString(10)], { encoding: 'utf8' });\n");
  expect(isTheiaWindowlessRuntime(root)).toBe(false);
});
