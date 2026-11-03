import { build } from "esbuild";
import { copyFile, mkdir } from "node:fs/promises";
import { resolve } from "node:path";

const root = resolve(import.meta.dirname, "..");
const output = resolve(root, "src-tauri/resources/office/worker.mjs");
await mkdir(resolve(root, "src-tauri/resources/office"), { recursive: true });
await build({
  entryPoints: [resolve(root, "scripts/office-worker.mjs")],
  outfile: output,
  bundle: true,
  platform: "node",
  format: "esm",
  target: "node22",
  sourcemap: false,
  legalComments: "eof",
  banner: { js: 'import { createRequire as __echoCreateRequire } from "node:module"; const require = __echoCreateRequire(import.meta.url);' },
});
const licenseDirectory = resolve(root, "src-tauri/resources/office/licenses");
await mkdir(licenseDirectory, { recursive: true });
for (const [name, license] of [
  ["docx", "LICENSE"],
  ["exceljs", "LICENSE"],
  ["pptxgenjs", "LICENSE"],
  ["pdf-lib", "LICENSE.md"],
  ["marked", "LICENSE"],
  ["jszip", "LICENSE.markdown"],
]) {
  await copyFile(
    resolve(root, "node_modules", name, license),
    resolve(licenseDirectory, `${name}-${license}`),
  );
}
await copyFile(
  resolve(root, "node_modules/@pdf-lib/fontkit/package.json"),
  resolve(licenseDirectory, "pdf-lib-fontkit-package.json"),
);
for (const [source, target] of [
  ["@tiptap/react/LICENSE.md", "tiptap-LICENSE.md"],
  ["@tiptap/pm/LICENSE", "tiptap-pm-LICENSE"],
  ["@tiptap/pm/THIRD_PARTY_LICENSES.md", "prosemirror-THIRD_PARTY_LICENSES.md"],
]) await copyFile(resolve(root, "node_modules", source), resolve(licenseDirectory, target));
console.log(`Built office worker: ${output}`);
