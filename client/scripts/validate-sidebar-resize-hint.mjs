import assert from "node:assert/strict";
import { mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { pathToFileURL } from "node:url";
import { createUiReviewServer, reviewChromium, reviewOutput } from "./ui-review-runtime.mjs";

const separator = '[data-sidebar-resize]';
const defaultWidth = 248;
const hint = '.pointer-side-hint';

async function geometry(page, selector) {
  return page.locator(selector).evaluate(element => {
    const { left, top, right, bottom, width, height } = element.getBoundingClientRect();
    const css = getComputedStyle(element);
    return {
      left, top, right, bottom, width, height, text: element.textContent,
      clientHeight: element.clientHeight, scrollHeight: element.scrollHeight,
      clientWidth: element.clientWidth, scrollWidth: element.scrollWidth,
      parent: element.parentElement?.tagName, maxHeight: css.maxHeight,
      overflowY: css.overflowY, fontSize: css.fontSize, lineHeight: css.lineHeight,
      role: element.getAttribute('role'),
      viewport: { width: innerWidth, height: innerHeight },
    };
  });
}

function assertReadable(box, label) {
  assert.equal(box.parent, 'BODY', `${label}: hint should escape sidebar clipping and stacking contexts`);
  assert.ok(box.height >= 20, `${label}: hint has no usable text height`);
  assert.ok(box.scrollHeight <= box.clientHeight + 1, `${label}: text is vertically clipped or scrolls (${box.scrollHeight}/${box.clientHeight})`);
  assert.ok(box.scrollWidth <= box.clientWidth + 1, `${label}: text is horizontally clipped or scrolls`);
  assert.ok(parseFloat(box.lineHeight) >= parseFloat(box.fontSize), `${label}: text line height squeezes enlarged glyphs`);
  assert.ok(box.left >= 7 && box.top >= 7 && box.right <= box.viewport.width - 7 && box.bottom <= box.viewport.height - 7,
    `${label}: hint leaves the safe viewport ${JSON.stringify(box)}`);
}

async function moveToHandle(page, relativeY = 0.5) {
  const box = await page.locator(separator).boundingBox();
  assert.ok(box, 'resize separator should exist');
  const point = { x: Math.min(box.x + box.width / 2, page.viewportSize().width - 2), y: box.y + Math.min(box.height - 2, Math.max(2, box.height * relativeY)) };
  await page.mouse.move(point.x, point.y);
  return point;
}

/** Reproduces the reported crop with the original source via UI_REVIEW_BASELINE=1. */
export async function captureSidebarResizeBaseline(page, output, { baseURL } = {}) {
  const evidence = [];
  for (const platform of ['mac', 'other']) {
    for (const collapsed of [false, true]) {
      await page.setViewportSize({ width: 1024, height: 720 });
      await page.goto(`${baseURL}/__ui-review?platform=${platform}&collapsed=${Number(collapsed)}`);
      await page.locator(separator).waitFor();
      const pointer = await moveToHandle(page);
      await page.getByRole('tooltip').waitFor();
      const tooltip = await geometry(page, '.global-tooltip');
      const anchor = await geometry(page, separator);
      evidence.push({ platform, collapsed, pointer, anchor, tooltip });
      assert.ok(tooltip.scrollHeight > tooltip.clientHeight,
        `${platform}/${collapsed}: baseline should reproduce the documented vertical clipping`);
      await page.screenshot({ path: join(output, `sidebar-resize-before-${platform}-${collapsed ? 'collapsed' : 'expanded'}.png`) });
    }
  }
  writeFileSync(join(output, 'sidebar-resize-before.json'), JSON.stringify(evidence, null, 2));
  return evidence;
}

/** Browser regressions exercise real pointer/focus events and the production shell. */
export async function validateSidebarResizeHint(page, output, { baseURL = 'http://127.0.0.1:1439' } = {}) {
  const evidence = [];
  const open = async (params = {}, viewport = { width: 1024, height: 720 }) => {
    await page.setViewportSize(viewport);
    await page.goto(`${baseURL}/__ui-review?${new URLSearchParams(params)}`);
    await page.locator(separator).waitFor();
    assert.ok(await page.locator(separator).getAttribute('data-tip'), 'separator must participate in shared tooltip coordination');
    assert.equal(await page.locator(separator).getAttribute('data-tip-placement'), 'pointer-side', 'separator must use its pointer-adjacent placement');
    assert.ok(await page.locator(separator).getAttribute('data-tip-focus'), 'separator must explain its keyboard controls on focus');
  };
  const inspect = async (label, { point, role = 'tooltip', screenshot = false } = {}) => {
    await page.locator(hint).waitFor({ state: 'visible' });
    await page.evaluate(() => new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve))));
    assert.equal(await page.locator(hint).count(), 1, `${label}: multiple resize hints`);
    assert.equal(await page.getByRole('tooltip').count(), role === 'tooltip' ? 1 : 0, `${label}: duplicate or stale tooltip`);
    assert.equal(await page.getByRole('status').count(), role === 'status' ? 1 : 0, `${label}: duplicate or stale drag status`);
    const box = await geometry(page, hint);
    assert.equal(box.role, role, `${label}: wrong accessible role`);
    assertReadable(box, label);
    if (point) {
      const dx = Math.max(box.left - point.x, point.x - box.right, 0);
      const dy = Math.max(box.top - point.y, point.y - box.bottom, 0);
      // A collapse preview deliberately clamps the visible divider to its
      // minimum width. The bubble follows that divider and the pointer's Y.
      assert.ok((role === 'status' ? dy : Math.hypot(dx, dy)) <= 40, `${label}: hint is detached from the pointer (${dx}, ${dy})`);
    }
    evidence.push({ label, point, ...box });
    writeFileSync(join(output, 'sidebar-resize-after.json'), JSON.stringify(evidence, null, 2));
    if (screenshot) await page.screenshot({ path: join(output, `sidebar-resize-${label}.png`) });
    return box;
  };
  const assertDismissed = async label => {
    try { await page.waitForFunction(() => !document.querySelector('.pointer-side-hint')); }
    catch (error) { throw new Error(`${label}: hint remains after cancellation`, { cause: error }); }
    assert.equal(await page.locator('.sidebar-resize--active').count(), 0, `${label}: active drag styling remains`);
    assert.equal(await page.locator('.app__body--sidebar-resizing').count(), 0, `${label}: resizing layout state remains`);
  };
  const assertOnlyTooltip = async (expected, label) => {
    await page.waitForFunction(text => {
      const tooltips = document.querySelectorAll('[role="tooltip"]');
      return tooltips.length === 1 && tooltips[0].textContent === text;
    }, expected);
    // Let a pending hover dwell expire before ruling out a second bubble.
    await page.waitForTimeout(400);
    assert.equal(await page.getByRole('tooltip').count(), 1, `${label}: mixed input opened multiple tooltips`);
    assert.equal(await page.getByRole('tooltip').textContent(), expected, `${label}: hover replaced the focused control's hint`);
  };
  const assertWidth = async (width, label) => {
    await page.waitForFunction(({ selector, width }) => document.querySelector(selector)?.getAttribute('aria-valuenow') === String(width), { selector: separator, width });
    assert.equal(await page.locator(separator).getAttribute('aria-valuenow'), String(width), label);
  };

  for (const platform of ['mac', 'other']) {
    for (const collapsed of [false, true]) {
      for (const [edge, relativeY] of [['top', 0.001], ['middle', 0.5], ['bottom', 0.999]]) {
        await open({ platform, collapsed: Number(collapsed), width: 400 });
        const point = await moveToHandle(page, relativeY);
        await inspect(`${platform}-${collapsed ? 'collapsed' : 'expanded'}-${edge}`, { point, screenshot: edge === 'middle' });
        // Moving within the same tall target must follow the new pointer row.
        const movedPoint = await moveToHandle(page, relativeY < 0.5 ? 0.75 : 0.25);
        await inspect(`${platform}-${collapsed ? 'collapsed' : 'expanded'}-${edge}-moved`, { point: movedPoint });
        await page.mouse.move(900, 600);
        await page.waitForFunction(() => !document.querySelector('.pointer-side-hint'));
      }
      await open({ platform, collapsed: Number(collapsed) });
      await page.locator(separator).focus();
      await inspect(`${platform}-${collapsed ? 'collapsed' : 'expanded'}-keyboard`, { screenshot: true });
      assert.equal(await page.locator(hint).textContent(), await page.locator(separator).getAttribute('data-tip-focus'), 'keyboard focus must show keyboard-specific help');
      await page.keyboard.press('Escape');
      await assertDismissed('keyboard Escape');
      await page.locator('[data-review-focus-target]').focus();
      await page.locator(separator).focus();
      await inspect(`${platform}-${collapsed ? 'collapsed' : 'expanded'}-refocus`);
    }
  }

  // Keyboard focus owns the single tooltip even while the pointer explores a
  // different control. Exercise the actual sidebar collapse button both ways.
  await open({ platform: 'mac', width: 264 });
  const collapseButton = page.getByRole('button', { name: '收起侧边栏', exact: true });
  await collapseButton.focus();
  await moveToHandle(page);
  await assertOnlyTooltip(await collapseButton.getAttribute('data-tip'), 'button focus and divider hover');
  assert.equal(await page.locator(hint).count(), 0, 'focused button should own the generic bubble');
  await page.screenshot({ path: join(output, 'sidebar-resize-focus-button-hover-divider.png') });
  evidence.push({ label: 'mixed-button-focus-divider-hover', text: await page.getByRole('tooltip').textContent() });

  await page.locator(separator).focus();
  await collapseButton.hover();
  await assertOnlyTooltip(await page.locator(separator).getAttribute('data-tip-focus'), 'divider focus and button hover');
  await inspect('mixed-divider-focus-button-hover', { screenshot: true });

  // Dismissing focused help applies to that focus interaction only. Hovering
  // a different control may show its hint without reviving the old one.
  await page.mouse.move(900, 600);
  await page.keyboard.press('Escape');
  await assertDismissed('focused divider Escape before another control hover');
  await collapseButton.hover();
  await assertOnlyTooltip(await collapseButton.getAttribute('data-tip'), 'new button hover after dismissing divider focus');
  assert.equal(await page.locator(hint).count(), 0, 'new hover must not revive dismissed divider focus help');
  await page.screenshot({ path: join(output, 'sidebar-resize-escaped-focus-hover-button.png') });
  evidence.push({ label: 'escaped-divider-focus-button-hover', text: await page.getByRole('tooltip').textContent() });

  // Native title hints must use the same owner arbitration and retain their
  // browser attribute after focus leaves the control.
  const nativeControl = page.locator('[data-review-focus-target]');
  await nativeControl.focus();
  await moveToHandle(page);
  await assertOnlyTooltip('内容区帮助', 'native-title focus and divider hover');
  assert.equal(await nativeControl.getAttribute('title'), null, 'custom native hint should suppress the browser title');
  await page.locator(separator).focus();
  assert.equal(await nativeControl.getAttribute('title'), '内容区帮助', 'native title should restore when inactive');
  await nativeControl.hover();
  await assertOnlyTooltip(await page.locator(separator).getAttribute('data-tip-focus'), 'divider focus and native-title hover');
  await inspect('mixed-divider-focus-native-hover');

  // Escape dismisses until a new hover interaction, even when the pointer
  // moves a little inside the same tall separator.
  await open({ platform: 'mac', width: 264 });
  const escapedPoint = await moveToHandle(page);
  await inspect('escape-hover-before');
  await page.keyboard.press('Escape');
  await assertDismissed('hover Escape');
  await page.mouse.move(escapedPoint.x, escapedPoint.y + 4);
  await page.waitForTimeout(450);
  assert.equal(await page.getByRole('tooltip').count(), 0, 'small pointer movement must not revive an escaped tooltip');
  evidence.push({ label: 'escape-hover-remains-dismissed' });
  await page.mouse.move(900, 600);
  const reenteredPoint = await moveToHandle(page);
  await inspect('escape-hover-reentered', { point: reenteredPoint });

  // A programmatic modal has no initiating click to clear hover help. The
  // production focus manager must remove it before the modal takes over.
  await page.evaluate(() => window.dispatchEvent(new Event('review-open-modal')));
  await page.getByRole('dialog', { name: '验证弹窗' }).waitFor();
  await page.waitForFunction(() => !document.querySelector('[role="tooltip"]'));
  assert.equal(await page.locator(hint).count(), 0, 'stale divider hint must not cover a programmatic modal');
  // Closing an occluding surface can legitimately synthesize pointerover on
  // the newly exposed divider. Move into the dialog first so this assertion
  // checks stale ownership rather than a fresh, browser-generated hover.
  await page.mouse.move(700, 300);
  await page.keyboard.press('Escape');
  await page.getByRole('dialog').waitFor({ state: 'detached' });
  await page.waitForTimeout(450);
  assert.equal(await page.getByRole('tooltip').count(), 0, 'closing a modal must not restore stale hover help');
  await page.mouse.move(900, 600);
  await moveToHandle(page);
  await inspect('programmatic-modal-hover-reentered');

  // Real keyboard actions must keep the correct width/collapse state and the
  // focused keyboard help while the shared tooltip text changes with state.
  await open({ platform: 'mac', width: 264 });
  await page.locator(separator).focus();
  const keyboardBefore = await inspect('keyboard-before-resize');
  await page.keyboard.press('ArrowRight');
  await assertWidth(280, 'ArrowRight expands by one step');
  await assertOnlyTooltip(await page.locator(separator).getAttribute('data-tip-focus'), 'ArrowRight keyboard hint');
  const keyboardAfter = await inspect('keyboard-after-resize', { screenshot: true });
  assert.ok(keyboardAfter.left - keyboardBefore.left >= 15.9, 'keyboard hint must follow the divider by at least the 16px resize step');
  await page.keyboard.press('Shift+ArrowLeft');
  await assertWidth(240, 'Shift+ArrowLeft uses a larger step');
  await page.keyboard.press('ArrowLeft');
  await assertWidth(0, 'ArrowLeft at minimum width collapses');
  await assertOnlyTooltip(await page.locator(separator).getAttribute('data-tip-focus'), 'ArrowLeft collapsed keyboard hint');
  await page.keyboard.press('ArrowRight');
  await assertWidth(240, 'ArrowRight restores the collapsed sidebar');
  await page.keyboard.press('End');
  await assertWidth(400, 'End selects the available maximum width');
  await page.keyboard.press('Home');
  await assertWidth(defaultWidth, 'Home restores the default width');
  await assertOnlyTooltip(await page.locator(separator).getAttribute('data-tip-focus'), 'Home keyboard hint');
  assert.equal(await page.evaluate(() => localStorage.getItem('app-sidebar-width')), String(defaultWidth), 'Home should persist the default width');
  await page.keyboard.press('Enter');
  await assertWidth(0, 'Enter collapses');
  await assertOnlyTooltip(await page.locator(separator).getAttribute('data-tip-focus'), 'Enter collapsed keyboard hint');
  await page.keyboard.press('Enter');
  await assertWidth(defaultWidth, 'Enter restores the preferred width');
  await page.keyboard.press('Space');
  await assertWidth(0, 'Space collapses');
  await page.keyboard.press('Home');
  await assertWidth(defaultWidth, 'Home restores from a collapsed state');
  await inspect('keyboard-actions-complete', { screenshot: true });

  // Beginning a drag transfers ownership from a different focused control to
  // the live width status immediately, before the first pointer move.
  await open({ platform: 'mac', width: 264 });
  await collapseButton.focus();
  const dragFromOtherTip = await moveToHandle(page);
  await assertOnlyTooltip(await collapseButton.getAttribute('data-tip'), 'other tooltip before drag');
  await page.mouse.down();
  await inspect('drag-dismisses-other-focused-tip', { point: dragFromOtherTip, role: 'status', screenshot: true });
  await page.mouse.move(dragFromOtherTip.x + 40, dragFromOtherTip.y + 20, { steps: 3 });
  await inspect('drag-transfers-tooltip-ownership', { role: 'status' });
  await page.keyboard.press('Escape');
  await assertDismissed('drag from other focused tooltip');
  await page.mouse.up();
  await assertWidth(264, 'canceling a drag restores the prior width');

  for (const collapsed of [false, true]) {
    await open({ platform: 'other', collapsed: Number(collapsed), width: 240, font: 28 }, { width: 320, height: 480 });
    const point = await moveToHandle(page, 0.98);
    await inspect(`narrow-large-font-${collapsed ? 'collapsed' : 'expanded'}`, { point, screenshot: true });
  }
  await open({ platform: 'other', width: 240, font: 36 }, { width: 240, height: 480 });
  const wrappedPointer = await moveToHandle(page, 0.98);
  await inspect('narrow-240-font36-wrap', { point: wrappedPointer, screenshot: true });
  await open({ platform: 'mac', theme: 'dark', width: 400 });
  const darkPointer = await moveToHandle(page);
  await inspect('dark-expanded', { point: darkPointer, screenshot: true });

  for (const collapsed of [false, true]) {
    await open({ platform: 'mac', collapsed: Number(collapsed), width: 264 });
    const start = await moveToHandle(page, 0.8);
    await inspect(`drag-${collapsed ? 'collapsed' : 'expanded'}-idle`, { point: start });
    await page.mouse.down();
    const destination = { x: collapsed ? 120 : 80, y: start.y - 90 };
    await page.mouse.move(destination.x, destination.y, { steps: 4 });
    await inspect(`drag-${collapsed ? 'expand' : 'collapse'}`, { point: destination, role: 'status', screenshot: true });
    await page.mouse.up();
    await assertDismissed('pointer release');
    assert.equal(await page.locator('.app__body--collapsed').count(), collapsed ? 0 : 1, 'pointer release should commit collapse/expand');
  }

  for (const cancellation of ['Escape', 'pointercancel', 'blur', 'lostpointercapture']) {
    await open({ platform: 'mac', width: 264 });
    const start = await moveToHandle(page, 0.5);
    await page.mouse.down();
    await page.mouse.move(80, start.y + 50, { steps: 3 });
    await inspect(`cancel-${cancellation}-active`, { role: 'status' });
    if (cancellation === 'Escape') await page.keyboard.press('Escape');
    else if (cancellation === 'blur') await page.evaluate(() => window.dispatchEvent(new Event('blur')));
    else if (cancellation === 'pointercancel') await page.locator(separator).dispatchEvent('pointercancel', { pointerId: 1, isPrimary: true });
    else {
      await page.locator(separator).evaluate(element => element.releasePointerCapture(1));
      // Native lostpointercapture is delivered before the next pointer event.
      await page.mouse.move(81, start.y + 50);
    }
    await assertDismissed(cancellation);
    await page.mouse.up();
    assert.equal(await page.locator('.app__body--collapsed').count(), 0, `${cancellation}: cancellation committed a collapsed state`);
    assert.equal(await page.locator(separator).getAttribute('aria-valuenow'), '264', `${cancellation}: cancellation committed a width`);
  }

  writeFileSync(join(output, 'sidebar-resize-after.json'), JSON.stringify(evidence, null, 2));
  return evidence.length;
}

