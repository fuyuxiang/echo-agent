import assert from "node:assert/strict";
import { createHash } from "node:crypto";

// Git may check text out with CRLF; the rendered SVG is unchanged.
export function templateDigest(data) {
  return createHash("sha256").update(data.toString("utf8").replace(/\r\n?/g, "\n")).digest("hex");
}

// Tauri emits ICNS entries in an unspecified order. Keep image bytes intact
// while giving the container a stable order, with each legacy mask after RGB.
export function canonicalizeIcns(data) {
  assert.ok(data.length >= 8 && data.toString("ascii", 0, 4) === "icns", "Invalid macOS ICNS header");
  assert.equal(data.readUInt32BE(4), data.length, "Incomplete macOS ICNS file");
  const masks = { s8mk: "is32", l8mk: "il32", h8mk: "ih32", t8mk: "it32" };
  const entries = [];
  const types = new Set();
  for (let offset = 8; offset < data.length;) {
    assert.ok(offset + 8 <= data.length, "Truncated macOS ICNS entry");
    const type = data.toString("ascii", offset, offset + 4);
    const length = data.readUInt32BE(offset + 4);
    assert.ok(length >= 8 && offset + length <= data.length, "Invalid macOS ICNS entry length");
    assert.ok(!types.has(type), `Duplicate macOS ICNS entry: ${type}`);
    types.add(type);
    entries.push({ type, bytes: data.subarray(offset, offset + length), key: `${masks[type] || type}${masks[type] ? "1" : "0"}` });
    offset += length;
  }
  assert.ok(entries.length > 0, "Empty macOS ICNS file");
  entries.sort((a, b) => a.key < b.key ? -1 : a.key > b.key ? 1 : 0);
  // A table of contents, when present, lists each entry's type and length.
  const toc = entries.find(entry => entry.type === "TOC ");
  if (toc) {
    const contents = entries.filter(entry => entry !== toc).map(entry => entry.bytes.subarray(0, 8));
    const length = 8 + contents.length * 8;
    const header = Buffer.alloc(8);
    header.write("TOC ");
    header.writeUInt32BE(length, 4);
    toc.bytes = Buffer.concat([header, ...contents]);
  }
  const result = Buffer.concat([data.subarray(0, 8), ...entries.map(entry => entry.bytes)]);
  result.writeUInt32BE(result.length, 4);
  return result;
}
