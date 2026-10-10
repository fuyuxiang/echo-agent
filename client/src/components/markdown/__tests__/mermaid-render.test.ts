import { beforeEach, describe, expect, it, vi } from "vitest";
import mermaid from "mermaid";
import { renderStandaloneMermaid } from "../mermaid-render";

vi.mock("mermaid", () => ({ default: {
  initialize: vi.fn(),
  render: vi.fn(),
  registerLayoutLoaders: vi.fn(),
  mermaidAPI: { getConfig: () => ({ secure: ["securityLevel"] }) },
} }));

const svg = '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 100 80"></svg>';
beforeEach(() => {
  vi.clearAllMocks();
  vi.mocked(mermaid.render).mockResolvedValue({ svg, diagramType: "flowchart-v2" });
});

describe("Mermaid render configuration isolation", () => {
  it("keeps concurrent light/dark diagrams paired with their own configuration", async () => {
    let release!: (value: { svg: string; diagramType: string }) => void;
    const pending = new Promise<{ svg: string; diagramType: string }>((resolve) => { release = resolve; });
    const firstStarted = new Promise<void>((resolve) => {
      vi.mocked(mermaid.render).mockImplementationOnce(() => { resolve(); return pending; });
    });
    const first = renderStandaloneMermaid("light", "flowchart LR\nA-->B", "light");
    const second = renderStandaloneMermaid("dark", "mindmap\nroot\n  child", "dark");
    await firstStarted;
    expect(mermaid.initialize).toHaveBeenCalledTimes(1);
    expect(mermaid.initialize).toHaveBeenLastCalledWith(expect.objectContaining({ theme: "default", htmlLabels: false }));
    release({ svg, diagramType: "flowchart-v2" });
    await Promise.all([first, second]);
    expect(mermaid.initialize).toHaveBeenLastCalledWith(expect.objectContaining({
      layout: "echo-mindmap", htmlLabels: true, themeVariables: expect.objectContaining({ darkMode: true }),
    }));
    expect(mermaid.render).toHaveBeenNthCalledWith(2, "dark", "mindmap\nroot\n  child", expect.any(HTMLDivElement));
  });

  it("continues rendering after an invalid graph fails", async () => {
    vi.mocked(mermaid.render).mockRejectedValueOnce(new Error("parse error"));
    const invalid = renderStandaloneMermaid("invalid", "not a diagram", "light");
    const valid = renderStandaloneMermaid("valid", "flowchart LR\nA-->B", "light");
    await expect(invalid).rejects.toThrow("parse error");
    await expect(valid).resolves.toContain('width="100"');
    expect(mermaid.render).toHaveBeenCalledTimes(2);
    for (const call of vi.mocked(mermaid.render).mock.calls) {
      expect(call[2]?.isConnected).toBe(false);
    }
  });
});
