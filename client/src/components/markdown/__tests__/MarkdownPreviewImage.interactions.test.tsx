import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { MarkdownPreviewImage } from "../MarkdownPreviewImage";

// jsdom has no layout. Model the actual preview's centered canvas and image
// extent so assertions exercise the visible point under the cursor rather
// than private zoom or gesture state.
let viewport = { width: 800, height: 600, left: 100, top: 80 };
const resizeCallbacks = new Set<ResizeObserverCallback>();
const scrollPositions = new WeakMap<HTMLElement, { left: number; top: number }>();
const isStage = (element: HTMLElement) => element.classList.contains("md-image-preview__stage");
const stageImage = (element: HTMLElement) => element.querySelector<HTMLImageElement>(".md-image-preview__canvas img");
function imageExtent(element: HTMLElement) {
  const stage = isStage(element) ? element : element.closest<HTMLElement>(".md-image-preview__stage");
  const image = stage && stageImage(stage);
  return { width: Number.parseFloat(image?.style.width ?? "0") || 0, height: Number.parseFloat(image?.style.height ?? "0") || 0 };
}
function canvasExtent(element: HTMLElement) {
  const image = imageExtent(element);
  return { width: Math.max(viewport.width, image.width + 48), height: Math.max(viewport.height, image.height + 48) };
}
function rect(left: number, top: number, width: number, height: number) {
  return new DOMRect(left, top, width, height);
}
function scrollPosition(element: HTMLElement) {
  let value = scrollPositions.get(element);
  if (!value) {
    value = { left: 0, top: 0 };
    scrollPositions.set(element, value);
  }
  return value;
}
function pointer(target: Element | Window, type: string, x: number, y: number, options: { id?: number; pointerType?: string; button?: number } = {}) {
  const event = new MouseEvent(type, { bubbles: true, cancelable: true, clientX: x, clientY: y, button: options.button ?? 0 });
  Object.defineProperties(event, {
    pointerId: { value: options.id ?? 1 },
    pointerType: { value: options.pointerType ?? "mouse" },
    isPrimary: { value: (options.id ?? 1) === 1 },
  });
  fireEvent(target, event);
  return event;
}
function openPreview(size: { width: number; height: number } | null = { width: 1600, height: 1200 }) {
  render(<MarkdownPreviewImage src="blob:diagram" alt="示例图表" previewTitle="图表预览" intrinsicSize={size ?? undefined} />);
  const trigger = screen.getByRole("button", { name: "放大预览：示例图表" });
  fireEvent.click(trigger);
  const dialog = screen.getByRole("dialog", { name: "图表预览" });
  const stage = dialog.querySelector<HTMLDivElement>(".md-image-preview__stage")!;
  const image = within(dialog).getByRole("img") as HTMLImageElement;
  return { trigger, dialog, stage, image, button: (name: string) => within(dialog).getByRole("button", { name }) };
}
const scaleOf = (image: HTMLImageElement, width = 1600) => Number.parseFloat(image.style.width) / width;
function imagePoint(image: HTMLImageElement, clientX: number, clientY: number) {
  const bounds = image.getBoundingClientRect();
  return { x: (clientX - bounds.left) / bounds.width, y: (clientY - bounds.top) / bounds.height };
}
async function settle() {
  await act(async () => { await new Promise(resolve => setTimeout(resolve, 25)); });
}
function resize(width: number, height: number) {
  viewport = { ...viewport, width, height };
  act(() => { for (const callback of resizeCallbacks) callback([], {} as ResizeObserver); });
}

