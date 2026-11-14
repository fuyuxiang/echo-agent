import { spawn } from "node:child_process";
import { createWriteStream, mkdirSync, mkdtempSync } from "node:fs";
import { join } from "node:path";
import { finished } from "node:stream/promises";
import { StringDecoder } from "node:string_decoder";

// Keep compiler/test progress live, and expose the actual failure in public
// check annotations even when downloading the complete job log needs login.
export async function runNativeTestProcess(command, args, options = {}) {
  const env = options.env ?? process.env;
  let directory;
  if (env.NATIVE_TEST_OUTPUT) {
    mkdirSync(env.NATIVE_TEST_OUTPUT, { recursive: true });
    directory = mkdtempSync(join(env.NATIVE_TEST_OUTPUT, "test-"));
  }
  const child = spawn(command, args, { ...options, env, stdio: ["inherit", "pipe", "pipe"] });
  const tails = ["", ""];
  const logs = [];
  for (const [index, [stream, output]] of [[child.stdout, process.stdout], [child.stderr, process.stderr]].entries()) {
    const decoder = new StringDecoder("utf8");
    const retain = (text) => { tails[index] = (tails[index] + text).slice(-12_000); };
    stream.pipe(output, { end: false });
    if (directory) {
      const file = createWriteStream(join(directory, index === 0 ? "stdout.log" : "stderr.log"));
      stream.pipe(file);
      const saved = finished(file);
      void saved.catch(() => {});
      logs.push(saved);
    }
    stream.on("data", chunk => retain(decoder.write(chunk)));
    stream.on("end", () => retain(decoder.end()));
  }
  const status = await new Promise((resolve, reject) => {
    child.once("error", reject);
    child.once("close", (code) => resolve(code ?? 1));
  });
  await Promise.all(logs);
  if (status !== 0 && env.GITHUB_ACTIONS === "true") {
    reportNativeTestFailure(status, tails, env);
  }
  return status;
}

export function reportNativeTestFailure(status, tails, env = process.env, title = "Native host tests failed") {
  if (env.GITHUB_ACTIONS !== "true") return;
  // libtest writes failed assertions to stdout; Cargo writes build warnings
  // to stderr. Combining them loses stdout, and a 12k annotation is itself
  // truncated by GitHub before its useful tail. Retain each channel within
  // the annotation limit and preserve the complete streams as artifacts.
  for (const [index, tail] of tails.entries()) {
    if (!tail && (index !== 0 || tails[1])) continue;
    const text = tail.replace(/\u001b\[[0-?]*[ -/]*[@-~]/g, "").slice(-2800);
    const message = text.replaceAll("%", "%25").replaceAll("\r", "%0D").replaceAll("\n", "%0A");
    console.error(`::error title=${title}::Exit code ${status}%0A${index === 0 ? "stdout" : "stderr"}%0A${message}`);
  }
}
