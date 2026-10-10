import { useState } from "react";
import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { useUnsavedClose } from "../use-unsaved-close";
import { useModalFocus } from "../use-modal-focus";

function Editor({ busy, onClose }: { busy: boolean; onClose: () => void }) {
  const [value, setValue] = useState("");
  const { requestClose, closeDialog } = useUnsavedClose({ dirty: Boolean(value), busy, onClose });
  const ref = useModalFocus<HTMLDivElement>(true, requestClose);
  return <>
    <div ref={ref} role="dialog" aria-label="编辑器" tabIndex={-1}>
      <input aria-label="内容" value={value} onChange={(event) => setValue(event.target.value)} />
      <button onClick={requestClose}>关闭编辑器</button>
    </div>
    {closeDialog}
  </>;
}

function Harness({ busy = false }: { busy?: boolean }) {
  const [open, setOpen] = useState(false);
  return <><button onClick={() => setOpen(true)}>打开编辑器</button>{open && <Editor busy={busy} onClose={() => setOpen(false)} />}</>;
}

describe("useUnsavedClose", () => {
  it("未修改立即关闭；脏输入关闭前确认，继续编辑保留草稿及焦点", async () => {
    render(<Harness />);
    const opener = screen.getByRole("button", { name: "打开编辑器" });
    opener.focus();
    fireEvent.click(opener);
    fireEvent.click(screen.getByRole("button", { name: "关闭编辑器" }));
    expect(screen.queryByRole("dialog")).toBeNull();
    expect(opener).toHaveFocus();
    fireEvent.click(opener);
    const content = screen.getByRole("textbox", { name: "内容" });
    fireEvent.change(content, { target: { value: "保留草稿" } });
    content.focus();
    fireEvent.keyDown(content, { key: "Escape" });
    const confirmation = screen.getByRole("alertdialog", { name: "舍弃未保存的修改？" });
    expect(within(confirmation).getByRole("button", { name: "继续编辑" })).toHaveFocus();
    fireEvent.keyDown(document, { key: "Escape" });
    expect(screen.queryByRole("alertdialog")).toBeNull();
    expect(content).toHaveValue("保留草稿");
    expect(content).toHaveFocus();
    fireEvent.click(screen.getByRole("button", { name: "关闭编辑器" }));
    fireEvent.click(screen.getByRole("button", { name: "舍弃修改" }));
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    expect(opener).toHaveFocus();
  });

  it("保存中忽略关闭和 Escape", () => {
    render(<Harness busy />);
    fireEvent.click(screen.getByRole("button", { name: "打开编辑器" }));
    fireEvent.change(screen.getByRole("textbox"), { target: { value: "保存中" } });
    fireEvent.click(screen.getByRole("button", { name: "关闭编辑器" }));
    fireEvent.keyDown(document, { key: "Escape" });
    expect(screen.getByRole("dialog", { name: "编辑器" })).toBeInTheDocument();
    expect(screen.queryByRole("alertdialog")).toBeNull();
  });
});
