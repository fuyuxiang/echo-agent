import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import { appendFileSync, existsSync, mkdirSync, mkdtempSync, readFileSync, renameSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { basename, join } from "node:path";

const targets = {
  "darwin/arm64": "aarch64-apple-darwin",
  "darwin/x64": "x86_64-apple-darwin",
  "win32/x64": "x86_64-pc-windows-msvc",
};
const host = `${process.platform}/${process.arch}`;
const target = targets[host];
if (!target) throw new Error(`Unsupported ONNX Runtime packaging host: ${host}`);
const distribution = readFileSync(new URL("../vendor/ort-sys/dist.txt", import.meta.url), "utf8")
  .split(/\r?\n/)
  .map(line => line.split("\t"))
  .find(([features, triple]) => features === "none" && triple === target);
if (!distribution) throw new Error(`Missing ONNX Runtime distribution for ${target}`);
const [, , url, checksum] = distribution;
const expected = checksum.toLowerCase();
const digest = path => createHash("sha256").update(readFileSync(path)).digest("hex");
const cache = join(process.env.RUNNER_TEMP ?? tmpdir(), "echo-onnx-runtime");
mkdirSync(cache, { recursive: true });
const archive = join(cache, basename(new URL(url).pathname));
if (!existsSync(archive) || digest(archive) !== expected) {
  const staging = mkdtempSync(join(cache, "download-"));
  try {
    const partial = join(staging, basename(archive));
    execFileSync(process.platform === "win32" ? "curl.exe" : "curl", [
      "--fail", "--silent", "--show-error", "--location", "--proto", "=https", "--proto-redir", "=https",
      "--connect-timeout", "20", "--max-time", "90", "--retry", "4",
      "--retry-all-errors", "--retry-max-time", "300", "--output", partial, url,
    ], { stdio: "inherit" });
    if (digest(partial) !== expected) throw new Error(`ONNX Runtime SHA-256 mismatch for ${target}`);
    renameSync(partial, archive);
  } finally {
    rmSync(staging, { recursive: true, force: true });
  }
}
if (process.env.GITHUB_ENV) appendFileSync(process.env.GITHUB_ENV, `ORT_ARCHIVE=${archive}\n`);
console.log(`Verified ONNX Runtime archive for ${target}: ${archive}`);
