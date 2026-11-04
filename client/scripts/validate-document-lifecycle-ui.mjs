import assert from 'node:assert/strict';
import { mkdtempSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { createUiReviewServer, reviewChromium, reviewOutput } from './ui-review-runtime.mjs';

const output = reviewOutput(mkdtempSync(join(tmpdir(), 'echo-document-lifecycle-')));
const server = await createUiReviewServer(0, { fixture: '/scripts/fixtures/document-workspace-review.tsx' });
const report = { passed: false, evidenceLevel: 'production ToolSidePanel, DocumentSession and TipTap / isolated fixture IPC', cases: [] };
const copyPath = '/review/家庭亲子作息表-可编辑副本.docx';
let browser;
try {
  browser = await (await reviewChromium()).launch({ headless: true,
    executablePath: process.env.CHROME_BIN || '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome' });
  for (const theme of ['light', 'dark']) for (const font of [13, 18]) {
    const context = await browser.newContext({ viewport: { width: 1024, height: 680 }, colorScheme: theme });
    const page = await context.newPage();
    page.setDefaultTimeout(20000); page.setDefaultNavigationTimeout(90000);
    const errors = []; page.on('pageerror', error => errors.push(error.message));
    try {
      await page.goto(`http://127.0.0.1:${server.httpServer.address().port}/__ui-review?scenario=markdown-simple&theme=${theme}`, { waitUntil: 'networkidle' });
      await page.evaluate(async font => { const { applyFontSize } = await import('/src/lib/font-size.ts'); applyFontSize(font); }, font);
      await page.getByRole('button', { name: '创建可编辑副本', exact: true }).waitFor();
      await page.evaluate(() => { window.documentReview.delayCopy = true; });
      await page.getByRole('button', { name: '创建可编辑副本', exact: true }).click();
      await page.waitForFunction(() => typeof window.documentReview.releaseCopy === 'function');
      await page.getByRole('button', { name: '关闭面板', exact: true }).click();
      assert.equal(await page.locator('.document-workbench').count(), 1, 'Closing must retain the actual document component');
      await page.evaluate(() => window.documentReview.releaseCopy());
      await page.waitForFunction(path => Boolean(window.documentReview.records[path]), copyPath);
      assert.equal(await page.locator('.tool-side-panel').isVisible(), false, 'Late copy must not reopen the panel');
      assert.equal(await page.getByText('已创建可编辑副本，原件已保留', { exact: true }).count(), 0, 'Late copy must not toast');
      await page.screenshot({ path: join(output, `copy-closed-${theme}-${font}.png`) });

      await page.getByRole('button', { name: '打开文档工作区', exact: true }).click();
      const create = page.getByRole('button', { name: '创建可编辑副本', exact: true });
      await create.waitFor();
      assert.equal(await create.isEnabled(), true, 'Reopening must release the completed action lock');
      await page.evaluate(() => { window.documentReview.delayCopy = false; });
      await create.click();
      await page.getByRole('button', { name: '编辑', exact: true }).waitFor();
      await page.getByRole('button', { name: '编辑', exact: true }).click();
      await page.evaluate(() => { window.documentReview.delayDraftSave = true; });
      const retained = '关闭面板期间仍需保存的用户修改';
      await page.getByRole('textbox', { name: '文档编辑区', exact: true }).fill(retained);
      await page.getByRole('button', { name: '让 Agent 修改', exact: true }).click();
      await page.waitForFunction(() => typeof window.documentReview.releaseDraftSave === 'function');
      await page.getByRole('button', { name: '关闭面板', exact: true }).click();
      await page.evaluate(() => window.documentReview.releaseDraftSave());
      await page.waitForFunction(({ path, text }) => JSON.stringify(window.documentReview.records[path].document).includes(text), { path: copyPath, text: retained });
      assert.equal(await page.getByTestId('document-context').count(), 0, 'A closed view must not inject Agent context after saving');
      assert.equal(await page.locator('.tool-side-panel').isVisible(), false);
      await page.getByRole('button', { name: '打开文档工作区', exact: true }).click();
      assert.equal(await page.getByRole('button', { name: '让 Agent 修改', exact: true }).isEnabled(), true);
      assert.equal(await page.getByRole('textbox', { name: '文档编辑区', exact: true }).textContent(), retained);
      await page.getByRole('button', { name: '让 Agent 修改', exact: true }).click();
      await page.getByTestId('document-context').waitFor();
      const received = await page.evaluate(() => window.documentEditContext);
      assert.equal(received.path, copyPath); assert(received.revision >= 2);
      assert.deepEqual(errors, []);
      await page.screenshot({ path: join(output, `agent-context-${theme}-${font}.png`) });
      report.cases.push({ theme, font, lateCopyRegistered: true, lateNavigationSuppressed: true,
        hiddenDraftSaved: true, lateAgentContextSuppressed: true, reopenedActionsAvailable: true, currentAgentContext: received });
    } finally { await context.close(); }
  }
  report.passed = true;
} finally {
  await browser?.close(); await server.close();
  writeFileSync(join(output, 'report.json'), JSON.stringify(report, null, 2));
  console.log(JSON.stringify({ passed: report.passed, output, cases: report.cases.length }));
}
