import { spawn } from "node:child_process";
import { StringDecoder } from "node:string_decoder";

// Keep compiler/test progress live, and expose the actual failure in public
// check annotations even when downloading the complete job log needs login.
export async function runNativeTestProcess(command, args, options = {}) {
  const env = options.env ?? process.env;
  const child = spawn(command, args, { ...options, env, stdio: ["inherit", "pipe", "pipe"] });
  const tails = ["", ""];
  for (const [index, [stream, output]] of [[child.stdout, process.stdout], [child.stderr, process.stderr]].entries()) {
    const decoder = new StringDecoder("utf8");
    const retain = (text) => { tails[index] = (tails[index] + text).slice(-12_000); };
    stream.pipe(output, { end: false });
    stream.on("data", chunk => retain(decoder.write(chunk)));
    stream.on("end", () => retain(decoder.end()));
  }
  const status = await new Promise((resolve, reject) => {
    child.once("error", reject);
    child.once("close", (code) => resolve(code ?? 1));
  });
  if (status !== 0 && env.GITHUB_ACTIONS === "true") {
    // Separate pipes can arrive out of order. Late stdout must not displace
    // the panic/stack trace from stderr in the bounded annotation.
    const tail = tails.join("\n").slice(-12_000);
    const message = tail.replaceAll("%", "%25").replaceAll("\r", "%0D").replaceAll("\n", "%0A");
    console.error(`::error title=Native host tests failed::Exit code ${status}%0A${message}`);
  }
  return status;
}