beforeEach(() => {
  viewport = { width: 800, height: 600, left: 100, top: 80 };
  resizeCallbacks.clear();
  const nativeClientWidth = Object.getOwnPropertyDescriptor(HTMLElement.prototype, "clientWidth")?.get;
  const nativeClientHeight = Object.getOwnPropertyDescriptor(HTMLElement.prototype, "clientHeight")?.get;
  vi.spyOn(HTMLElement.prototype, "clientWidth", "get").mockImplementation(function (this: HTMLElement) { return isStage(this) ? viewport.width : nativeClientWidth?.call(this) ?? 0; });
  vi.spyOn(HTMLElement.prototype, "clientHeight", "get").mockImplementation(function (this: HTMLElement) { return isStage(this) ? viewport.height : nativeClientHeight?.call(this) ?? 0; });
  vi.spyOn(HTMLElement.prototype, "scrollWidth", "get").mockImplementation(function (this: HTMLElement) { return isStage(this) ? canvasExtent(this).width : 0; });
  vi.spyOn(HTMLElement.prototype, "scrollHeight", "get").mockImplementation(function (this: HTMLElement) { return isStage(this) ? canvasExtent(this).height : 0; });
  vi.spyOn(Element.prototype, "scrollLeft", "get").mockImplementation(function (this: Element) { return this instanceof HTMLElement ? scrollPosition(this).left : 0; });
  vi.spyOn(Element.prototype, "scrollTop", "get").mockImplementation(function (this: Element) { return this instanceof HTMLElement ? scrollPosition(this).top : 0; });
  vi.spyOn(Element.prototype, "scrollLeft", "set").mockImplementation(function (this: Element, value: number) {
    if (this instanceof HTMLElement) scrollPosition(this).left = Math.max(0, Math.min(isStage(this) ? canvasExtent(this).width - viewport.width : 0, value));
  });
  vi.spyOn(Element.prototype, "scrollTop", "set").mockImplementation(function (this: Element, value: number) {
    if (this instanceof HTMLElement) scrollPosition(this).top = Math.max(0, Math.min(isStage(this) ? canvasExtent(this).height - viewport.height : 0, value));
  });
  vi.spyOn(Element.prototype, "getBoundingClientRect").mockImplementation(function (this: Element) {
    if (!(this instanceof HTMLElement)) return rect(0, 0, 0, 0);
    if (isStage(this)) return rect(viewport.left, viewport.top, viewport.width, viewport.height);
    const stage = this.closest<HTMLElement>(".md-image-preview__stage");
    if (!stage) return rect(0, 0, 0, 0);
    const extent = canvasExtent(stage);
    if (this instanceof HTMLImageElement) {
      const image = imageExtent(stage);
      return rect(viewport.left + (extent.width - image.width) / 2 - stage.scrollLeft, viewport.top + (extent.height - image.height) / 2 - stage.scrollTop, image.width, image.height);
    }
    return rect(viewport.left - stage.scrollLeft, viewport.top - stage.scrollTop, extent.width, extent.height);
  });
  vi.stubGlobal("ResizeObserver", class {
    constructor(private readonly callback: ResizeObserverCallback) { resizeCallbacks.add(callback); }
    observe() {}
    unobserve() {}
    disconnect() { resizeCallbacks.delete(this.callback); }
  });
});

afterEach(() => { cleanup(); vi.restoreAllMocks(); vi.unstubAllGlobals(); });

