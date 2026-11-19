// Production ChatView + QueuePanel/CSS, isolated fixture IPC; no native claims.
import assert from 'node:assert/strict';
import { mkdtempSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { createUiReviewServer, reviewChromium, reviewOutput } from './ui-review-runtime.mjs';

export async function validateQueueUi(page, output, origin = 'http://127.0.0.1:1439') {
  let layouts = 0;
  for (const [width, height] of [[1440, 900], [1024, 768], [768, 720], [1024, 680]]) {
    for (const theme of ['light', 'dark']) {
      await page.setViewportSize({ width, height });
      await page.goto(`${origin}/__ui-review?surface=queue&theme=${theme}`);
      const queue = page.getByRole('list', { name: '待发送队列' });
      const toggle = page.getByRole('button', { name: /^待发送 20 条/ });
      await toggle.waitFor();
      assert.equal(await toggle.getAttribute('aria-expanded'), 'false');
      assert.equal(await queue.count(), 0);
      await toggle.press('Enter');
      await queue.waitFor();
      assert.equal(await queue.getByRole('listitem').count(), 20);
      const geometry = await page.evaluate(() => {
        const rect = selector => {
          const { top, bottom, height } = document.querySelector(selector).getBoundingClientRect();
          const element = document.querySelector(selector);
          return { top, bottom, height, overflow: element.scrollWidth > element.clientWidth + 1 };
        };
        return { queue: rect('.queue-panel'), composer: rect('.echo-composer'),
          history: rect('.chatview__scroll'), overflow: document.documentElement.scrollWidth > innerWidth + 1 };
      });
      await page.screenshot({ path: join(output, `queue-${width}-${height}-${theme}.png`) });
      assert.ok(geometry.queue.height <= 242, `long queue must scroll internally: ${JSON.stringify(geometry)}`);
      assert.ok(geometry.composer.top >= 0 && geometry.composer.bottom <= height, 'queue must leave composer in view');
      assert.ok(geometry.history.height >= 80, 'queue must leave room to read the conversation');
      assert.ok(!geometry.overflow, 'queue must not overflow horizontally');
      assert.ok(!geometry.queue.overflow, 'queue contents must not scroll horizontally');
      const edit = page.getByRole('button', { name: '编辑消息：待发送消息 20：核对中文文件与任务交付结果。' });
      await edit.focus();
      await page.keyboard.press('Enter');
      const input = queue.getByRole('textbox');
      await input.fill('仅保留附件');
      await page.keyboard.press('Escape');
      assert.equal(await queue.getByRole('textbox').count(), 0, 'Escape must cancel queue editing');
      assert.ok(await edit.isVisible(), 'keyboard editing must keep the row accessible');
      layouts += 1;
    }
  }
  return layouts;
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  const output = reviewOutput(mkdtempSync(join(tmpdir(), 'echo-queue-ui-')));
  console.log(`Queue UI screenshots: ${output}`);
  const server = await createUiReviewServer(0);
  let browser;
  try {
    browser = await (await reviewChromium()).launch({ headless: true,
      executablePath: process.env.CHROME_BIN || '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome' });
    const page = await browser.newPage();
    const origin = `http://127.0.0.1:${server.httpServer.address().port}`;
    const layouts = await validateQueueUi(page, output, origin);
    console.log(JSON.stringify({ passed: true, layouts, output, native: false }));
  } finally { await browser?.close(); await server.close(); }
}
