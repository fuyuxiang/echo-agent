import { Outlet, Navigate } from "react-router";
import { useAuthStore } from "../stores/auth";
import { Sidebar } from "./Sidebar";
import { RouteErrorBoundary } from "./ErrorBoundary";

export function Layout() {
  const token = useAuthStore((s) => s.token);
  if (!token) return <Navigate to="/login" replace />;

  return (
    <div className="flex h-screen bg-gray-50">
      <Sidebar />
      <main className="flex-1 overflow-auto p-6">
        {/* Scoped to the page, not the shell: a page that throws must not take
            the sidebar (and thus the way out of it) down with it. */}
        <RouteErrorBoundary>
          <Outlet />
        </RouteErrorBoundary>
      </main>
    </div>
  );
}
