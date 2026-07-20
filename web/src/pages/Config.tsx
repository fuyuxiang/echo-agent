import { useApi } from "../hooks/use-api";

export function Config() {
  const { data, loading, error } = useApi<Record<string, unknown>>("/config");

  return (
    <div className="space-y-4">
      <h1 className="text-lg font-bold">配置（只读）</h1>
      {error && (
        <div className="bg-red-50 text-red-600 rounded-lg p-4 text-sm">
          加载失败：{error}
          {/* /config 需要 admin token,403 通常意味着登录用的不是有效的 admin token */}
        </div>
      )}
      {!error && (
        <pre className="bg-gray-900 text-green-300 rounded-lg p-4 text-xs overflow-auto max-h-[75vh]">
          {loading ? "加载中..." : JSON.stringify(data, null, 2)}
        </pre>
      )}
    </div>
  );
}
