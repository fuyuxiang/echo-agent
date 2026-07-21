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
  config_valid?: boolean;
}

export function Cron() {
  const { data, loading, error, refetch } = useApi<{ jobs: CronJob[] }>("/cron");
  const [showCreate, setShowCreate] = useState(false);
  const [name, setName] = useState("");
  const [expr, setExpr] = useState("");
  const [command, setCommand] = useState("");
  const [deliverChannel, setDeliverChannel] = useState("");
  const [deliverChatId, setDeliverChatId] = useState("");
  const [sourceSessionKey, setSourceSessionKey] = useState("");

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
    if (!command.trim()) {
      return; // 任务内容必填:与后端 400 校验双保险
    }
    const payload: Record<string, string> = { command: command.trim() };
    if (deliverChannel.trim()) payload.deliver_channel = deliverChannel.trim();
    if (deliverChatId.trim()) payload.deliver_chat_id = deliverChatId.trim();
    if (sourceSessionKey.trim()) payload.source_session_key = sourceSessionKey.trim();
    const ok = await runMutation(
      () => apiFetch("/cron", { method: "POST", body: JSON.stringify({ name, cron_expr: expr, payload }) }),
      { success: "已创建", error: "创建失败" },
    );
    if (ok) {
      setName(""); setExpr(""); setCommand("");
      setDeliverChannel(""); setDeliverChatId(""); setSourceSessionKey("");
      setShowCreate(false);
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
        <form onSubmit={create} className="bg-white border rounded p-4 flex flex-col gap-3">
          <div className="flex gap-3">
            <input value={name} onChange={(e) => setName(e.target.value)} placeholder="任务名称" className="border rounded px-3 py-1.5 flex-1" />
            <input value={expr} onChange={(e) => setExpr(e.target.value)} placeholder="cron 表达式" className="border rounded px-3 py-1.5 w-40" />
          </div>
          <textarea value={command} onChange={(e) => setCommand(e.target.value)} placeholder="任务内容(必填,交给 Agent 执行的指令)" className="border rounded px-3 py-1.5" rows={2} />
          <div className="flex gap-3">
            <input value={deliverChannel} onChange={(e) => setDeliverChannel(e.target.value)} placeholder="投递渠道(deliver_channel,可选)" className="border rounded px-3 py-1.5 flex-1" />
            <input value={deliverChatId} onChange={(e) => setDeliverChatId(e.target.value)} placeholder="会话/群 ID(deliver_chat_id,可选)" className="border rounded px-3 py-1.5 flex-1" />
          </div>
          <input value={sourceSessionKey} onChange={(e) => setSourceSessionKey(e.target.value)} placeholder="会话目标 source_session_key(可选,填了可自动解析渠道)" className="border rounded px-3 py-1.5" />
          <button type="submit" disabled={!command.trim()} className="bg-green-600 text-white px-3 py-1.5 rounded text-sm disabled:opacity-50 self-start">创建</button>
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
              <td className="py-2 font-medium">
                {job.name || job.id}
                {job.config_valid === false && (
                  <span className="ml-2 text-xs px-1.5 rounded bg-red-100 text-red-700">配置无效</span>
                )}
              </td>
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
