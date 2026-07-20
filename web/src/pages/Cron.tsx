import { useState } from "react";
import { useApi } from "../hooks/use-api";
import { apiFetch } from "../lib/api";
import { runMutation } from "../stores/toast";
import { Play, Trash2, Plus } from "lucide-react";

interface CronJob {
  id: string;
  name: string;
  cron_expr: string;
  enabled: boolean;
  status: string;
  last_status: string;
  next_run_ms: number | null;
}

export function Cron() {
  const { data, loading, error, refetch } = useApi<{ jobs: CronJob[] }>("/cron");
  const [showCreate, setShowCreate] = useState(false);
  const [name, setName] = useState("");
  const [expr, setExpr] = useState("");

  const trigger = async (id: string) => {
    const ok = await runMutation(() => apiFetch(`/cron/${id}/trigger`, { method: "POST" }), {
      success: "已触发执行", error: "触发失败",
    });
    if (ok) refetch();
  };

  const remove = async (id: string) => {
    const ok = await runMutation(() => apiFetch(`/cron/${id}`, { method: "DELETE" }), {
      success: "已删除", error: "删除失败",
    });
    if (ok) refetch();
  };

  const create = async (e: React.FormEvent) => {
    e.preventDefault();
    const ok = await runMutation(
      () => apiFetch("/cron", { method: "POST", body: JSON.stringify({ name, cron_expr: expr }) }),
      { success: "已创建", error: "创建失败" },
    );
    if (ok) {
      setName(""); setExpr(""); setShowCreate(false);
      refetch();
    }
  };

  return (
    <div className="space-y-4">
      <div className="flex justify-between items-center">
        <h1 className="text-lg font-bold">定时任务</h1>
        <button onClick={() => setShowCreate(!showCreate)} className="flex items-center gap-1 bg-blue-600 text-white px-3 py-1.5 rounded text-sm">
          <Plus size={16} /> 新建
        </button>
      </div>

      {showCreate && (
        <form onSubmit={create} className="bg-white border rounded p-4 flex gap-3">
          <input value={name} onChange={(e) => setName(e.target.value)} placeholder="任务名称" className="border rounded px-3 py-1.5 flex-1" />
          <input value={expr} onChange={(e) => setExpr(e.target.value)} placeholder="cron 表达式" className="border rounded px-3 py-1.5 w-40" />
          <button type="submit" className="bg-green-600 text-white px-3 py-1.5 rounded text-sm">创建</button>
        </form>
      )}

      <table className="w-full text-sm">
        <thead>
          <tr className="border-b text-left text-gray-500">
            <th className="py-2">名称</th>
            <th>Cron</th>
            <th>状态</th>
            <th>最近结果</th>
            <th>下次执行</th>
            <th>操作</th>
          </tr>
        </thead>
        <tbody>
          {data?.jobs.map((job) => (
            <tr key={job.id} className="border-b">
              <td className="py-2 font-medium">{job.name || job.id}</td>
              <td className="font-mono text-xs">{job.cron_expr}</td>
              <td><span className={`text-xs px-1.5 rounded ${job.enabled ? "bg-green-100 text-green-700" : "bg-gray-100"}`}>{job.enabled ? "活跃" : "暂停"}</span></td>
              <td className="text-xs">{job.last_status || "-"}</td>
              <td className="text-xs">{job.next_run_ms ? new Date(job.next_run_ms).toLocaleString() : "-"}</td>
              <td className="flex gap-1">
                <button onClick={() => trigger(job.id)} className="p-1 hover:bg-gray-100 rounded" title="立即执行"><Play size={14} /></button>
                <button onClick={() => remove(job.id)} className="p-1 hover:bg-red-50 rounded text-red-500" title="删除"><Trash2 size={14} /></button>
              </td>
            </tr>
          ))}
        </tbody>
      </table>
      {error && <div className="text-red-600 text-sm bg-red-50 border border-red-200 rounded p-3">加载失败：{error}</div>}
      {!error && loading && !data && <div className="text-gray-400 text-sm p-3">加载中...</div>}
      {!error && data && data.jobs.length === 0 && <div className="text-gray-400 text-center py-8">暂无定时任务</div>}
    </div>
  );
}
