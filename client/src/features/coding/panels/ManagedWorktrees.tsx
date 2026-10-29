import { useEffect, useState } from "react";
import { codingApi } from "../lib/tauri-api";
export function ManagedWorktrees({
  root,
  onOpen,
  onRemoved,
  confirm,
}: {
  root: string;
  onOpen: (root: string) => void;
  onRemoved: (root: string) => void;
  confirm: (options: {
    title: string;
    description: string;
    confirmLabel: string;
    danger: boolean;
  }) => Promise<boolean>;
}) {
  const [items, setItems] = useState<
    Awaited<ReturnType<typeof codingApi.isolationList>>
  >([]);
  const [error, setError] = useState("");
  const [busy, setBusy] = useState("");
  const [reload, setReload] = useState(0);
  useEffect(() => {
    let disposed = false;
    void codingApi
      .isolationList(root)
      .then((value) => {
        if (!disposed) {
          setItems(value);
          setError("");
        }
      })
      .catch((error) => {
        if (!disposed) setError(String(error));
      });
    return () => {
      disposed = true;
    };
  }, [root, reload]);
  return (
    <div className="coding-panel coding-managed-worktrees">
      <p>
        隔离工作树从 Git HEAD
        创建，包含已提交内容。任务记录与磁盘目录分别管理；清理会重新核对未提交内容、未回并提交、引用和进程。
      </p>
      {items.length === 0 && !error && <p>当前项目没有受管理的隔离工作树。</p>}
      {items.map((item) => (
        <article key={item.root}>
          <strong>{item.root}</strong>
          <p>
            {item.status} · {item.taskIds.length} 个任务 ·{" "}
            {item.sessionIds.length} 个会话
          </p>
          {item.detail && <p>{item.detail}</p>}
          <button
            type="button"
            disabled={item.status === "missing" || !!busy}
            onClick={() => onOpen(item.root)}
          >
            打开工作树
          </button>
          <button
            type="button"
            disabled={item.root === root || !!busy}
            title={
              item.root === root
                ? "请先切换到原项目并关闭该工作树终端，再清理"
                : "使用 Git worktree remove 安全清理目录，保留会话记录"
            }
            onClick={async () => {
              if (
                !(await confirm({
                  title: "清理受管理的工作树？",
                  description: `${item.root}；有未提交内容、未回并提交或进程占用时将拒绝清理。会话记录会保留。`,
                  confirmLabel: "安全清理",
                  danger: true,
                }))
              )
                return;
              setBusy(item.root);
              try {
                await codingApi.isolationCleanup(item.root);
                onRemoved(item.root);
                setReload((value) => value + 1);
                setError("");
              } catch (error) {
                setError(String(error));
              } finally {
                setBusy("");
              }
            }}
          >
            {busy === item.root ? "正在核对…" : "清理目录"}
          </button>
        </article>
      ))}
      {error && <p role="alert">{error}</p>}
      <button
        type="button"
        disabled={!!busy}
        onClick={() => setReload((value) => value + 1)}
      >
        刷新状态
      </button>
    </div>
  );
}
