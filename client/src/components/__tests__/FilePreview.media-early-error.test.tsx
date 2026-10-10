import { render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { FilePreview } from "../FilePreview";

vi.mock("../markdown/MarkdownPreviewImage", async () => {
  const { useLayoutEffect } = await import("react");
  return {
    MarkdownPreviewImage({ onError, alt }: { onError?: (event: unknown) => void; alt?: string }) {
      // Cached invalid resources can fail during commit, before passive effects.
      useLayoutEffect(() => { onError?.({ type: "error" }); }, []);
      return <img alt={alt} />;
    },
  };
});

describe("FilePreview early media failure", () => {
  it("缓存失效图片在mount effect前失败时保留可读错误状态", () => {
    render(<FilePreview filename="invalid.png" content="data:image/png;base64,invalid" />);
    expect(screen.getByRole("alert")).toHaveTextContent("无法加载预览");
    expect(screen.queryByRole("img")).not.toBeInTheDocument();
  });
});