/** Real double-clicks include pointer capture, click focus and the moving divider. */
export async function validateSidebarResizeDoubleClick(page, output, { baseURL } = {}) {
  const evidence = [];
  const open = async (params = {}, viewportWidth = 1024) => {
    await page.setViewportSize({ width: viewportWidth, height: 720 });
    await page.goto(`${baseURL}/__ui-review?${new URLSearchParams(params)}`);
    await page.locator(separator).waitFor();
  };
  const check = async (width, label) => {
    await page.waitForFunction(({ selector, width }) => document.querySelector(selector)?.getAttribute('aria-valuenow') === String(width), { selector: separator, width });
    const actual = await page.locator('.sidebar').evaluate(element => element.getBoundingClientRect().width);
    assert.ok(Math.abs(actual - width) < 1, `${label}: painted sidebar and accessible width differ`);
    const anchor = await geometry(page, separator);
    assert.ok(Math.abs(anchor.left + anchor.width / 2 - width) <= 4, `${label}: divider no longer follows the sidebar edge`);
    assert.equal(await page.evaluate(() => localStorage.getItem('app-sidebar-width')), String(width), `${label}: width was not persisted`);
    assert.equal(await page.locator('.sidebar-resize--active, .app__body--sidebar-resizing').count(), 0, `${label}: pointer capture left resizing active`);
    assert.equal(await page.locator(hint).count(), 0, `${label}: stale hover/drag help after double-click`);
    assert.equal(await page.locator(separator).evaluate(element => element === document.activeElement), true, `${label}: divider lost keyboard focus`);
    evidence.push({ label, width, anchor });
  };
  const hover = async (text, label) => {
    await page.mouse.move(page.viewportSize().width - 20, 500);
    await page.locator(separator).evaluate(element => element.blur());
    await moveToHandle(page);
    await page.getByRole('tooltip').waitFor();
    assert.equal(await page.getByRole('tooltip').textContent(), text, label);
    assert.equal(await page.getByRole('tooltip').count(), 1, `${label}: duplicate help`);
    assertReadable(await geometry(page, hint), label);
  };
  await page.emulateMedia({ reducedMotion: 'no-preference' });
  try {
    for (const platform of ['mac', 'other']) for (const theme of ['light', 'dark']) for (const font of [16, 22]) {
      const suffix = `${platform}-${theme}-${font}`;
      await open({ platform, theme, font, width: defaultWidth });
      await hover('拖动调整宽度，双击展开到最大宽度', `${suffix}: default help`);
      await page.locator(separator).dblclick();
      await check(400, `${suffix}: default to maximum`);
      await hover('拖动调整宽度，双击恢复默认宽度', `${suffix}: maximum help`);
      await page.screenshot({ path: join(output, `sidebar-double-click-${suffix}-maximum.png`) });
      await page.locator(separator).dblclick();
      await check(defaultWidth, `${suffix}: maximum to default`);
      // Moving the divider must not interfere with subsequent double-clicks.
      for (const width of [400, defaultWidth, 400, defaultWidth]) {
        await page.locator(separator).dblclick();
        await check(width, `${suffix}: repeated toggle to ${width}`);
      }
      // Home remains an unconditional reset even at the default width.
      await page.keyboard.press('Home');
      await page.keyboard.press('Home');
      assert.equal(await page.locator(separator).getAttribute('aria-valuenow'), String(defaultWidth));
      await page.keyboard.press('End');
      assert.equal(await page.locator(separator).getAttribute('aria-valuenow'), '400');
      await page.keyboard.press('Enter');
      assert.equal(await page.locator(separator).getAttribute('aria-valuenow'), '0');
      await hover('向右拖动展开，双击恢复默认宽度', `${suffix}: collapsed help`);
      await page.locator(separator).dblclick();
      await check(defaultWidth, `${suffix}: collapsed custom width to default`);
      await hover('拖动调整宽度，双击展开到最大宽度', `${suffix}: restored help`);
      await page.screenshot({ path: join(output, `sidebar-double-click-${suffix}-default.png`) });
    }
    for (const width of [240, 280, 320, 399]) {
      await open({ platform: 'mac', width });
      await hover('拖动调整宽度，双击恢复默认宽度', `${width}: custom width help`);
      await page.locator(separator).dblclick();
      await check(defaultWidth, `${width}: custom/minimum to default`);
    }
    await open({ platform: 'mac', width: defaultWidth }, 780);
    await page.locator(separator).dblclick();
    await check(300, 'narrow window: maximum preserves main content space');
    assert.ok(await page.locator('.app__main').evaluate(element => element.getBoundingClientRect().width >= 480), 'maximum must reserve the main content minimum');
    await page.locator(separator).dblclick();
    await check(defaultWidth, 'narrow window: restore default');
    for (const viewport of [728, 720]) {
      await open({ platform: 'mac', width: defaultWidth }, viewport);
      const actual = await page.locator(separator).getAttribute('aria-valuenow');
      await hover('拖动调整宽度，双击恢复默认宽度', `${viewport}: no room to expand`);
      await page.locator(separator).dblclick();
      assert.equal(await page.locator(separator).getAttribute('aria-valuenow'), actual);
      assert.equal(await page.evaluate(() => localStorage.getItem('app-sidebar-width')), String(defaultWidth), 'temporary constraints must not overwrite the default preference');
      await page.setViewportSize({ width: 1024, height: 720 });
      await page.waitForFunction(width => document.querySelector('[data-sidebar-resize]')?.getAttribute('aria-valuenow') === String(width), defaultWidth);
      evidence.push({ label: `constrained-${viewport}-restored`, width: defaultWidth });
    }
    // A normal drag must still work immediately after a real double-click.
    await open({ platform: 'mac', width: defaultWidth });
    await page.locator(separator).dblclick();
    const start = await moveToHandle(page);
    await page.mouse.down();
    await page.mouse.move(336, start.y, { steps: 5 });
    await page.mouse.up();
    await check(336, 'drag after double-click');
    await page.locator(separator).dblclick();
    await check(defaultWidth, 'double-click after drag');
    writeFileSync(join(output, 'sidebar-double-click.json'), JSON.stringify(evidence, null, 2));
    return evidence.length;
  } finally {
    await page.emulateMedia({ reducedMotion: 'reduce' });
  }
}

