import { mkdtempSync, mkdirSync, readFileSync, readdirSync, writeFileSync, copyFileSync, cpSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { spawnSync } from "node:child_process";
import { createTheiaRuntimeManifest } from "./theia-runtime-manifest.mjs";
import { assertSupportedNodeVersion, assertSupportedNpmVersion } from "./node-runtime-version.mjs";

assertSupportedNodeVersion();
const npm = process.platform === "win32" ? "npm.cmd" : "npm";
const npmVersion = spawnSync(npm, ["--version"], { encoding: "utf8", shell: process.platform === "win32" });
if (npmVersion.status !== 0) throw new Error(npmVersion.stderr || "npm is unavailable");
assertSupportedNpmVersion(npmVersion.stdout);

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const source = join(root, "vendor/theia-platform");
const read = path => JSON.parse(readFileSync(path, "utf8"));
const app = read(join(source, "examples/browser/package.json"));
const packages = new Map();
for (const group of ["packages", "examples"]) {
  for (const entry of readdirSync(join(source, group), { withFileTypes: true })) {
    if (!entry.isDirectory()) continue;
    const name = entry.name;
    try {
      const manifest = read(join(source, group, name, "package.json"));
      packages.set(manifest.name, { relative: join(group, name), manifest });
    } catch (error) {
      if (error.code !== "ENOENT") throw error;
    }
  }
}
const temporary = mkdtempSync(join(tmpdir(), "echoagent-theia-lock-"));
try {
  writeFileSync(join(temporary, "package.json"), JSON.stringify(createTheiaRuntimeManifest(app, read(join(source, "package.json"))), null, 2));
  cpSync(join(source, "security"), join(temporary, "security"), { recursive: true });
  const needed = new Set();
  const queue = Object.keys(app.dependencies ?? {});
  while (queue.length) {
    const name = queue.pop();
    if (needed.has(name) || !packages.has(name)) continue;
    needed.add(name);
    const { relative, manifest } = packages.get(name);
    const directory = join(temporary, relative);
    mkdirSync(directory, { recursive: true });
    writeFileSync(join(directory, "package.json"), JSON.stringify({ ...manifest, scripts: {}, devDependencies: {} }, null, 2));
    queue.push(...Object.keys(manifest.dependencies ?? {}), ...Object.keys(manifest.optionalDependencies ?? {}));
  }
  const lock = join(root, "scripts/theia-runtime-package-lock.json");
  // Preserve the previously reviewed graph while resolving changed manifests
  // and security workspaces. A fresh solve upgrades unrelated dependencies.
  copyFileSync(lock, join(temporary, "package-lock.json"));
  const result = spawnSync(npm, ["install", "--package-lock-only", "--ignore-scripts", "--omit=dev", "--no-audit", "--no-fund"], {
    cwd: temporary,
    stdio: "inherit",
    shell: process.platform === "win32",
  });
  if (result.status !== 0) throw new Error("Theia runtime lock update failed");
  copyFileSync(join(temporary, "package-lock.json"), lock);
  console.log(`Updated the production lock for ${needed.size} local Theia packages.`);
} finally {
  rmSync(temporary, { recursive: true, force: true });
}
