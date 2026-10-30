// Exercise macOS button focus with native events; DOM click() skips this bug.
import { spawn, execFile } from "node:child_process";
import { promisify } from "node:util";
import { mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { createUiReviewServer, reviewOutput } from "./ui-review-runtime.mjs";
import { assertNativeReviewSessionUnlocked } from "./lib/native-review-window.mjs";

if (process.platform !== "darwin") {
  console.log("Native automation model validation requires macOS.");
  process.exit(0);
}
assertNativeReviewSessionUnlocked();
const output = reviewOutput(mkdtempSync(join(tmpdir(), "echo-automation-model-webkit-")));
const executable = join(output, "automation-model-review");
await promisify(execFile)("swiftc", [resolve("scripts/validate-automation-model-webkit.swift"), "-o", executable]);
const server = await createUiReviewServer(0);
try {
  const child = spawn(executable, [`http://127.0.0.1:${server.httpServer.address().port}`, output], { stdio: "inherit" });
  process.exitCode = await new Promise((resolve, reject) => {
    child.once("error", reject);
    child.once("close", code => resolve(code ?? 1));
  });
  console.log(`Native automation model evidence: ${output}`);
} finally {
  await server.close();
}
