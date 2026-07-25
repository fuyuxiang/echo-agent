import { Outlet, Navigate } from "react-router";
import { useAuthStore } from "../stores/auth";
import { Sidebar } from "./Sidebar";

export function Layout() {
  const token = useAuthStore((s) => s.token);
  if (!token) return <Navigate to="/login" replace />;

  return (
    <div className="flex h-screen bg-gray-50">
      <Sidebar />
      <main className="flex-1 overflow-auto p-6">
        <Outlet />
      </main>
    </div>
  );
}
