import assert from "node:assert/strict";
import { runNativeTestProcess } from "./lib/native-test-process.mjs";

assert.equal(process.argv.length, 2, "This entry checks all desktop library warnings; arbitrary flags are not accepted");
// Preserve the strict lint policy and publish the compiler's actual failure
// in check annotations, alongside the complete native-test log artifacts.
process.exitCode = await runNativeTestProcess("cargo", ["clippy", "--locked",
  "--manifest-path", "src-tauri/Cargo.toml", "--lib", "--", "-D", "warnings"]);
