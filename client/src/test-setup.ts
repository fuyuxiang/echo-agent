// vitest 全局 setup:为每个测试注册 jest-dom matchers 与 DOM cleanup。
// testing-library v16 在检测到 vitest afterEach 时会自动 cleanup,
// 但显式导入确保跨版本一致。
import "@testing-library/jest-dom/vitest";
import { configure } from "@testing-library/react";

// Give DOM updates time to commit on shared CI runners, including when a
// component's first render needs to load a lazy module.
configure({ asyncUtilTimeout: 3000 });

// Some components query media preferences during rendering.
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
