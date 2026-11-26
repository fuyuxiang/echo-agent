import { execFileSync } from 'node:child_process';
import { copyFileSync, mkdirSync, mkdtempSync, readFileSync, realpathSync, rmSync, symlinkSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { expect, it } from 'vitest';

function withFixture(run) {
  const root = realpathSync(mkdtempSync(join(tmpdir(), 'echo-document-stage-')));
  try {
    const runner = join(root, 'scripts', 'stage-document-runtime.mjs');
    mkdirSync(dirname(runner));
    copyFileSync(new URL('./stage-document-runtime.mjs', import.meta.url), runner);
    const assets = {
      'cmaps/Fixture.bcmap': 'character map',
      'standard_fonts/fixture.ttf': 'standard font',
      'wasm/fixture.wasm': 'decoder',
      'iccs/fixture.icc': 'color profile',
    };
    const packages = {
      'node_modules/@office-kit/pptx/LICENSE': 'license',
      'node_modules/@office-kit/pptx-preview/LICENSE': 'license',
      'node_modules/ssf/LICENSE': 'license',
      'node_modules/pdfjs-dist/LICENSE': 'license',
      'node_modules/pdfjs-dist/package.json': '{"version":"6.5.438"}',
      'third-party/docx-preview/LICENSE': 'license',
      'third-party/docx-preview/NOTICE': 'notice',
      'third-party/docx-preview/node_modules/@noble/ciphers/LICENSE': 'ciphers license',
      'third-party/docx-preview/node_modules/@noble/hashes/LICENSE': 'hashes license',
      ...Object.fromEntries(Object.entries(assets).map(([path, data]) => [`node_modules/pdfjs-dist/${path}`, data])),
    };
    for (const [name, data] of Object.entries(packages)) {
      const path = join(root, name);
      mkdirSync(dirname(path), { recursive: true });
      writeFileSync(path, data);
    }
    const staged = join(root, 'public', 'document-runtime', 'pdfjs', '6.5.438');
    run({ root, runner, staged, assets });
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
}

it('stages resources when invoked through an aliased checkout', () => {
  withFixture(({ root, staged, assets }) => {
    const alias = join(root, 'checkout-alias');
    symlinkSync(root, alias, process.platform === 'win32' ? 'junction' : 'dir');
    execFileSync(process.execPath, [join(alias, 'scripts', 'stage-document-runtime.mjs')]);
    for (const [path, data] of Object.entries(assets)) {
      expect(readFileSync(join(staged, path), 'utf8')).toBe(data);
    }
    const licenses = join(root, 'public/document-runtime/licenses');
    expect(readFileSync(join(licenses, '@apollo-design-docx-preview-NOTICE'), 'utf8')).toBe('notice');
    expect(readFileSync(join(licenses, '@noble-ciphers-LICENSE'), 'utf8')).toBe('ciphers license');
    expect(readFileSync(join(licenses, '@noble-hashes-LICENSE'), 'utf8')).toBe('hashes license');
  });
});

it('repairs missing and changed PDF preview assets even after an earlier successful staging', () => {
  withFixture(({ runner, staged, assets }) => {
    execFileSync(process.execPath, [runner]);
    rmSync(join(staged, 'cmaps/Fixture.bcmap'));
    writeFileSync(join(staged, 'wasm/fixture.wasm'), 'changed');
    execFileSync(process.execPath, [runner]);
    for (const [path, data] of Object.entries(assets)) {
      expect(readFileSync(join(staged, path), 'utf8')).toBe(data);
    }
  });
});
