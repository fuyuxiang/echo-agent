import { gzipSync } from "node:zlib";
import { readFile, readdir, stat } from "node:fs/promises";
import { resolve, sep } from "node:path";

// Deliberate CI budgets, not Vite warning settings. The route-split baseline is
// ~345 KiB raw / ~109 KiB gzip, so these leave roughly 15-20% dependency drift
// headroom while still catching a page accidentally imported into the entry.
// Raise a budget only with a measured build and an explanation in review.
const BUDGETS = Object.freeze({
  initialRawBytes: 400 * 1024,
  initialGzipBytes: 130 * 1024,
  anyChunkRawBytes: 500 * 1024,
});

const outputDir = resolve(
  process.cwd(),
  process.env.ECHO_DASHBOARD_OUT_DIR ?? "dist",
);
const indexPath = resolve(outputDir, "index.html");
const html = await readFile(indexPath, "utf8");

function attribute(tag, name) {
  const match = tag.match(new RegExp(`\\b${name}\\s*=\\s*["']([^"']+)["']`, "i"));
  return match?.[1] ?? "";
}

const initialUrls = new Set();
for (const match of html.matchAll(/<(?:script|link)\b[^>]*>/gi)) {
  const tag = match[0];
  if (/^<script\b/i.test(tag) && attribute(tag, "type") === "module") {
    const src = attribute(tag, "src");
    if (src) initialUrls.add(src);
  }
  if (/^<link\b/i.test(tag)) {
    const rel = attribute(tag, "rel").toLowerCase().split(/\s+/);
    const href = attribute(tag, "href");
    if (rel.includes("modulepreload") && href) initialUrls.add(href);
  }
}

if (initialUrls.size === 0) {
  throw new Error(`No initial module scripts found in ${indexPath}`);
}

function localAssetPath(urlText) {
  const url = new URL(urlText, "https://dashboard.invalid/");
  if (url.origin !== "https://dashboard.invalid") {
    throw new Error(`External initial script is not bundle-budgeted: ${urlText}`);
  }
  const relative = decodeURIComponent(url.pathname).replace(/^\/+/, "");
  const path = resolve(outputDir, relative);
  if (path !== outputDir && !path.startsWith(`${outputDir}${sep}`)) {
    throw new Error(`Initial script escapes build output: ${urlText}`);
  }
  return path;
}

const initialFiles = [...initialUrls].map(localAssetPath);
let initialRawBytes = 0;
let initialGzipBytes = 0;
for (const path of initialFiles) {
  const content = await readFile(path);
  initialRawBytes += content.byteLength;
  initialGzipBytes += gzipSync(content).byteLength;
}

const assetsDir = resolve(outputDir, "assets");
const jsChunks = (await readdir(assetsDir))
  .filter((name) => name.endsWith(".js"))
  .map(async (name) => ({ name, bytes: (await stat(resolve(assetsDir, name))).size }));
const chunkSizes = await Promise.all(jsChunks);
const oversizedChunks = chunkSizes.filter(
  ({ bytes }) => bytes > BUDGETS.anyChunkRawBytes,
);

function kib(bytes) {
  return `${(bytes / 1024).toFixed(1)} KiB`;
}

const failures = [];
if (initialRawBytes > BUDGETS.initialRawBytes) {
  failures.push(
    `initial JS raw ${kib(initialRawBytes)} exceeds ${kib(BUDGETS.initialRawBytes)}`,
  );
}
if (initialGzipBytes > BUDGETS.initialGzipBytes) {
  failures.push(
    `initial JS gzip ${kib(initialGzipBytes)} exceeds ${kib(BUDGETS.initialGzipBytes)}`,
  );
}
for (const { name, bytes } of oversizedChunks) {
  failures.push(
    `${name} raw ${kib(bytes)} exceeds per-chunk ${kib(BUDGETS.anyChunkRawBytes)}`,
  );
}

if (failures.length) {
  throw new Error(`Dashboard bundle budget failed:\n- ${failures.join("\n- ")}`);
}

console.log(
  `Bundle budget passed: initial ${kib(initialRawBytes)} raw / ` +
  `${kib(initialGzipBytes)} gzip across ${initialFiles.length} file(s); ` +
  `${chunkSizes.length} JS chunks.`,
);
