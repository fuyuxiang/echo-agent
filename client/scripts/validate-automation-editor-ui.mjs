// Real production editor/store/dialogs with the isolated fixture IPC.
import assert from 'node:assert/strict';
import { join } from 'node:path';

export async function validateAutomationEditorUi(page, output, origin = 'http://127.0.0.1:1439') {
  let layouts = 0;
  for (const [width, height] of [[1024, 680], [1440, 900]]) for (const theme of ['light', 'dark']) {
    await page.setViewportSize({ width, height });
    await page.goto(`${origin}/__ui-review?surface=automation-flow&theme=${theme}`);
    await page.getByText('保留草稿与通知导航验收任务', { exact: true }).click();
    await page.getByRole('button', { name: '编辑任务', exact: true }).click();
    const name = page.getByRole('textbox', { name: '名称', exact: true }), marker = '不能丢失的自动化草稿 · 中文文件与交付报告';
    await name.fill(marker);
    await page.getByRole('button', { name: '返回列表', exact: true }).click();
    await page.getByRole('status', { name: '保留的自动化草稿' }).waitFor();
    assert.equal(await name.count(), 0, 'back navigation must show the list');
    await page.getByRole('button', { name: '模拟任务通知' }).click();
    await page.getByRole('heading', { name: '最近运行结果', exact: true }).waitFor();
    assert((await page.getByRole('status', { name: '保留的自动化草稿' }).innerText()).includes(marker));
    assert(await page.getByText('未找到关联任务或运行记录，可能已被删除或清理。', { exact: true }).isVisible());
    const overflow = await page.evaluate(() => document.documentElement.scrollWidth > innerWidth + 1);
    assert(!overflow, 'draft notice must fit the supported minimum window');
    await page.screenshot({ path: join(output, `automation-draft-notice-${width}-${theme}.png`) });
    await page.getByRole('button', { name: '继续编辑', exact: true }).focus();
    await page.keyboard.press('Enter');
    assert.equal(await name.inputValue(), marker, 'keyboard resume must retain input');
    assert.equal(await page.getByRole('button', { name: '保存并立即运行', exact: true }).count(), 1);
    await page.getByRole('button', { name: '取消', exact: true }).click();
    await page.getByRole('alertdialog', { name: '舍弃未保存的修改？' }).waitFor();
    await page.keyboard.press('Escape');
    assert.equal(await name.inputValue(), marker, 'Escape must preserve the draft');
    await page.getByRole('button', { name: '返回列表', exact: true }).click();
    await page.getByRole('button', { name: '舍弃草稿', exact: true }).click();
    await page.getByRole('alertdialog').getByRole('button', { name: '舍弃修改', exact: true }).click();
    assert.equal(await page.getByRole('status', { name: '保留的自动化草稿' }).count(), 0);
    // A disappeared task offers an explicit recovery path, with no automatic creation.
    await page.goto(`${origin}/__ui-review?surface=automation-flow&state=removed&theme=${theme}`);
    await page.getByRole('button', { name: '转为新任务草稿', exact: true }).waitFor();
    await name.fill(marker);
    await page.getByRole('button', { name: '转为新任务草稿', exact: true }).click();
    assert.equal(await name.inputValue(), marker);
    assert.equal(await page.getByRole('button', { name: '保存并立即运行', exact: true }).count(), 0, 'recovery stays a new unsaved draft');
    await page.getByRole('button', { name: '创建任务', exact: true }).click();
    await page.getByText(marker, { exact: true }).waitFor();
    assert.equal(await name.count(), 0, 'saving the recovered copy returns to the list');
    layouts += 1;
  }
  return layouts;
}
