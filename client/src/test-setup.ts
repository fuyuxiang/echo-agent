// vitest 全局 setup:为每个测试注册 jest-dom matchers 与 DOM cleanup。
// testing-library v16 在检测到 vitest afterEach 时会自动 cleanup,
// 但显式导入确保跨版本一致。
import "@testing-library/jest-dom/vitest";
import { configure } from "@testing-library/react";

// Give DOM updates time to commit on shared CI runners, including when a
// component's first render needs to load a lazy module.
configure({ asyncUtilTimeout: 3000 });

// monaco-editor 0.52 的 editor.main.js 间接加载 editor.all.js,
// 后者在模块顶层就调用 document.queryCommandSupported 探测剪贴板命令。
// jsdom 没有这个 API,加载 monaco 会抛 TypeError,补 noop 让探测返回 false。
if (typeof document !== "undefined" && typeof document.queryCommandSupported !== "function") {
  document.queryCommandSupported = () => false;
}

// monaco-editor 0.52 的 StandaloneThemeService 在构造期与延迟回调里都会
// 读取 mainWindow.matchMedia(forced-colors) 探测系统高对比度;jsdom 没有
// 这个 API,直接抛 TypeError 变成 unhandled rejection。补一个永远
// matches=false 的 stub,Monaco 就不会再触发。
if (typeof window !== "undefined" && typeof window.matchMedia !== "function") {
  window.matchMedia = (query: string) => ({
    matches: false,
    media: query,
    onchange: null,
    addListener: () => {},
    removeListener: () => {},
    addEventListener: () => {},
    removeEventListener: () => {},
    dispatchEvent: () => false,
  });
}