/** Normal motion is the default user path; never freeze away geometry bugs. */
export async function validateSidebarResizeNaturalMotion(page, output, { baseURL } = {}) {
  const evidence = [];
  const inspect = async (label, role = 'tooltip') => {
    await page.locator(hint).waitFor({ state: 'visible' });
    await page.locator(hint).evaluate(async element => {
      await Promise.all(element.getAnimations().map(animation => animation.finished.catch(() => {})));
      await new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve)));
    });
    assert.equal(await page.locator(hint).count(), 1, `${label}: multiple side hints`);
    assert.equal(await page.getByRole('tooltip').count(), role === 'tooltip' ? 1 : 0, `${label}: duplicate tooltip`);
    assert.equal(await page.getByRole('status').count(), role === 'status' ? 1 : 0, `${label}: duplicate drag status`);
    const box = await geometry(page, hint);
    assertReadable(box, label);
    assert.equal(box.role, role, `${label}: wrong accessible role`);
    evidence.push({ label, ...box });
    await page.screenshot({ path: join(output, `sidebar-resize-${label}.png`) });
    return box;
  };
  try {
    await page.emulateMedia({ reducedMotion: 'no-preference' });
    await page.setViewportSize({ width: 1024, height: 720 });
    await page.goto(`${baseURL}/__ui-review?platform=mac&width=264`);
    await page.locator(separator).waitFor();
    assert.equal(await page.evaluate(() => matchMedia('(prefers-reduced-motion: reduce)').matches), false, 'normal-motion checks must use production animations');
    const pointer = await moveToHandle(page);
    const hoveredBox = await inspect('normal-motion-hover');
    assert.ok(Math.hypot(Math.max(hoveredBox.left - pointer.x, pointer.x - hoveredBox.right, 0), Math.max(hoveredBox.top - pointer.y, pointer.y - hoveredBox.bottom, 0)) <= 40,
      'normal-motion hover hint must remain beside the pointer');

    await page.mouse.move(900, 600);
    await page.locator(separator).focus();
    const keyboardBefore = await inspect('normal-motion-keyboard-before');
    await page.keyboard.press('ArrowRight');
    await page.waitForFunction(() => document.querySelector('[data-sidebar-resize]')?.getAttribute('aria-valuenow') === '280');
    const keyboardAfter = await inspect('normal-motion-keyboard-after');
    const anchor = await geometry(page, separator);
    assert.ok(keyboardAfter.left - keyboardBefore.left >= 15.9, 'normal-motion keyboard hint must follow the 16px width change');
    assert.ok(Math.abs(keyboardAfter.left - anchor.right - 14) <= 1, 'normal-motion keyboard hint must preserve its 14px divider gap');
    assert.equal(keyboardAfter.text, await page.locator(separator).getAttribute('data-tip-focus'), 'normal-motion focus must retain keyboard help');

    const start = await moveToHandle(page);
    await page.mouse.down();
    await page.mouse.move(start.x + 32, start.y + 40, { steps: 3 });
    await inspect('normal-motion-drag', 'status');
    await page.mouse.up();
    await page.waitForFunction(() => !document.querySelector('.pointer-side-hint'));
    await page.waitForTimeout(400);
    assert.equal(await page.getByRole('tooltip').count(), 0, 'normal-motion release must not revive a stale tooltip');
    assert.equal(await page.getByRole('status').count(), 0, 'normal-motion release must remove drag status');
    assert.equal(await page.locator('.sidebar-resize--active, .app__body--sidebar-resizing').count(), 0, 'normal-motion release must finish resizing');
    assert.equal(await page.locator(separator).getAttribute('aria-valuenow'), '312', 'normal-motion drag must commit the final width');
    assert.equal(await page.evaluate(() => localStorage.getItem('app-sidebar-width')), '312', 'normal-motion drag must persist the final width');
    evidence.push({ label: 'normal-motion-release-clean', width: 312 });
    writeFileSync(join(output, 'sidebar-resize-natural-motion.json'), JSON.stringify(evidence, null, 2));
    return evidence.length;
  } finally {
    await page.emulateMedia({ reducedMotion: 'reduce' });
  }
}

