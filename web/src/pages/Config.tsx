import { useApi } from "../hooks/use-api";

export function Config() {
  const { data } = useApi<Record<string, unknown>>("/config");

  return (
    <div className="space-y-4">
      <h1 className="text-lg font-bold">配置（只读）</h1>
      <pre className="bg-gray-900 text-green-300 rounded-lg p-4 text-xs overflow-auto max-h-[75vh]">
        {data ? JSON.stringify(data, null, 2) : "Loading..."}
      </pre>
    </div>
  );
}
