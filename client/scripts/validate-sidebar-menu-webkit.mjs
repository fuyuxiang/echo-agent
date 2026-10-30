// Run with the project's supported Node runtime on an unlocked macOS desktop.
// Uses the production React fixture and AppKit events scoped to its own window.
import { spawn, execFile } from "node:child_process";
import { promisify } from "node:util";
import { mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { createUiReviewServer, reviewOutput } from "./ui-review-runtime.mjs";
import { assertNativeReviewSessionUnlocked } from "./lib/native-review-window.mjs";

if (process.platform !== "darwin") {
  console.log("Sidebar native WebView validation requires macOS.");
  process.exit(0);
}
assertNativeReviewSessionUnlocked();
const output = reviewOutput(mkdtempSync(join(tmpdir(), "echo-sidebar-webkit-")));
const executable = join(output, "sidebar-menu-review");
await promisify(execFile)("swiftc", [resolve("scripts/validate-sidebar-menu-webkit.swift"), "-o", executable]);
const server = await createUiReviewServer(0);
try {
  const child = spawn(executable, [`http://127.0.0.1:${server.httpServer.address().port}`, output], { stdio: "inherit" });
  process.exitCode = await new Promise((resolve, reject) => {
    child.once("error", reject);
    child.once("close", code => resolve(code ?? 1));
  });
  console.log(`Native menu evidence: ${output}`);
} finally {
  await server.close();
}
