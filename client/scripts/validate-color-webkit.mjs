import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { promisify } from "node:util";
import { mkdtempSync,readFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join,resolve } from "node:path";
import { createUiReviewServer,reviewOutput } from "./ui-review-runtime.mjs";
import { assertNativeReviewSessionUnlocked } from "./lib/native-review-window.mjs";

assert.equal(process.platform,"darwin","Native form acceptance requires macOS; do not count another engine as WKWebView");
const active=process.argv.includes("--active");
if(active)assertNativeReviewSessionUnlocked();
const output=reviewOutput(mkdtempSync(join(tmpdir(),"echo-native-colors-")));
console.log(`Native form color evidence: ${output}`);
const server=await createUiReviewServer(0,{fixture:"/scripts/fixtures/native-color-review.tsx"});
try {
  const executable=resolve(output,"native-color-review");
  // Keep the Swift program's 180-second render watchdog independent of cold
  // AppKit/WebKit compilation, as in the native Mermaid review.
  console.log("Compiling native form color review.");
  await promisify(execFile)("/usr/bin/swiftc",["-module-cache-path",process.env.NATIVE_COLOR_MODULE_CACHE || join(output,"swift-modules"),resolve("scripts/validate-color-webkit.swift"),"-o",executable],{timeout:360000,maxBuffer:1024*1024});
  console.log("Running native form color review.");
  const {stdout}=await promisify(execFile)(executable,[`http://127.0.0.1:${server.httpServer.address().port}`,output,...(active?["--active"]:[])],{timeout:210000,maxBuffer:1024*1024});
  console.log(stdout.trim());
  const results=JSON.parse(readFileSync(join(output,"native-colors.json"),"utf8"));
  assert.equal(results.length,4);
  for(const result of results){
    assert.equal(result.checked,true);
    if(active)assert(result.focused.controls.some(c=>c.role==="focus"),"Active native field must expose a measured focus indicator");
    for(const state of ["original","toggled","focused"]){
    assert.deepEqual(result[state].failures,[]);assert.deepEqual(result[state].controlFailures,[]);
    }
  }
  console.log(`Native forms passed: ${results.length} configurations, live theme changes, Checkbox/Radio/Select, ${active ? "visible focus indicators" : "inactive-window focus requests"} and screenshots.`);
} finally {await server.close();}
