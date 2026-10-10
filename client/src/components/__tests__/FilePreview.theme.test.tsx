import { act, cleanup, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import mermaid from "mermaid";
import { FilePreview } from "../FilePreview";

vi.mock("mermaid", () => ({
  default: {
    mermaidAPI: { getConfig: vi.fn(() => ({ secure: ["securityLevel"] })) },
    initialize: vi.fn(),
    render: vi.fn(async () => ({ svg: '<svg xmlns="http://www.w3.org/2000/svg" width="300" height="80" viewBox="0 0 300 80"><text x="10" y="30">示例图表</text></svg>' })),
  },
}));

describe("FilePreview diagram theme", () => {
  const originalCreate = Object.getOwnPropertyDescriptor(URL, "createObjectURL");
  const originalRevoke = Object.getOwnPropertyDescriptor(URL, "revokeObjectURL");
  beforeEach(() => {
    Object.defineProperty(URL, "createObjectURL", { configurable: true, value: vi.fn(() => "blob:diagram") });
    Object.defineProperty(URL, "revokeObjectURL", { configurable: true, value: vi.fn() });
  });
  afterEach(() => {
    cleanup();
    document.documentElement.removeAttribute("data-theme");
    if (originalCreate) Object.defineProperty(URL, "createObjectURL", originalCreate);
    else Reflect.deleteProperty(URL, "createObjectURL");
    if (originalRevoke) Object.defineProperty(URL, "revokeObjectURL", originalRevoke);
    else Reflect.deleteProperty(URL, "revokeObjectURL");
    vi.clearAllMocks();
  });

  it("文件内图表遵循当前深色主题，并响应应用主题切换", async () => {
    document.documentElement.dataset.theme = "dark";
    render(<FilePreview filename="diagram.md" content={"```mermaid\ngraph LR\n  A --> B\n```"} />);
    await screen.findByRole("button", { name: "放大预览图表" });
    expect(mermaid.initialize).toHaveBeenCalledWith(expect.objectContaining({ theme: "dark" }));
    await act(async () => { document.documentElement.dataset.theme = "light"; });
    await waitFor(() => expect(mermaid.initialize).toHaveBeenLastCalledWith(expect.objectContaining({ theme: "default" })));
  });
});
