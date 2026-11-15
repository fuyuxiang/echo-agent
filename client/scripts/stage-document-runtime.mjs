import { cpSync, existsSync, mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
export function stageDocumentRuntime() {
  const licenses = join(root, 'public/document-runtime/licenses');
  mkdirSync(licenses, { recursive: true });
  for (const [packageName, names] of [
    ['@office-kit/pptx', ['LICENSE']],
    ['@office-kit/pptx-preview', ['LICENSE']],
    ['@apollo-design/docx-preview', ['LICENSE', 'NOTICE']],
    ['pdfjs-dist', ['LICENSE']],
    ['ssf', ['LICENSE']],
  ]) {
    for (const name of names) cpSync(join(root, 'node_modules', packageName, name), join(licenses, `${packageName.replaceAll('/', '-')}-${name}`));
  }
  const source = join(root, 'node_modules/pdfjs-dist');
  const { version } = JSON.parse(readFileSync(join(source, 'package.json'), 'utf8'));
  const destination = join(root, 'public/document-runtime/pdfjs', version);
  const marker = join(destination, '.ready');
  if (existsSync(marker) && readFileSync(marker, 'utf8') === version) return;
  mkdirSync(destination, { recursive: true });
  for (const directory of ['cmaps', 'standard_fonts', 'wasm', 'iccs']) cpSync(join(source, directory), join(destination, directory), { recursive: true });
  cpSync(join(source, 'LICENSE'), join(destination, 'LICENSE'));
  writeFileSync(marker, version);
  console.log(`Staged local PDF preview fonts, CMaps and decoders: ${version}`);
}
if (process.argv[1] && pathToFileURL(resolve(process.argv[1])).href === import.meta.url) stageDocumentRuntime();
