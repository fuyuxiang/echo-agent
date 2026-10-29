import { act, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";

import { ChangeSetView } from "../explorer/ChangeSetView";
import type { ChangeSet, CommitHunk } from "../lib/types";

const changeSet: ChangeSet = {
  taskId: "task-1",
  baselineMode: "git",
  createdAt: "2026-09-24T00:00:00Z",
  reviewedFiles: [],
  changes: [
    { path: "src/task.ts", kind: "modified", added: 2, removed: 1, preExisting: false },
    { path: "src/other.ts", kind: "added", added: 3, removed: 0, preExisting: false },
    { path: "src/user.ts", kind: "modified", added: 1, removed: 1, preExisting: true },
  ],
};

describe("ChangeSetView", () => {
  it("commits only selected task files and keeps pre-existing changes protected", async () => {
    const onCommit = vi.fn();
    render(<ChangeSetView changeSet={changeSet} hasTask canCommit onCommit={onCommit} onOpenDiff={vi.fn()} onDiscard={vi.fn()} onRollback={vi.fn()} />);

    expect(screen.getByRole("checkbox", { name: "选择提交 src/user.ts" })).toBeDisabled();
    await userEvent.click(screen.getByRole("checkbox", { name: "选择提交 src/other.ts" }));
    await userEvent.click(screen.getByRole("button", { name: "提交选中 1 个文件" }));
    expect(onCommit).toHaveBeenCalledWith(["src/task.ts"], {});
  });

  it("lets the user submit a selected task hunk from a pre-existing dirty file", async () => {
    const onCommit = vi.fn();
    const onLoadHunks = vi.fn().mockResolvedValue([{ id: "hunk-1", preview: "@@ -2 +2 @@\n-old\n+task\n" }]);
    render(<ChangeSetView changeSet={changeSet} hasTask canCommit onCommit={onCommit} onLoadHunks={onLoadHunks} onOpenDiff={vi.fn()} onDiscard={vi.fn()} onRollback={vi.fn()} />);

    await userEvent.click(screen.getByRole("button", { name: "选择 src/user.ts 的任务差异块" }));
    expect(await screen.findByText("任务差异块 1")).toBeInTheDocument();
    await userEvent.click(screen.getByRole("checkbox", { name: "任务差异块 1" }));
    await userEvent.click(screen.getByRole("button", { name: "提交选中 3 个文件" }));
    expect(onCommit).toHaveBeenCalledWith(["src/task.ts", "src/other.ts"], { "src/user.ts": ["hunk-1"] });
  });

  it("任务切换后的旧差异块响应不会覆盖新任务可提交差异", async () => {
    let resolve!: (hunks: CommitHunk[]) => void;
    const onLoadHunks = vi.fn()
      .mockReturnValueOnce(new Promise<CommitHunk[]>((accept) => { resolve = accept; }))
      .mockResolvedValueOnce([{ id: "new-hunk", preview: "new task diff" }]);
    const props = { hasTask: true, canCommit: true, onCommit: vi.fn(), onLoadHunks, onOpenDiff: vi.fn(), onDiscard: vi.fn(), onRollback: vi.fn() };
    const view = render(<ChangeSetView {...props} changeSet={changeSet} />);
    await userEvent.click(screen.getByRole("button", { name: "选择 src/user.ts 的任务差异块" }));
    view.rerender(<ChangeSetView {...props} changeSet={{ ...changeSet, taskId: "task-2" }} />);
    await userEvent.click(screen.getByRole("button", { name: "选择 src/user.ts 的任务差异块" }));
    expect(await screen.findByText("new task diff")).toBeInTheDocument();
    await act(async () => { resolve([{ id: "old-hunk", preview: "old task diff" }]); });
    expect(screen.queryByText("old task diff")).not.toBeInTheDocument();
    expect(screen.getByText("new task diff")).toBeInTheDocument();
  });
  it("keeps remaining files selectable and supports a second partial commit in the same session", async () => {
    const onCommit = vi.fn();
    const props = { hasTask: true, canCommit: true, onCommit, onOpenDiff: vi.fn(), onDiscard: vi.fn(), onRollback: vi.fn() };
    const view = render(<ChangeSetView {...props} changeSet={changeSet} />);
    await userEvent.click(screen.getByRole("checkbox", { name: "选择提交 src/other.ts" }));
    await userEvent.click(screen.getByRole("button", { name: "提交选中 1 个文件" }));
    expect(onCommit).toHaveBeenNthCalledWith(1, ["src/task.ts"], {});
    const remaining = { ...changeSet, committedHash: "first-commit-hash", committedPaths: ["src/task.ts"], changes: changeSet.changes.filter(change => change.path !== "src/task.ts") };
    view.rerender(<ChangeSetView {...props} changeSet={remaining} />);
    const selectRemaining = screen.getByRole("checkbox", { name: "选择提交 src/other.ts" });
    expect(selectRemaining).toBeEnabled();
    await userEvent.click(selectRemaining);
    await userEvent.click(screen.getByRole("button", { name: "提交选中 1 个文件" }));
    expect(onCommit).toHaveBeenNthCalledWith(2, ["src/other.ts"], {});
    expect(screen.getByRole("checkbox", { name: "选择提交 src/user.ts" })).toBeDisabled();
  });

  it("unreviewed files remain reviewable, discardable and locally committable", async () => {
    const onOpenDiff = vi.fn();
    const onDiscard = vi.fn();
    render(<ChangeSetView changeSet={changeSet} hasTask canCommit onCommit={vi.fn()} onOpenDiff={onOpenDiff} onDiscard={onDiscard} onRollback={vi.fn()} />);
    expect(screen.getByRole("button", { name: "提交选中 2 个文件" })).toBeEnabled();
    await userEvent.click(screen.getByTitle("src/task.ts"));
    expect(onOpenDiff).toHaveBeenCalledWith(changeSet.changes[0]);
    await userEvent.click(screen.getByRole("button", { name: "丢弃 src/task.ts 的改动" }));
    expect(onDiscard).toHaveBeenCalledWith(changeSet.changes[0]);
  });

  it("ordinary files use mutually exclusive full-file and hunk selections and expose a real discard callback", async () => {
    const onCommit = vi.fn();
    const onDiscardHunk = vi.fn();
    render(<ChangeSetView changeSet={changeSet} hasTask canCommit onCommit={onCommit}
      onLoadHunks={vi.fn().mockResolvedValue([{id:"ordinary-hunk",preview:"@@ -1 +1 @@\n-old\n+new"}])}
      onDiscardHunk={onDiscardHunk} onOpenDiff={vi.fn()} onDiscard={vi.fn()} onRollback={vi.fn()}/>);
    await userEvent.click(screen.getByRole("button",{name:"选择 src/task.ts 的任务差异块"}));
    await userEvent.click(await screen.findByRole("checkbox",{name:"任务差异块 1"}));
    expect(screen.getByRole("checkbox",{name:"选择提交 src/task.ts"})).not.toBeChecked();
    await userEvent.click(screen.getByRole("button",{name:"提交选中 2 个文件"}));
    expect(onCommit).toHaveBeenLastCalledWith(["src/other.ts"],{"src/task.ts":["ordinary-hunk"]});
    await userEvent.click(screen.getByRole("checkbox",{name:"选择提交 src/task.ts"}));
    expect(screen.getByRole("checkbox",{name:"任务差异块 1"})).not.toBeChecked();
    await userEvent.click(screen.getByRole("button",{name:"丢弃 src/task.ts 的差异块 1"}));
    expect(onDiscardHunk).toHaveBeenCalledWith("src/task.ts","ordinary-hunk");
  });

  it("keeps deleted-file hunks committable while requiring a whole-file restore for permissions", async () => {
    const onCommit=vi.fn();const onDiscardHunk=vi.fn();const onDiscard=vi.fn();
    const deleted:ChangeSet={...changeSet,changes:[{path:"bin/run",kind:"deleted",added:0,removed:3,preExisting:false}]};
    render(<ChangeSetView changeSet={deleted} hasTask canCommit onCommit={onCommit}
      onLoadHunks={vi.fn().mockResolvedValue([{id:"deleted-hunk",preview:"@@ -1,3 +0,0 @@\n-#!/bin/sh"}])}
      onDiscardHunk={onDiscardHunk} onOpenDiff={vi.fn()} onDiscard={onDiscard} onRollback={vi.fn()}/>);
    await userEvent.click(screen.getByRole("button",{name:"选择 bin/run 的任务差异块"}));
    expect(await screen.findByRole("button",{name:"丢弃 bin/run 的差异块 1"})).toBeDisabled();
    expect(screen.getByText(/已删除文件请按文件恢复/)).toBeInTheDocument();
    await userEvent.click(screen.getByRole("checkbox",{name:"任务差异块 1"}));
    await userEvent.click(screen.getByRole("button",{name:"提交选中 1 个文件"}));
    expect(onCommit).toHaveBeenCalledWith([],{"bin/run":["deleted-hunk"]});
    await userEvent.click(screen.getByRole("button",{name:"丢弃 bin/run 的改动"}));
    expect(onDiscard).toHaveBeenCalledWith(deleted.changes[0]);
    expect(onDiscardHunk).not.toHaveBeenCalled();
  });

});
