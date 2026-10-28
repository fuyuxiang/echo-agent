import { describe, it, expect, vi } from "vitest";
import { render, screen, fireEvent, waitFor } from "@testing-library/react";
import { ConfirmProvider, useConfirm } from "./ConfirmDialog";

function Harness({ onResult }: { onResult: (v: boolean) => void }) {
  const confirm = useConfirm();
  return (
    <button
      onClick={async () => {
        const ok = await confirm({
          title: "删除文档？",
          message: "不可撤销",
          confirmLabel: "删除",
          destructive: true,
        });
        onResult(ok);
      }}
    >
      开始删除
    </button>
  );
}

function setup() {
  const onResult = vi.fn();
  render(
    <ConfirmProvider>
      <Harness onResult={onResult} />
    </ConfirmProvider>,
  );
  fireEvent.click(screen.getByText("开始删除"));
  return { onResult };
}

describe("ConfirmDialog", () => {
  it("默认不渲染,直到调用 confirm", () => {
    const onResult = vi.fn();
    render(
      <ConfirmProvider>
        <Harness onResult={onResult} />
      </ConfirmProvider>,
    );
    expect(screen.queryByRole("dialog")).toBeNull();
  });

  it("点确认时 resolve 为 true", async () => {
    const { onResult } = setup();
    expect(screen.getByRole("dialog")).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "删除" }));

    await waitFor(() => expect(onResult).toHaveBeenCalledWith(true));
    expect(screen.queryByRole("dialog")).toBeNull();
  });

  it("点取消时 resolve 为 false", async () => {
    const { onResult } = setup();
    fireEvent.click(screen.getByRole("button", { name: "取消" }));

    await waitFor(() => expect(onResult).toHaveBeenCalledWith(false));
  });

  it("Esc 关闭并 resolve 为 false", async () => {
    const { onResult } = setup();
    fireEvent.keyDown(window, { key: "Escape" });

    await waitFor(() => expect(onResult).toHaveBeenCalledWith(false));
    expect(screen.queryByRole("dialog")).toBeNull();
  });

  it("破坏性操作打开时焦点落在取消按钮上", () => {
    // 这里的 harness 是 destructive: true,焦点必须停在安全的一侧,
    // 否则一次误触 Enter 就直接执行删除。
    setup();
    expect(screen.getByRole("button", { name: "取消" })).toHaveFocus();
  });

  it("对话框带 aria-modal 与标题关联", () => {
    setup();
    const dialog = screen.getByRole("dialog");
    expect(dialog).toHaveAttribute("aria-modal", "true");
    expect(dialog).toHaveAccessibleName("删除文档？");
  });

  it("点遮罩层视为取消", async () => {
    const { onResult } = setup();
    // 遮罩是 dialog 的父节点,点它应当取消;点面板本身不应关闭。
    fireEvent.click(screen.getByRole("dialog"));
    expect(onResult).not.toHaveBeenCalled();

    fireEvent.click(screen.getByRole("dialog").parentElement!);
    await waitFor(() => expect(onResult).toHaveBeenCalledWith(false));
  });

  it("useConfirm 在 Provider 外使用时抛出明确错误", () => {
    const spy = vi.spyOn(console, "error").mockImplementation(() => {});
    expect(() => render(<Harness onResult={() => {}} />)).toThrow(/ConfirmProvider/);
    spy.mockRestore();
  });
});
