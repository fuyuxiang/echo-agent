import { describe, it, expect } from "vitest";
import {
  lastSegment,
  deriveBrowserLabel,
  deriveActiveTab,
  type UseUnifiedTabsOptions,
} from "../use-unified-tabs";
import type { SessionArtifact } from "../session-artifacts";
import type { FileChange } from "../file-changes";

const noop = () => {};
function baseOptions(
  over: Partial<UseUnifiedTabsOptions> = {},
): UseUnifiedTabsOptions {
  return {
    resetKey: "s1",
    enabled: true,
    currentView: "artifacts",
    artifacts: [],
    changes: [],
    onViewChange: noop,
    onArtifactSelect: noop,
    ...over,
  };
}

const artifact = (id: string, path: string): SessionArtifact => ({
  id,
  path,
  kind: "file",
  title: id,
  toolCallId: "t",
  status: "completed",
});

const change = (path: string): FileChange => ({
  path,
  name: path.split(/[\\/]/).pop() ?? path,
  added: 1,
  removed: 0,
  approximate: false,
  ext: "ts",
  edits: 1,
});

describe("lastSegment", () => {
  it("returns last segment for unix paths", () => {
    expect(lastSegment("/a/b/c.ts")).toBe("c.ts");
  });
  it("normalizes backslashes", () => {
    expect(lastSegment("C:\\proj\\src\\foo.ts")).toBe("foo.ts");
  });
  it("returns whole string when no separator", () => {
    expect(lastSegment("readme.md")).toBe("readme.md");
  });
  it("returns empty string for undefined", () => {
    expect(lastSegment(undefined)).toBe("");
  });
});

describe("deriveBrowserLabel", () => {
  it("uses filename from path", () => {
    expect(deriveBrowserLabel("https://example.com/docs/intro.html")).toBe(
      "intro.html",
    );
  });
  it("falls back to host for root path", () => {
    expect(deriveBrowserLabel("https://example.com/")).toBe("example.com");
  });
  it("decodes percent-encoding", () => {
    expect(deriveBrowserLabel("https://x.io/%E4%B8%AD.md")).toBe("中.md");
  });
});

describe("deriveActiveTab", () => {
  it("uses one tab identity when the directory and artifact list open the same file", () => {
    const options = baseOptions({artifacts:[artifact("/work/a.md","/work/a.md")],selectedArtifactId:"/work/a.md"});
    expect(deriveActiveTab(options)?.id).toBe(deriveActiveTab({...options,currentView:"fileTree",selectedFilePath:"/work/a.md"})?.id);
  });

  it("derives artifact tab from selectedArtifactId", () => {
    const a = artifact("a1", "/p/foo.ts");
    const tab = deriveActiveTab(
      baseOptions({
        currentView: "artifacts",
        selectedArtifactId: "a1",
        artifacts: [a],
      }),
    );
    expect(tab).not.toBeNull();
    expect(tab?.kind).toBe("artifact");
    expect(tab?.artifactId).toBe("a1");
    expect(tab?.viewWhenActive).toBe("artifacts");
  });

  it("returns null when selected artifact missing from list", () => {
    const tab = deriveActiveTab(
      baseOptions({
        currentView: "artifacts",
        selectedArtifactId: "ghost",
        artifacts: [],
      }),
    );
    expect(tab).toBeNull();
  });

  it("derives file tab from selectedFilePath", () => {
    const tab = deriveActiveTab(
      baseOptions({
        currentView: "fileTree",
        selectedFilePath: "/p/src/main.ts",
      }),
    );
    expect(tab?.kind).toBe("file");
    expect(tab?.label).toBe("main.ts");
    expect(tab?.filePath).toBe("/p/src/main.ts");
  });

  it("derives preview tab from browserUrl", () => {
    const tab = deriveActiveTab(
      baseOptions({
        currentView: "browser",
        browserUrl: "https://example.com/page",
      }),
    );
    expect(tab?.kind).toBe("browser");
    expect(tab?.browserUrl).toBe("https://example.com/page");
    expect(tab?.id).toBe("preview:current");
  });

  it("derives changes tab from selectedArtifactId matching change path", () => {
    const c = change("/p/x.ts");
    const tab = deriveActiveTab(
      baseOptions({
        currentView: "changes",
        selectedArtifactId: "/p/x.ts",
        changes: [c],
      }),
    );
    expect(tab?.kind).toBe("changes");
    expect(tab?.artifactId).toBe("/p/x.ts");
  });

  it("returns null when no selection", () => {
    expect(deriveActiveTab(baseOptions({ currentView: "artifacts" }))).toBeNull();
    expect(deriveActiveTab(baseOptions({ currentView: "fileTree" }))).toBeNull();
    expect(deriveActiveTab(baseOptions({ currentView: "browser" }))).toBeNull();
  });
});
