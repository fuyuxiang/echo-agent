import { cpSync, lstatSync, mkdirSync, readFileSync, statSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');

function needsCopy(source, destination) {
  const staged = lstatSync(destination, { throwIfNoEntry: false });
  if (!staged?.isFile()) return true;
  const current = statSync(source);
  return !current.isFile() || staged.size !== current.size
    || !readFileSync(source).equals(readFileSync(destination));
}

export function stageDocumentRuntime() {
  const licenses = join(root, 'public/document-runtime/licenses');
  mkdirSync(licenses, { recursive: true });
  const npmPackages = join(root, 'node_modules');
  const docxPreview = join(root, 'third-party/docx-preview');
  for (const [packageName, directory, names] of [
    ['@office-kit/pptx', join(npmPackages, '@office-kit/pptx'), ['LICENSE']],
    ['@office-kit/pptx-preview', join(npmPackages, '@office-kit/pptx-preview'), ['LICENSE']],
    ['@apollo-design/docx-preview', docxPreview, ['LICENSE', 'NOTICE']],
    ['@noble/ciphers', join(docxPreview, 'node_modules/@noble/ciphers'), ['LICENSE']],
    ['@noble/hashes', join(docxPreview, 'node_modules/@noble/hashes'), ['LICENSE']],
    ['pdfjs-dist', join(npmPackages, 'pdfjs-dist'), ['LICENSE']],
    ['ssf', join(npmPackages, 'ssf'), ['LICENSE']],
  ]) {
    for (const name of names) cpSync(join(directory, name), join(licenses, `${packageName.replaceAll('/', '-')}-${name}`));
  }
  const source = join(npmPackages, 'pdfjs-dist');
  const { version } = JSON.parse(readFileSync(join(source, 'package.json'), 'utf8'));
  const destination = join(root, 'public/document-runtime/pdfjs', version);
  for (const name of ['cmaps', 'standard_fonts', 'wasm', 'iccs', 'LICENSE']) {
    cpSync(join(source, name), join(destination, name), { recursive: true, filter: needsCopy });
  }
  console.log(`Local PDF preview resources ready: ${version}`);
}
if (import.meta.main) stageDocumentRuntime();