async function main() {
  const output = reviewOutput(mkdtempSync(join(tmpdir(), 'echo-sidebar-resize-')));
  const server = await createUiReviewServer(Number(process.env.UI_REVIEW_PORT || 1443), { freezeMotion: false, fixture: '/scripts/fixtures/sidebar-resize-review.tsx' });
  const address = server.httpServer.address();
  const baseURL = `http://127.0.0.1:${address.port}`;
  let browser;
  const errors = [];
  try {
    browser = await (await reviewChromium()).launch({ headless: true, executablePath: process.env.CHROME_BIN || '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome' });
    const page = await browser.newPage({ reducedMotion: 'reduce' });
    page.setDefaultTimeout(7000);
    page.setDefaultNavigationTimeout(60000);
    page.on('pageerror', error => errors.push(error.message));
    let results = process.env.UI_REVIEW_BASELINE === '1'
      ? await captureSidebarResizeBaseline(page, output, { baseURL })
      : process.argv.includes('--motion-only') ? 0 : await validateSidebarResizeHint(page, output, { baseURL });
    if (process.env.UI_REVIEW_BASELINE !== '1') {
      if (!process.argv.includes('--motion-only')) results += await validateSidebarResizeDoubleClick(page, output, { baseURL });
      results += await validateSidebarResizeNaturalMotion(page, output, { baseURL });
    }
    assert.deepEqual(errors, [], 'browser fixture emitted application errors');
    console.log(JSON.stringify({ output, checks: Array.isArray(results) ? results.length : results, baseline: process.env.UI_REVIEW_BASELINE === '1' }));
  } catch (error) {
    console.error(error, { browserErrors: errors });
    throw error;
  } finally {
    await browser?.close();
    server.httpServer.closeAllConnections();
    await server.close();
  }
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) await main();
