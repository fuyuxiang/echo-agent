import { useState } from "react";
import { act, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { useAppDialog } from "../AppDialog";

function ConfirmHarness({ action }: { action: () => void | Promise<void> }) {
  const { requestConfirmation, dialog } = useAppDialog();
  return (
    <>
      <button
        type="button"
        onClick={() => requestConfirmation({
          title: "删除记录？",
          description: "此操作无法撤销。",
          confirmLabel: "删除",
          danger: true,
          action,
        })}
      >
        打开确认
      </button>
      {dialog}
    </>
  );
}

function ScopedConfirmHarness({
  scope,
  action,
}: {
  scope: string;
  action: () => void | Promise<void>;
}) {
  const { requestConfirmation, dialog } = useAppDialog(scope);
  return (
    <>
      <button type="button" onClick={() => requestConfirmation({ title: "执行操作？", action })}>
        打开操作
      </button>
      {dialog}
    </>
  );
}

function PromptHarness({ action }: { action: (values: Record<string, string>) => void | Promise<void> }) {
  const [submitted, setSubmitted] = useState("");
  const { requestInput, dialog } = useAppDialog();
  return (
    <>
      <button
        type="button"
        onClick={() => requestInput({
          title: "创建文件",
          fields: [
            { name: "name", label: "文件名", required: true },
            { name: "content", label: "内容", multiline: true },
          ],
          validate: ({ name }) => name.includes("/") ? "文件名不能包含斜杠。" : null,
          action: async (values) => {
            await action(values);
            setSubmitted(values.name);
          },
        })}
      >
        打开输入
      </button>
      <output>{submitted}</output>
      {dialog}
    </>
  );
}

function AwaitableConfirmHarness() {
  const [result, setResult] = useState("等待决定");
  const { confirm, dialog } = useAppDialog();
  return (
    <>
      <button
        type="button"
        onClick={() => void confirm({ title: "继续操作？", confirmLabel: "继续" })
          .then((confirmed) => setResult(confirmed ? "已确认" : "已取消"))}
      >
        询问
      </button>
      <output>{result}</output>
      {dialog}
    </>
  );
}

describe("AppDialog", () => {
  it("输入法候选 Enter 的 composing 或 229 不提交，普通 Enter 正常提交", async () => {
    const action = vi.fn();
    render(<PromptHarness action={action} />);
    fireEvent.click(screen.getByRole("button", { name: "打开输入" }));
    const input = screen.getByRole("textbox", { name: /文件名/ });
    fireEvent.change(input, { target: { value: "draft.md" } });
    fireEvent.keyDown(input, { key: "Enter", isComposing: true });
    fireEvent.keyDown(input, { key: "Enter", keyCode: 229, isComposing: false });
    expect(action).not.toHaveBeenCalled();
    expect(screen.getByRole("dialog")).toBeInTheDocument();
    fireEvent.keyDown(input, { key: "Enter" });
    await waitFor(() => expect(action).toHaveBeenCalledTimes(1));
    expect(action).toHaveBeenCalledWith({ name: "draft.md", content: "" });
  });

  it("输入框修改后 Escape 只打开舍弃确认，继续编辑保留内容", async () => {
    render(<PromptHarness action={vi.fn()} />);
    fireEvent.click(screen.getByRole("button", { name: "打开输入" }));
    const input = screen.getByRole("textbox", { name: /文件名/ });
    fireEvent.change(input, { target: { value: "draft.md" } });
    fireEvent.keyDown(input, { key: "Escape", isComposing: true });
    expect(screen.queryByRole("alertdialog")).toBeNull();
    fireEvent.keyDown(input, { key: "Escape" });
    expect(screen.getByRole("button", { name: "继续编辑" })).toHaveFocus();
    fireEvent.click(screen.getByRole("button", { name: "继续编辑" }));
    expect(input).toHaveValue("draft.md");
    expect(input).toHaveFocus();
    fireEvent.click(screen.getByRole("button", { name: "取消" }));
    fireEvent.click(screen.getByRole("button", { name: "舍弃修改" }));
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
  });

  it("可等待用户确认结果，并在取消时返回 false", async () => {
    render(<AwaitableConfirmHarness />);
    fireEvent.click(screen.getByRole("button", { name: "询问" }));
    fireEvent.click(screen.getByRole("button", { name: "取消" }));
    await waitFor(() => expect(screen.getByText("已取消")).toBeInTheDocument());

    fireEvent.click(screen.getByRole("button", { name: "询问" }));
    fireEvent.click(screen.getByRole("button", { name: "继续" }));
    await waitFor(() => expect(screen.getByText("已确认")).toBeInTheDocument());
  });

  it("危险确认默认聚焦取消，Escape 取消并恢复触发器焦点", () => {
    const action = vi.fn();
    render(<ConfirmHarness action={action} />);
    const opener = screen.getByRole("button", { name: "打开确认" });
    opener.focus();
    fireEvent.click(opener);

    const modal = screen.getByRole("alertdialog", { name: "删除记录？" });
    expect(within(modal).getByRole("button", { name: "取消" })).toHaveFocus();
    fireEvent.keyDown(document, { key: "Escape" });

    expect(screen.queryByRole("alertdialog")).toBeNull();
    expect(opener).toHaveFocus();
    expect(action).not.toHaveBeenCalled();
  });

  it("异步提交期间防止重复执行", async () => {
    let release!: () => void;
    const action = vi.fn(() => new Promise<void>((resolve) => { release = resolve; }));
    render(<ConfirmHarness action={action} />);
    fireEvent.click(screen.getByRole("button", { name: "打开确认" }));
    const modal = screen.getByRole("alertdialog");
    const submit = within(modal).getByRole("button", { name: "删除" });
    fireEvent.click(submit);
    fireEvent.click(submit);

    expect(action).toHaveBeenCalledTimes(1);
    expect(modal).toHaveAttribute("aria-busy", "true");
    expect(within(modal).getByRole("button", { name: "取消" })).toBeDisabled();
    await act(async () => release());
    await waitFor(() => expect(screen.queryByRole("alertdialog")).toBeNull());
  });

  it("操作失败时保留对话框和输入，可直接重试", async () => {
    const action = vi.fn()
      .mockRejectedValueOnce(new Error("远端暂时不可用"))
      .mockResolvedValueOnce(undefined);
    render(<PromptHarness action={action} />);
    fireEvent.click(screen.getByRole("button", { name: "打开输入" }));
    const name = screen.getByRole("textbox", { name: /文件名/ });
    fireEvent.change(name, { target: { value: "note.md" } });
    fireEvent.click(screen.getByRole("button", { name: "确定" }));

    expect(await screen.findByRole("alert")).toHaveTextContent("远端暂时不可用");
    expect(name).toHaveValue("note.md");
    fireEvent.click(screen.getByRole("button", { name: "确定" }));
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    expect(action).toHaveBeenCalledTimes(2);
    expect(screen.getByText("note.md")).toBeInTheDocument();
  });

  it("必填和业务校验均在提交前执行，单行输入支持 Enter", async () => {
    const action = vi.fn();
    render(<PromptHarness action={action} />);
    fireEvent.click(screen.getByRole("button", { name: "打开输入" }));
    const name = screen.getByRole("textbox", { name: /文件名/ });
    expect(name).toHaveFocus();
    fireEvent.keyDown(name, { key: "Enter" });
    expect(screen.getByRole("alert")).toHaveTextContent("请填写“文件名”");
    fireEvent.change(name, { target: { value: "bad/name" } });
    fireEvent.keyDown(name, { key: "Enter" });
    expect(screen.getByRole("alert")).toHaveTextContent("不能包含斜杠");
    fireEvent.change(name, { target: { value: "ok.md" } });
    const content = screen.getByRole("textbox", { name: "内容" });
    expect(content).not.toHaveAttribute("maxlength");
    fireEvent.change(content, { target: { value: "hello" } });
    fireEvent.keyDown(name, { key: "Enter" });
    await waitFor(() => expect(action).toHaveBeenCalledWith({ name: "ok.md", content: "hello" }));
  });

  it("提示框的取消按钮保留 Enter 取消语义", () => {
    const action = vi.fn();
    render(<PromptHarness action={action} />);
    fireEvent.click(screen.getByRole("button", { name: "打开输入" }));
    const cancel = screen.getByRole("button", { name: "取消" });
    cancel.focus();
    fireEvent.keyDown(cancel, { key: "Enter" });
    fireEvent.click(cancel);

    expect(screen.queryByRole("dialog")).toBeNull();
    expect(action).not.toHaveBeenCalled();
  });

  it("宿主切换业务范围时作废旧确认，不执行 stale action", async () => {
    const action = vi.fn();
    const { rerender } = render(<ScopedConfirmHarness scope="session-a" action={action} />);
    fireEvent.click(screen.getByRole("button", { name: "打开操作" }));
    expect(screen.getByRole("dialog", { name: "执行操作？" })).toBeInTheDocument();

    rerender(<ScopedConfirmHarness scope="session-b" action={action} />);
    await waitFor(() => expect(screen.queryByRole("dialog", { name: "执行操作？" })).toBeNull());
    expect(action).not.toHaveBeenCalled();
  });

  it("已作废的异步操作完成时不会关闭新对话框", async () => {
    let release!: () => void;
    const action = vi.fn()
      .mockImplementationOnce(() => new Promise<void>((resolve) => { release = resolve; }))
      .mockResolvedValue(undefined);
    const { rerender } = render(<ScopedConfirmHarness scope="session-a" action={action} />);
    fireEvent.click(screen.getByRole("button", { name: "打开操作" }));
    fireEvent.click(screen.getByRole("button", { name: "确定" }));

    rerender(<ScopedConfirmHarness scope="session-b" action={action} />);
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    fireEvent.click(screen.getByRole("button", { name: "打开操作" }));
    expect(screen.getByRole("dialog", { name: "执行操作？" })).toBeInTheDocument();

    await act(async () => release());
    expect(screen.getByRole("dialog", { name: "执行操作？" })).toBeInTheDocument();
    expect(action).toHaveBeenCalledTimes(1);
  });
});
