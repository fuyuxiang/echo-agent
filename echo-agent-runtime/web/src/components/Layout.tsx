import { Outlet, Navigate } from "react-router";
import { useAuthStore } from "../stores/auth";
import { useAuthRequired } from "../stores/capabilities";
import { Sidebar } from "./Sidebar";
import { RouteErrorBoundary } from "./ErrorBoundary";

export function Layout() {
  const token = useAuthStore((s) => s.token);
  const authRequired = useAuthRequired();

  // An empty token is legitimate in the supported open / no-token mode, so
  // "no token" alone cannot mean "not logged in". Redirecting on it looped:
  // /login's probe succeeded with an empty token and navigated back here, which
  // bounced it straight out again.
  //
  // While the probe is in flight (null) render nothing rather than guessing.
  // Guessing "auth required" reinstates the loop; guessing "open" flashes the
  // dashboard on deployments that do need a token.
  if (authRequired === null && !token) return null;
  if (authRequired !== false && !token) return <Navigate to="/login" replace />;

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
