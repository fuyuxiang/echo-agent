import { runNativeTestProcess } from "./lib/native-test-process.mjs";

// CI runs the whole harness; preserve Cargo's local debugging arguments.
process.exitCode = await runNativeTestProcess("cargo", ["test", "--locked",
  "--manifest-path", "scripts/stability-check/Cargo.toml", ...process.argv.slice(2)]);
