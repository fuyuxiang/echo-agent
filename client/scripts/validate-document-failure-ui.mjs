import assert from 'node:assert/strict';
import { writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { createUiReviewServer, reviewChromium, reviewOutput } from './ui-review-runtime.mjs';

const baseline = process.argv.includes('--baseline');
const output = reviewOutput(`/private/tmp/echo-document-failure-${baseline ? 'before' : 'after'}`);
const server = await createUiReviewServer(0, { fixture: '/scripts/fixtures/document-workspace-review.tsx', baseline, currentFixtures: true });
const report = { baseline, evidenceLevel: 'real TipTap and production document save controller / simulated IPC', cases: [], passed: false };
let browser;
try {
  browser = await (await reviewChromium()).launch({ headless: true, executablePath: process.env.CHROME_BIN || '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome' });
  for (const theme of ['light', 'dark']) {
    const context = await browser.newContext({ viewport: { width: 1024, height: 680 } });
    const page = await context.newPage(); page.setDefaultNavigationTimeout(90000); page.setDefaultTimeout(20000);
    const errors = []; page.on('pageerror', error => errors.push(error.message));
    try {
      await page.goto(`http://127.0.0.1:${server.httpServer.address().port}/__ui-review?theme=${theme}`, { waitUntil: 'networkidle' });
      await page.getByRole('button', { name: '编辑', exact: true }).click();
      await page.evaluate(() => { window.documentReview.failDraftSave = true; });
      const editor = page.getByRole('textbox', { name: '文档编辑区', exact: true });
      const cell = editor.locator('td').filter({ hasText: '6:30' }).locator('p');
      await cell.click();
      await cell.evaluate(element => {
        const range = document.createRange(); range.selectNodeContents(element);
        const selection = window.getSelection(); selection.removeAllRanges(); selection.addRange(range);
        document.dispatchEvent(new Event('selectionchange'));
      });
      await page.keyboard.insertText('7:45');
      await page.getByRole('alert').filter({ hasText: '编辑稿暂时不可写' }).waitFor();
      const status = await page.locator('.document-workbench__status').textContent();
      assert.equal(status, baseline ? '已保存 · Word 已同步' : '编辑稿保存失败 · 修改已保留');
      assert(await editor.getByText('7:45', { exact: true }).isVisible(), 'Failed writes must retain the local cell');
      assert(!(await page.evaluate(() => JSON.stringify(window.documentReview.records['/review/家庭亲子作息表.docx'].document).includes('7:45'))), 'Fixture unexpectedly committed the failed write');
      await page.screenshot({ path: join(output, `save-failed-${theme}.png`) });
      await page.evaluate(() => { window.documentReview.failDraftSave = false; });
      await page.getByRole('button', { name: '重试保存', exact: true }).click();
      await page.getByText('已保存 · Word 已同步', { exact: true }).waitFor();
      assert(await page.evaluate(() => JSON.stringify(window.documentReview.records['/review/家庭亲子作息表.docx'].document).includes('7:45')), 'Retry must actually commit the retained draft');
      await page.screenshot({ path: join(output, `save-recovered-${theme}.png`) });
      assert.deepEqual(errors, []);
      report.cases.push({ theme, viewport: [1024, 680], dpr: 1, font: 13, failedStatus: status, draftRetained: true, verifiedSavedRecordAfterRetry: true });
    } finally { await context.close(); }
  }
  report.passed = true;
} finally {
  await browser?.close(); await server.close();
  writeFileSync(join(output, 'report.json'), JSON.stringify(report, null, 2));
  console.log(JSON.stringify({ passed: report.passed, baseline, output }));
}
