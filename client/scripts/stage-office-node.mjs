import { chmodSync, copyFileSync, existsSync, lstatSync, mkdirSync, realpathSync, renameSync, rmSync } from "node:fs";
import { execFileSync } from "node:child_process";
import { dirname, join, resolve } from "node:path";
import { clearRetiredResourceCopies, ensureOfficeNodeWritable } from "./ensure-office-node-writable.mjs";
import { assertSupportedNodeVersion } from "./node-runtime-version.mjs";

const projectRoot = resolve(import.meta.dirname, "..");
const officeRoot = join(projectRoot, "src-tauri", "resources", "office");
const nodeRoot = join(officeRoot, "node");
const target = join(nodeRoot, process.platform === "win32" ? "node.exe" : "bin/node");
const license = join(nodeRoot, "LICENSE");
const checkOnly = process.argv.includes("--check");
assertSupportedNodeVersion();

const targetArch = process.env.TAURI_ENV_ARCH ?? process.env.CARGO_BUILD_TARGET?.split("-")[0];
const runtimeArch = { aarch64: "arm64", x86_64: "x64" }[targetArch] ?? targetArch;
if (runtimeArch && runtimeArch !== process.arch) {
  throw new Error(`Office Node.js needs a ${runtimeArch} build runtime for this package (found ${process.arch}).`);
}

if (!checkOnly) {
  const source = realpathSync(process.execPath);
  const nodeDir = dirname(source);
  const licenseCandidates = [
    join(nodeDir, "LICENSE"),
    join(nodeDir, "../LICENSE"),
    join(nodeDir, `../node_modules/node-${process.platform}-${process.arch}/LICENSE`),
    join(nodeDir, "../share/doc/node/LICENSE"),
    join(projectRoot, `vendor/nodejs/LICENSE-${process.versions.node}`),
  ];
  const sourceLicense = licenseCandidates.find(existsSync);
  if (!sourceLicense) throw new Error("The bundled Office Node.js runtime needs its LICENSE file.");

  clearRetiredResourceCopies(projectRoot);
  mkdirSync(dirname(target), { recursive: true });
  ensureOfficeNodeWritable(projectRoot, target);
  if (!existsSync(target) || realpathSync(target) !== source) {
    const stage = `${target}.${process.pid}.stage`;
    try {
      copyFileSync(source, stage);
      if (process.platform !== "win32") chmodSync(stage, 0o755);
      renameSync(stage, target);
    } finally {
      rmSync(stage, { force: true });
    }
  }
  if (sourceLicense !== license) copyFileSync(sourceLicense, license);
  ensureOfficeNodeWritable(projectRoot, target);
}

const required = [target, license];
if (checkOnly) required.push(join(officeRoot, "worker.mjs"), join(officeRoot, "fonts/NotoSansCJKsc-Regular.otf"));
for (const path of required) {
  if (!existsSync(path) || !lstatSync(path).isFile()) {
    throw new Error(`Bundled Office resource missing or invalid: ${path}. Run pnpm office:build.`);
  }
}
const runtime = JSON.parse(execFileSync(target, ["-p", "JSON.stringify({version:process.versions.node,arch:process.arch})"], {
  encoding: "utf8",
  windowsHide: true,
}));
assertSupportedNodeVersion(runtime.version);
if (runtime.arch !== process.arch || runtime.version !== process.versions.node) {
  throw new Error(`Staged Office Node.js ${runtime.version} (${runtime.arch}) differs from the build runtime.`);
}
