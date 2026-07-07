import { useApi } from "../hooks/use-api";

interface Channel {
  name: string;
  type: string;
  running: boolean;
}

export function Channels() {
  const { data } = useApi<{ channels: Channel[] }>("/channels");

  return (
    <div className="space-y-2">
      {data?.channels.map((ch) => (
        <div key={ch.name} className="bg-white border rounded-lg p-4 flex items-center gap-4">
          <span className={`w-3 h-3 rounded-full ${ch.running ? "bg-green-500" : "bg-gray-300"}`} />
          <div className="flex-1">
            <div className="font-medium text-sm">{ch.name}</div>
            <div className="text-xs text-gray-500">{ch.type}</div>
          </div>
          <span className={`text-xs px-2 py-0.5 rounded ${ch.running ? "bg-green-100 text-green-700" : "bg-gray-100 text-gray-500"}`}>
            {ch.running ? "在线" : "离线"}
          </span>
        </div>
      ))}
    </div>
  );
}
