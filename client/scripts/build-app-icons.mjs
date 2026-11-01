// Export deterministic platform formats from the app artwork without changing it.
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { copyFileSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { buildTrayIcons, renderIcon } from "./build-tray-icons.mjs";
import { canonicalizeIcns, templateDigest } from "./lib/icon-formats.mjs";

const root = fileURLToPath(new URL("../", import.meta.url));
const source = "app-icon.png";
const monochrome = "src-tauri/icons/tray/mark.svg";
const manifestPath = join(root, "src-tauri/icons/source.json");
const native = ["32x32.png", "128x128.png", "128x128@2x.png", "icon.icns", "icon.ico"];
const assets = ["public/app-icon.png", ...native.map(name => `src-tauri/icons/${name}`),
  "src-tauri/icons/tray/macos@2x.png", "src-tauri/icons/tray/windows-64.png"];
const bytes = name => readFileSync(join(root, name));
const hash = name => name === monochrome ? templateDigest(bytes(name)) : createHash("sha256").update(bytes(name)).digest("hex");
const hint = "Run pnpm icons:generate after updating the app artwork or tray template.";

function pngSize(name) {
  const data = bytes(name);
  assert.ok(data.length >= 33 && data.subarray(0, 8).equals(Buffer.from("89504e470d0a1a0a", "hex")), `Invalid PNG: ${name}`);
  return [data.readUInt32BE(16), data.readUInt32BE(20)];
}

function validate() {
  const report = JSON.parse(readFileSync(manifestPath, "utf8"));
  assert.equal(report.version, 1);
  assert.deepEqual(report.source, { path: source, sha256: hash(source) }, `App artwork changed. ${hint}`);
  assert.deepEqual(report.monochrome, { path: monochrome, sha256: hash(monochrome) }, `Tray template changed. ${hint}`);
  assert.deepEqual(Object.keys(report.assets).sort(), [...assets].sort(), `Incomplete icon manifest. ${hint}`);
  assert.ok(bytes(source).equals(bytes("public/app-icon.png")), `Public app icon differs from the source. ${hint}`);
  for (const path of assets) assert.equal(hash(path), report.assets[path], `Stale icon: ${path}. ${hint}`);
  for (const [name, size] of [["32x32.png", 32], ["128x128.png", 128], ["128x128@2x.png", 256],
    ["tray/macos@2x.png", 44], ["tray/windows-64.png", 64]]) {
    assert.deepEqual(pngSize(`src-tauri/icons/${name}`), [size, size], `Wrong icon dimensions: ${name}`);
  }
  const ico = bytes("src-tauri/icons/icon.ico");
  assert.equal(ico.readUInt32LE(0), 0x00010000, "Invalid Windows ICO header");
  const sizes = new Set();
  for (let index = 0; index < ico.readUInt16LE(4); index++) {
    const start = 6 + 16 * index;
    const width = ico[start] || 256, height = ico[start + 1] || 256;
    assert.equal(width, height, "Non-square Windows icon");
    const length = ico.readUInt32LE(start + 8), offset = ico.readUInt32LE(start + 12);
    assert.ok(length > 0 && offset + length <= ico.length, "Invalid Windows icon frame");
    sizes.add(width);
  }
  assert.ok([16, 32, 48, 256].every(size => sizes.has(size)), "Windows icon lacks small or high-DPI frames");
  const icns = bytes("src-tauri/icons/icon.icns");
  assert.ok(canonicalizeIcns(icns).equals(icns), `Unstable macOS ICNS entry order. ${hint}`);
  console.log("App icons verified: source, public image, platform formats and tray assets are in sync.");
}

try {
  assert.ok(process.argv.length === 2 || (process.argv.length === 3 && process.argv[2] === "--check"),
    "Usage: node scripts/build-app-icons.mjs [--check]");
  const [width, height] = pngSize(source);
  assert.ok(width === height && width >= 512, "App artwork must be a square PNG of at least 512 pixels");
  if (process.argv[2] !== "--check") {
    const temporary = mkdtempSync(join(tmpdir(), "echo-app-icons-"));
    try {
      renderIcon(join(root, source), temporary);
      buildTrayIcons(join(temporary, "tray"));
      const icnsPath = join(temporary, "icon.icns");
      writeFileSync(icnsPath, canonicalizeIcns(readFileSync(icnsPath)));
      for (const name of native) copyFileSync(join(temporary, name), join(root, "src-tauri/icons", name));
      for (const name of ["macos@2x.png", "windows-64.png"]) {
        copyFileSync(join(temporary, "tray", name), join(root, "src-tauri/icons/tray", name));
      }
      copyFileSync(join(root, source), join(root, "public/app-icon.png"));
      writeFileSync(manifestPath, JSON.stringify({ version: 1,
        source: { path: source, sha256: hash(source) }, monochrome: { path: monochrome, sha256: hash(monochrome) },
        assets: Object.fromEntries(assets.map(path => [path, hash(path)])),
      }, null, 2) + "\n");
    } finally {
      rmSync(temporary, { recursive: true, force: true });
    }
  }
  validate();
} catch (error) {
  console.error(error.message);
  process.exitCode = 1;
}