describe("图片和 Mermaid 共享预览手势", () => {
  it("打开即聚焦画布并支持键盘，Tab 可切换到工具栏且不会离开对话框", async () => {
    const user = userEvent.setup();
    const { stage, image, button } = openPreview();
    expect(stage).toHaveFocus();
    fireEvent.keyDown(document.activeElement!, { key: "1" });
    expect(scaleOf(image)).toBe(1);
    await user.tab();
    expect(button("缩小图片")).toHaveFocus();
    await user.tab({ shift: true });
    expect(stage).toHaveFocus();
    const top = stage.scrollTop;
    fireEvent.keyDown(document.activeElement!, { key: "ArrowDown" });
    expect(stage.scrollTop).toBeGreaterThan(top);
  });

  it("只在预览画布拦截滚轮，并围绕指针所指内容缩放", async () => {
    const { trigger, stage, image } = openPreview();
    const initial = scaleOf(image);
    const before = imagePoint(image, 630, 400);
    expect(fireEvent.wheel(trigger, { deltaY: -160, clientX: 630, clientY: 400 })).toBe(true);
    expect(scaleOf(image)).toBe(initial);
    expect(fireEvent.wheel(stage, { deltaY: -160, clientX: 630, clientY: 400 })).toBe(false);
    await waitFor(() => expect(scaleOf(image)).toBeGreaterThan(initial));
    const after = imagePoint(image, 630, 400);
    expect(after.x).toBeCloseTo(before.x, 2);
    expect(after.y).toBeCloseTo(before.y, 2);
    const enlarged = scaleOf(image);
    fireEvent.wheel(stage, { deltaY: 80, clientX: 630, clientY: 400 });
    await waitFor(() => expect(scaleOf(image)).toBeLessThan(enlarged));
  });

  it("Shift 滚轮平移；切换滚轮模式后普通滚轮平移，Ctrl 和 Meta 仍可缩放", async () => {
    const { stage, image, button } = openPreview();
    fireEvent.click(button("原始大小"));
    const left = stage.scrollLeft;
    fireEvent.wheel(stage, { deltaY: 90, shiftKey: true });
    await settle();
    expect(scaleOf(image)).toBe(1);
    expect(stage.scrollLeft).toBeGreaterThan(left);
    fireEvent.click(button("切换为滚轮平移"));
    const top = stage.scrollTop;
    fireEvent.wheel(stage, { deltaY: 90 });
    await settle();
    expect(stage.scrollTop).toBeGreaterThan(top);
    expect(scaleOf(image)).toBe(1);
    fireEvent.wheel(stage, { deltaY: -100, ctrlKey: true, clientX: 500, clientY: 380 });
    await waitFor(() => expect(scaleOf(image)).toBeGreaterThan(1));
    const zoomed = scaleOf(image);
    fireEvent.wheel(stage, { deltaY: 60, metaKey: true, clientX: 500, clientY: 380 });
    await waitFor(() => expect(scaleOf(image)).toBeLessThan(zoomed));
    expect(button("切换为滚轮缩放")).toBeInTheDocument();
  });

  it("双击查看所指位置的细节，再次双击恢复完整图表", async () => {
    const { stage, image, button } = openPreview();
    const initial = scaleOf(image);
    const before = imagePoint(image, 630, 400);
    fireEvent.doubleClick(image, { clientX: 630, clientY: 400 });
    await waitFor(() => expect(scaleOf(image)).toBe(1));
    expect(imagePoint(image, 630, 400).x).toBeCloseTo(before.x, 2);
    expect(imagePoint(image, 630, 400).y).toBeCloseTo(before.y, 2);
    fireEvent.doubleClick(stage, { clientX: 630, clientY: 400 });
    await waitFor(() => expect(scaleOf(image)).toBe(initial));
    expect(stage.scrollLeft).toBe(0);
    expect(stage.scrollTop).toBe(0);
    expect(button("适应窗口")).toHaveAttribute("aria-pressed", "true");
  });

  it("适应窗口已是原始大小的小图仍能通过双击查看细节", async () => {
    const { image } = openPreview({ width: 300, height: 200 });
    expect(scaleOf(image, 300)).toBe(1);
    fireEvent.doubleClick(image, { clientX: 500, clientY: 380 });
    await waitFor(() => expect(scaleOf(image, 300)).toBe(2));
    fireEvent.doubleClick(image, { clientX: 500, clientY: 380 });
    await waitFor(() => expect(scaleOf(image, 300)).toBe(1));
  });

  it("放大后拖动整图，限制边界，松开后不继续移动", async () => {
    const { stage, image, button } = openPreview();
    fireEvent.click(button("原始大小"));
    const start = { left: stage.scrollLeft, top: stage.scrollTop };
    pointer(image, "pointerdown", 500, 380);
    pointer(stage, "pointermove", 380, 300);
    await settle();
    expect(stage.scrollLeft).toBeCloseTo(start.left + 120);
    expect(stage.scrollTop).toBeCloseTo(start.top + 80);
    expect(stage).toHaveFocus();
    pointer(stage, "pointermove", -5000, -5000);
    await settle();
    expect(stage.scrollLeft).toBe(stage.scrollWidth - stage.clientWidth);
    expect(stage.scrollTop).toBe(stage.scrollHeight - stage.clientHeight);
    pointer(stage, "pointerup", -5000, -5000);
    const end = stage.scrollLeft;
    pointer(stage, "pointermove", 500, 380);
    await settle();
    expect(stage.scrollLeft).toBe(end);
    pointer(stage, "pointerdown", 500, 380);
    pointer(stage, "pointermove", 5000, 5000);
    await settle();
    expect(stage.scrollLeft).toBe(0);
    expect(stage.scrollTop).toBe(0);
    pointer(stage, "pointerup", 5000, 5000);
  });

  it.each(["pointercancel", "lostpointercapture", "blur"])("右键不会开始拖动，%s 后不会残留拖动状态", async (reason) => {
    const { stage, button } = openPreview();
    fireEvent.click(button("原始大小"));
    const start = stage.scrollLeft;
    pointer(stage, "pointerdown", 500, 380, { button: 2 });
    pointer(stage, "pointermove", 300, 200, { button: 2 });
    await settle();
    expect(stage.scrollLeft).toBe(start);
    pointer(stage, "pointerdown", 500, 380);
    if (reason === "blur") fireEvent.blur(window);
    else pointer(stage, reason, 500, 380);
    pointer(stage, "pointermove", 300, 200);
    await settle();
    expect(stage.scrollLeft).toBe(start);
  });

  it("图表完全放得下时不能拖出画布", async () => {
    const { stage, image } = openPreview();
    const initial = scaleOf(image);
    pointer(stage, "pointerdown", 500, 380);
    pointer(stage, "pointermove", -1000, 1600);
    await settle();
    expect(stage.scrollLeft).toBe(0);
    expect(stage.scrollTop).toBe(0);
    expect(scaleOf(image)).toBe(initial);
    pointer(stage, "pointerup", -1000, 1600);
  });

  it("键盘可缩放、平移和复位，Escape 关闭后恢复调用入口焦点", async () => {
    const { trigger, stage, image } = openPreview();
    stage.focus();
    fireEvent.keyDown(stage, { key: "1" });
    expect(scaleOf(image)).toBe(1);
    const top = stage.scrollTop;
    fireEvent.keyDown(stage, { key: "ArrowDown" });
    expect(stage.scrollTop).toBeGreaterThan(top);
    fireEvent.keyDown(stage, { key: "+" });
    expect(scaleOf(image)).toBeGreaterThan(1);
    fireEvent.keyDown(stage, { key: "-" });
    expect(scaleOf(image)).toBeCloseTo(1);
    fireEvent.keyDown(stage, { key: "0" });
    expect(scaleOf(image)).toBeCloseTo(0.46);
    fireEvent.keyDown(stage, { key: "Escape" });
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
    expect(trigger).toHaveFocus();
    await settle();
  });

  it("在倍率上限和下限处停止缩放，按钮与手势保持一致", async () => {
    const { stage, image, button } = openPreview();
    fireEvent.click(button("原始大小"));
    for (let index = 0; index < 12; index++) fireEvent.click(button("放大图片"));
    expect(scaleOf(image)).toBe(4);
    expect(button("放大图片")).toBeDisabled();
    fireEvent.wheel(stage, { deltaY: -1000, clientX: 500, clientY: 380 });
    await settle();
    expect(scaleOf(image)).toBe(4);
    for (let index = 0; index < 30; index++) fireEvent.click(button("缩小图片"));
    expect(scaleOf(image)).toBe(0.02);
    expect(button("缩小图片")).toBeDisabled();
    fireEvent.wheel(stage, { deltaY: 1000, clientX: 500, clientY: 380 });
    await settle();
    expect(scaleOf(image)).toBe(0.02);
  });

  it("超宽图的适应倍率低于常规下限时，缩小操作不会反而放大", async () => {
    const { stage, image } = openPreview({ width: 100000, height: 200 });
    const initial = scaleOf(image, 100000);
    expect(initial).toBeLessThan(0.02);
    fireEvent.wheel(stage, { deltaY: 500, clientX: 500, clientY: 380 });
    await settle();
    expect(scaleOf(image, 100000)).toBeLessThanOrEqual(initial);
    expect(stage.scrollLeft).toBe(0);
  });

  it("普通图片读取加载后的自然尺寸后具有同样的预览交互", async () => {
    const { stage, image, button } = openPreview(null);
    Object.defineProperties(image, { naturalWidth: { value: 1600 }, naturalHeight: { value: 1200 } });
    fireEvent.load(image);
    expect(scaleOf(image)).toBeCloseTo(0.46);
    fireEvent.doubleClick(image, { clientX: 500, clientY: 380 });
    expect(scaleOf(image)).toBe(1);
    const current = scaleOf(image);
    fireEvent.wheel(stage, { deltaY: -100, clientX: 500, clientY: 380 });
    await waitFor(() => expect(scaleOf(image)).toBeGreaterThan(current));
    fireEvent.click(button("适应窗口"));
    expect(scaleOf(image)).toBeCloseTo(0.46);
  });

  it.each([
    { width: 0, height: 200 },
    { width: -300, height: 200 },
    { width: Number.NaN, height: 200 },
    { width: 300, height: Number.POSITIVE_INFINITY },
  ])("非法 intrinsicSize %j 会回退到图片自然尺寸", (size) => {
    const { image, button } = openPreview(size);
    expect(button("放大图片")).toBeDisabled();
    Object.defineProperties(image, { naturalWidth: { value: 500 }, naturalHeight: { value: 250 } });
    fireEvent.load(image);
    expect(image).toHaveStyle({ width: "500px", height: "250px" });
    expect(button("放大图片")).toBeEnabled();
  });

  it("连续滚轮事件累计缩放且不会丢失鼠标指向的节点", async () => {
    const { stage, image, button } = openPreview();
    const initial = scaleOf(image);
    fireEvent.wheel(stage, { deltaY: -40, clientX: 570, clientY: 400 });
    await settle();
    const factor = scaleOf(image) / initial;
    fireEvent.click(button("适应窗口"));
    const point = imagePoint(image, 570, 400);
    act(() => {
      for (let index = 0; index < 3; index++) fireEvent.wheel(stage, { deltaY: -40, clientX: 570, clientY: 400 });
    });
    await settle();
    expect(scaleOf(image)).toBeCloseTo(initial * factor ** 3, 4);
    expect(imagePoint(image, 570, 400).x).toBeCloseTo(point.x, 2);
    expect(imagePoint(image, 570, 400).y).toBeCloseTo(point.y, 2);
  });

  it("同一批连续滚轮达到 400% 后仍保留指针所指的节点", async () => {
    const { stage, image } = openPreview();
    const point = imagePoint(image, 570, 400);
    act(() => {
      for (let index = 0; index < 30; index++) {
        fireEvent.wheel(stage, { deltaY: -240, clientX: 570, clientY: 400 });
      }
    });
    await settle();
    expect(scaleOf(image)).toBe(4);
    expect(imagePoint(image, 570, 400).x).toBeCloseTo(point.x, 2);
    expect(imagePoint(image, 570, 400).y).toBeCloseTo(point.y, 2);
  });

  it("滚动条区域保留原生操作，不会被图表拖动手势拦截", async () => {
    const { stage, button } = openPreview();
    fireEvent.click(button("原始大小"));
    const down = pointer(stage, "pointerdown", viewport.left + viewport.width + 5, 380);
    expect(down.defaultPrevented).toBe(false);
    const scroll = { left: stage.scrollLeft, top: stage.scrollTop };
    pointer(window, "pointermove", 600, 200);
    await settle();
    expect(stage.scrollLeft).toBe(scroll.left);
    expect(stage.scrollTop).toBe(scroll.top);
    pointer(window, "pointerup", 600, 200);
    stage.scrollTop = 180;
    fireEvent.scroll(stage);
    expect(stage.scrollTop).toBe(180);
  });

  it("Safari 捏合与同时到达的 Ctrl 滚轮不会重复计算缩放", async () => {
    const { stage, image, button } = openPreview();
    fireEvent.click(button("原始大小"));
    const gesture = (type: string, scale: number) => {
      const event = new Event(type, { bubbles: true, cancelable: true });
      Object.defineProperties(event, { scale: { value: scale }, clientX: { value: 500 }, clientY: { value: 380 } });
      fireEvent(stage, event);
    };
    pointer(stage, "pointerdown", 400, 380, { id: 1, pointerType: "touch" });
    pointer(stage, "pointerdown", 600, 380, { id: 2, pointerType: "touch" });
    gesture("gesturestart", 1);
    gesture("gesturechange", 1.5);
    await settle();
    expect(scaleOf(image)).toBeCloseTo(1.5);
    pointer(stage, "pointermove", 700, 380, { id: 2, pointerType: "touch" });
    await settle();
    expect(scaleOf(image)).toBeCloseTo(1.5);
    fireEvent.wheel(stage, { deltaY: -100, ctrlKey: true, clientX: 500, clientY: 380 });
    await settle();
    expect(scaleOf(image)).toBeCloseTo(1.5);
    gesture("gesturechange", 2);
    await settle();
    expect(scaleOf(image)).toBeCloseTo(2);
    gesture("gestureend", 2);
    pointer(stage, "pointerup", 700, 380, { id: 2, pointerType: "touch" });
    pointer(stage, "pointerup", 400, 380, { id: 1, pointerType: "touch" });
    fireEvent.wheel(stage, { deltaY: -40, ctrlKey: true, clientX: 500, clientY: 380 });
    await waitFor(() => expect(scaleOf(image)).toBeGreaterThan(2));
  });

  it("拖拽后复位图表，再次双击仍可查看细节", async () => {
    const { stage, image, button } = openPreview();
    fireEvent.click(button("原始大小"));
    pointer(stage, "pointerdown", 500, 380);
    pointer(stage, "pointermove", 400, 300);
    pointer(stage, "pointerup", 400, 300);
    fireEvent.click(button("适应窗口"));
    pointer(stage, "pointerdown", 500, 380);
    pointer(stage, "pointerup", 500, 380);
    fireEvent.doubleClick(stage, { clientX: 500, clientY: 380 });
    await waitFor(() => expect(scaleOf(image)).toBe(1));
  });

  it("拖拽不会误关闭预览，随后单独点击遮罩仍能关闭", async () => {
    const { stage, dialog, button } = openPreview();
    fireEvent.click(button("原始大小"));
    const overlay = dialog.parentElement!;
    pointer(stage, "pointerdown", 500, 380);
    pointer(stage, "pointermove", 20, 20);
    pointer(window, "pointerup", 20, 20);
    fireEvent.click(overlay);
    expect(dialog).toBeInTheDocument();
    pointer(overlay, "pointerdown", 20, 20);
    pointer(overlay, "pointerup", 20, 20);
    fireEvent.click(overlay);
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
    await settle();
  });

  it("预览期间图片 src 更新会清除旧错误和缩放状态，并正确恢复入口焦点", () => {
    const { rerender } = render(<MarkdownPreviewImage src="blob:first" alt="示例图表" intrinsicSize={{ width: 1600, height: 1200 }} />);
    const trigger = screen.getByRole("button", { name: "放大预览：示例图表" });
    fireEvent.click(trigger);
    fireEvent.click(screen.getByRole("button", { name: "原始大小" }));
    fireEvent.error(within(screen.getByRole("dialog")).getByRole("img"));
    expect(screen.getByRole("alert")).toBeInTheDocument();
    rerender(<MarkdownPreviewImage src="blob:second" alt="示例图表" intrinsicSize={{ width: 800, height: 400 }} />);
    const dialog = screen.getByRole("dialog");
    const image = within(dialog).getByRole("img");
    expect(image).toHaveAttribute("src", "blob:second");
    expect(image).toHaveStyle({ width: "752px", height: "376px" });
    expect(screen.queryByRole("alert")).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "适应窗口" })).toHaveAttribute("aria-pressed", "true");
    fireEvent.keyDown(dialog, { key: "Escape" });
    expect(trigger).toHaveFocus();
  });

  it("窗口变化后自动适应大小；手动放大保留倍率和当前中心内容", async () => {
    const { stage, image, button } = openPreview();
    expect(scaleOf(image)).toBeCloseTo(0.46);
    resize(640, 480);
    await waitFor(() => expect(scaleOf(image)).toBeCloseTo(0.36));
    fireEvent.click(button("原始大小"));
    stage.scrollLeft = 280;
    stage.scrollTop = 210;
    fireEvent.scroll(stage);
    const center = imagePoint(image, viewport.left + viewport.width / 2, viewport.top + viewport.height / 2);
    resize(900, 680);
    await settle();
    expect(scaleOf(image)).toBe(1);
    const after = imagePoint(image, viewport.left + viewport.width / 2, viewport.top + viewport.height / 2);
    expect(after.x).toBeCloseTo(center.x, 2);
    expect(after.y).toBeCloseTo(center.y, 2);
    fireEvent.click(button("适应窗口"));
    expect(scaleOf(image)).toBeCloseTo((680 - 48) / 1200);
    expect(stage.scrollLeft).toBe(0);
    expect(stage.scrollTop).toBe(0);
  });

  it("窗口放大先收紧原生滚动边界时，仍保留放大前正在看的中心内容", async () => {
    const { stage, image, button } = openPreview();
    fireEvent.click(button("原始大小"));
    stage.scrollLeft = 700;
    stage.scrollTop = 500;
    fireEvent.scroll(stage);
    const center = imagePoint(image, viewport.left + viewport.width / 2, viewport.top + viewport.height / 2);
    // Browsers can clamp scroll offsets as soon as viewport dimensions change,
    // and emit a scroll event before ResizeObserver measures the new stage.
    viewport = { ...viewport, width: 1000, height: 800 };
    stage.scrollLeft = stage.scrollLeft;
    stage.scrollTop = stage.scrollTop;
    expect(stage.scrollLeft).toBe(648);
    expect(stage.scrollTop).toBe(448);
    fireEvent.scroll(stage);
    act(() => { for (const callback of resizeCallbacks) callback([], {} as ResizeObserver); });
    await settle();
    expect(scaleOf(image)).toBe(1);
    const after = imagePoint(image, viewport.left + viewport.width / 2, viewport.top + viewport.height / 2);
    expect(after.x).toBeCloseTo(center.x, 2);
    expect(after.y).toBeCloseTo(center.y, 2);
  });

  it("触屏双指缩放，结束一根手指后可继续单指平移", async () => {
    const { stage, image, button } = openPreview();
    fireEvent.click(button("原始大小"));
    pointer(stage, "pointerdown", 400, 380, { id: 1, pointerType: "touch" });
    pointer(stage, "pointerdown", 600, 380, { id: 2, pointerType: "touch" });
    pointer(stage, "pointermove", 700, 380, { id: 2, pointerType: "touch" });
    await settle();
    expect(scaleOf(image)).toBeCloseTo(1.5);
    pointer(stage, "pointerup", 700, 380, { id: 2, pointerType: "touch" });
    const left = stage.scrollLeft;
    pointer(stage, "pointermove", 350, 380, { id: 1, pointerType: "touch" });
    await settle();
    expect(stage.scrollLeft).toBeGreaterThan(left);
    expect(scaleOf(image)).toBeCloseTo(1.5);
    pointer(stage, "pointerup", 350, 380, { id: 1, pointerType: "touch" });
  });

  it("图片加载失败时给出明确反馈并禁用缩放，仍可关闭预览", async () => {
    const { trigger, stage, image, button } = openPreview();
    fireEvent.error(image);
    expect(screen.getByRole("alert")).toHaveTextContent("图片无法加载");
    expect(button("放大图片")).toBeDisabled();
    expect(button("缩小图片")).toBeDisabled();
    expect(button("适应窗口")).toBeDisabled();
    expect(button("原始大小")).toBeDisabled();
    fireEvent.wheel(stage, { deltaY: -100, clientX: 500, clientY: 380 });
    fireEvent.doubleClick(stage, { clientX: 500, clientY: 380 });
    fireEvent.keyDown(stage, { key: "+" });
    pointer(stage, "pointerdown", 500, 380);
    pointer(stage, "pointermove", 300, 200);
    await settle();
    expect(screen.getByRole("alert")).toHaveTextContent("图片无法加载");
    expect(stage.scrollLeft).toBe(0);
    fireEvent.click(button("关闭图片预览"));
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
    expect(trigger).toHaveFocus();
  });
});
