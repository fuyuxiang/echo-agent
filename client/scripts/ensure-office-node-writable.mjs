import { chmodSync, existsSync, lstatSync, readdirSync, realpathSync, rmSync } from "node:fs";
import { join, resolve } from "node:path";

function outputProfiles(projectRoot) {
  const roots = new Set([join(projectRoot, "src-tauri", "target")]);
  if (process.env.CARGO_TARGET_DIR) {
    roots.add(resolve(projectRoot, process.env.CARGO_TARGET_DIR));
    roots.add(resolve(projectRoot, "src-tauri", process.env.CARGO_TARGET_DIR));
  }
  const profiles = new Set();
  for (const root of roots) {
    const outputs = [root];
    if (existsSync(root)) {
      for (const entry of readdirSync(root, { withFileTypes: true })) {
        if (entry.isDirectory()) outputs.push(join(root, entry.name));
      }
    }
    for (const output of outputs) {
      for (const profile of ["debug", "release"]) {
        const path = join(output, profile);
        if (existsSync(path)) profiles.add(path);
      }
    }
  }
  return profiles;
}

// Tauri copies the bundled Node binary over an existing copy on rebuilds.
export function ensureOfficeNodeWritable(projectRoot, stagedNode, platform = process.platform) {
  const nodeResource = platform === "win32"
    ? ["office", "node", "node.exe"]
    : ["office", "node", "bin", "node"];
  const candidates = new Set([stagedNode]);
  for (const profile of outputProfiles(projectRoot)) candidates.add(join(profile, ...nodeResource));

  for (const path of candidates) {
    if (!existsSync(path)) continue;
    const stats = lstatSync(path);
    if (!stats.isFile()) throw new Error(`Bundled Node.js must be a regular file: ${path}`);
    if ((stats.mode & 0o200) === 0) chmodSync(path, (stats.mode & 0o777) | 0o200);
  }
}

// Remove only recognized retired payloads from Cargo's generated output trees.
export function clearRetiredResourceCopies(projectRoot) {
  const resources = [join(projectRoot, "src-tauri", "resources")];
  for (const profile of outputProfiles(projectRoot)) {
    resources.push(profile, join(profile, "bundle", "macos", "EchoAgent.app", "Contents", "Resources"));
  }
  for (const root of resources) {
    const payload = join(root, "theia");
    const backend = join(payload, "browser", "lib", "backend", "main.js");
    const node = join(payload, "node", process.platform === "win32" ? "node.exe" : "bin/node");
    if (!existsSync(backend) || !existsSync(node) || !lstatSync(payload).isDirectory()) continue;
    if (realpathSync(payload) !== join(realpathSync(root), "theia")) continue;
    rmSync(payload, { recursive: true });
    const licenses = join(root, "licenses", "theia");
    if (existsSync(join(licenses, "NOTICE.md")) && lstatSync(licenses).isDirectory()
        && realpathSync(licenses) === join(realpathSync(root), "licenses", "theia")) {
      rmSync(licenses, { recursive: true });
    }
  }
}
