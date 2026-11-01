import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { copyFileSync, mkdirSync, mkdtempSync, readFileSync, rmSync, symlinkSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import test from "node:test";
import { canonicalizeIcns, templateDigest } from "./lib/icon-formats.mjs";

const root = fileURLToPath(new URL("../", import.meta.url));
function entry(type, payload = type) {
  const data = Buffer.from(payload);
  const header = Buffer.alloc(8);
  header.write(type);
  header.writeUInt32BE(8 + data.length, 4);
  return Buffer.concat([header, data]);
}
function family(entries) {
  const data = Buffer.concat([Buffer.from("icns\0\0\0\0"), ...entries]);
  data.writeUInt32BE(data.length, 4);
  return data;
}

test("template digest tolerates LF/CRLF but detects real artwork changes", () => {
  const svg = '<svg>\n<path fill="black"/>\n</svg>\n';
  assert.equal(templateDigest(Buffer.from(svg)), templateDigest(Buffer.from(svg.replaceAll("\n", "\r\n"))));
  assert.notEqual(templateDigest(Buffer.from(svg)), templateDigest(Buffer.from(svg.replace("black", "white"))));
});

test("ICNS ordering is stable, lossless and keeps legacy masks next to RGB", () => {
  const chunks = [entry("is32"), entry("s8mk"), entry("ic10"), entry("il32"), entry("l8mk"), entry("ic08")];
  const expected = family([chunks[5], chunks[2], chunks[3], chunks[4], chunks[0], chunks[1]]);
  const original = family(chunks);
  const before = Buffer.from(original);
  assert.deepEqual(canonicalizeIcns(original), expected);
  assert.deepEqual(canonicalizeIcns(family([...chunks].reverse())), expected);
  assert.deepEqual(canonicalizeIcns(expected), expected);
  assert.deepEqual(original, before, "canonicalizing must not mutate the input");
});

test("ICNS table of contents follows the normalized entries", () => {
  const small = entry("ic08"), large = entry("ic10");
  const toc = entry("TOC ", Buffer.concat([large.subarray(0, 8), small.subarray(0, 8)]));
  const expectedToc = entry("TOC ", Buffer.concat([small.subarray(0, 8), large.subarray(0, 8)]));
  assert.deepEqual(canonicalizeIcns(family([large, toc, small])), family([expectedToc, small, large]));
});

for (const [name, make] of [
  ["short header", () => Buffer.from("icns")],
  ["wrong header", () => Buffer.alloc(8)],
  ["wrong file length", () => { const data = family([entry("ic10")]); data.writeUInt32BE(data.length + 1, 4); return data; }],
  ["empty family", () => family([])],
  ["truncated entry header", () => family([Buffer.from("ic10")])],
  ["zero entry length", () => { const data = entry("ic10"); data.writeUInt32BE(0, 4); return family([data]); }],
  ["entry beyond file", () => { const data = entry("ic10"); data.writeUInt32BE(data.length + 1, 4); return family([data]); }],
  ["duplicate entry", () => family([entry("ic10"), entry("ic10")])],
]) test(`rejects ICNS with ${name}`, () => assert.throws(() => canonicalizeIcns(make()), /ICNS/));

test("real icon pipeline works across text checkouts and regenerates identical assets", { timeout: 180_000 }, async t => {
  const directory = mkdtempSync(join(tmpdir(), "echo-icon-regression-"));
  const manifest = JSON.parse(readFileSync(join(root, "src-tauri/icons/source.json"), "utf8"));
  const files = ["scripts/build-app-icons.mjs", "scripts/build-tray-icons.mjs", "scripts/lib/icon-formats.mjs",
    "src-tauri/icons/source.json", manifest.source.path, manifest.monochrome.path, ...Object.keys(manifest.assets)];
  const bytes = path => readFileSync(join(directory, path));
  const write = (path, data) => writeFileSync(join(directory, path), data);
  const run = (...args) => {
    const result = spawnSync(process.execPath, [join(directory, "scripts/build-app-icons.mjs"), ...args], {
      cwd: directory, encoding: "utf8", timeout: 60_000, maxBuffer: 2 * 1024 * 1024,
    });
    if (result.error) throw result.error;
    return result;
  };
  const success = result => assert.equal(result.status, 0, result.stderr || result.stdout);
  try {
    for (const path of files) {
      mkdirSync(dirname(join(directory, path)), { recursive: true });
      copyFileSync(join(root, path), join(directory, path));
    }
    symlinkSync(join(root, "node_modules"), join(directory, "node_modules"), "junction");
    const template = bytes(manifest.monochrome.path).toString("utf8").replace(/\r\n?/g, "\n");
    await t.test("checked-in resources pass validation", () => success(run("--check")));
    await t.test("CRLF checkout passes validation without regenerating", () => {
      write(manifest.monochrome.path, template.replaceAll("\n", "\r\n"));
      success(run("--check"));
    });
    await t.test("two full generations with CRLF and LF have identical bytes", () => {
      success(run());
      const outputs = ["src-tauri/icons/source.json", ...Object.keys(manifest.assets)];
      const first = outputs.map(path => bytes(path));
      write(manifest.monochrome.path, template);
      success(run());
      outputs.forEach((path, index) => assert.deepEqual(bytes(path), first[index], `Unstable generated resource: ${path}`));
      success(run("--check"));
    });
    await t.test("real template changes still fail validation", () => {
      write(manifest.monochrome.path, template.replace('fill="black"', 'fill="white"'));
      const result = run("--check");
      assert.equal(result.status, 1);
      assert.match(result.stderr, /Tray template changed/);
      write(manifest.monochrome.path, template);
    });
    await t.test("changed source and generated image bytes still fail validation", () => {
      for (const path of [manifest.source.path, "public/app-icon.png"]) {
        const original = bytes(path);
        write(path, Buffer.concat([original, Buffer.from([1])]));
        const result = run("--check");
        assert.equal(result.status, 1);
        assert.match(result.stderr, /App artwork changed|Public app icon differs/);
        write(path, original);
      }
      success(run("--check"));
    });
  } finally {
    rmSync(directory, { recursive: true, force: true });
  }
});
