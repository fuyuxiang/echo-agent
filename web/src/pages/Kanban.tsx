import { useEffect, useState } from "react";
import { DndContext, DragEndEvent, closestCenter, useDroppable, useDraggable } from "@dnd-kit/core";
import { useKanbanStore, COLUMNS, TaskCard } from "../stores/kanban";
import { useWsSubscribe } from "../hooks/use-ws";
import { Plus } from "lucide-react";

export function Kanban() {
  const { tasks, loading, fetchTasks, transitionTask, createTask, updateLocal, addLocal } = useKanbanStore();
  const [newTitle, setNewTitle] = useState("");

  useEffect(() => { fetchTasks(); }, []);

  useWsSubscribe(["tasks"], (ev) => {
    if (ev.type === "task_created") addLocal(ev.payload);
    else if (ev.type === "task_transitioned") updateLocal(ev.payload.id, { status: ev.payload.to });
    else if (ev.type === "task_updated") updateLocal(ev.payload.id, ev.payload);
  }, ["task_created", "task_transitioned", "task_updated"]);

  const handleDragEnd = (event: DragEndEvent) => {
    const { active, over } = event;
    if (!over) return;
    const targetColumn = over.id as string;
    const taskId = active.id as string;
    const task = tasks.find((t) => t.id === taskId);
    if (task && task.status !== targetColumn) {
      const previousStatus = task.status;
      // Optimistically update
      updateLocal(taskId, { status: targetColumn });
      transitionTask(taskId, targetColumn).catch(() => {
        // Revert on failure
        updateLocal(taskId, { status: previousStatus });
      });
    }
  };

  const handleCreate = async (e: React.FormEvent) => {
    e.preventDefault();
    if (!newTitle.trim()) return;
    await createTask(newTitle.trim());
    setNewTitle("");
  };

  if (loading) return <div className="p-8 text-center text-gray-500">Loading...</div>;

  return (
    <div className="h-full flex flex-col">
      <form onSubmit={handleCreate} className="flex gap-2 mb-4">
        <input
          value={newTitle}
          onChange={(e) => setNewTitle(e.target.value)}
          placeholder="新建任务..."
          className="border rounded px-3 py-1.5 flex-1"
        />
        <button type="submit" className="bg-blue-600 text-white px-3 py-1.5 rounded flex items-center gap-1">
          <Plus size={16} /> 创建
        </button>
      </form>

      <DndContext collisionDetection={closestCenter} onDragEnd={handleDragEnd}>
        <div className="flex gap-3 flex-1 overflow-x-auto">
          {COLUMNS.map((col) => (
            <KanbanColumn key={col.id} column={col} tasks={tasks.filter((t) => t.status === col.id)} />
          ))}
        </div>
      </DndContext>
    </div>
  );
}

function KanbanColumn({ column, tasks }: { column: { id: string; label: string }; tasks: TaskCard[] }) {
  const { setNodeRef, isOver } = useDroppable({ id: column.id });

  return (
    <div
      ref={setNodeRef}
      className={`flex-shrink-0 w-64 rounded-lg p-2 flex flex-col ${isOver ? "bg-blue-50" : "bg-gray-100"}`}
    >
      <div className="font-semibold text-sm mb-2 flex items-center gap-2">
        {column.label}
        <span className="text-xs bg-gray-200 rounded-full px-2">{tasks.length}</span>
      </div>
      <div className="flex-1 space-y-2 overflow-y-auto">
        {tasks.map((task) => (
          <KanbanCard key={task.id} task={task} />
        ))}
      </div>
    </div>
  );
}

function KanbanCard({ task }: { task: TaskCard }) {
  const { attributes, listeners, setNodeRef, transform, isDragging } = useDraggable({ id: task.id });

  const style = transform
    ? { transform: `translate(${transform.x}px, ${transform.y}px)` }
    : undefined;

  return (
    <div
      ref={setNodeRef}
      style={style}
      {...listeners}
      {...attributes}
      className={`bg-white rounded-md border p-3 shadow-sm cursor-grab active:cursor-grabbing ${isDragging ? "opacity-50" : ""}`}
    >
      <div className="text-sm font-medium">{task.title}</div>
      {task.assignee && <div className="text-xs text-gray-500 mt-1">@{task.assignee}</div>}
      {task.labels.length > 0 && (
        <div className="flex gap-1 mt-1 flex-wrap">
          {task.labels.map((l) => (
            <span key={l} className="text-xs bg-blue-100 text-blue-700 px-1.5 rounded">{l}</span>
          ))}
        </div>
      )}
    </div>
  );
}
