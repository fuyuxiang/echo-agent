import { describe, expect, it } from "vitest";
import {
  createMermaidConfig,
  detectMermaidDiagramKind,
  normalizeStandaloneMermaidSvg,
} from "../mermaid-svg";

describe("Mermaid standalone SVG", () => {
  it("detects diagram kinds after init directives and comments", () => {
    expect(detectMermaidDiagramKind('%%{init: {"theme":"neutral"}}%%\n%% comment\nmindmap')).toBe("mindmap");
    expect(detectMermaidDiagramKind('---\nconfig:\n  theme: neutral\n---\n%% comment\n%%{init: {"fontSize": 18}}%%\nmindmap')).toBe("mindmap");
    expect(detectMermaidDiagramKind("flowchart LR\nA-->B")).toBe("flowchart");
    expect(detectMermaidDiagramKind("sequenceDiagram\nA->>B: hello")).toBe("other");
  });

  it("uses concrete dimensions for standalone images", () => {
    const source = '<svg width="100%" style="max-width: 2400px;" viewBox="-8 -8 2400 400" xmlns="http://www.w3.org/2000/svg"></svg>';
    const normalized = normalizeStandaloneMermaidSvg(source);
    expect(normalized).toContain('width="2400"');
    expect(normalized).toContain('height="400"');
    expect(normalized).toContain('preserveAspectRatio="xMidYMid meet"');
    expect(normalized).not.toContain("max-width: 2400px");
  });

  it("keeps mindmap labels wrapped and disables responsive sizing", () => {
    const config = createMermaidConfig("mindmap", "light", ["securityLevel"]);
    expect(config.htmlLabels).toBe(true);
    expect(config.useMaxWidth).toBe(false);
    expect(config.mindmap).toEqual({ useMaxWidth: false, maxNodeWidth: 240, padding: 12 });
    expect(config.layout).toBe("echo-mindmap");
    expect(config.secure).toEqual(expect.arrayContaining(["securityLevel", "htmlLabels", "fontFamily", "useMaxWidth"]));
  });
});
