import { Component, type ErrorInfo, type ReactNode } from "react";
import { useLocation } from "react-router";
import { useTranslation } from "react-i18next";
import { AlertTriangle } from "lucide-react";

/**
 * Catches render-time errors from a page so one bad response cannot blank the
 * whole dashboard. Without it, something as small as a backend returning `{}`
 * where the page expects `{tasks: [...]}` left the user with a white screen and
 * no recovery short of reloading.
 *
 * Keyed by route in {@link RouteErrorBoundary}: navigating elsewhere clears the
 * error, so a broken page never traps the rest of the app.
 */
class ErrorBoundaryInner extends Component<
  { children: ReactNode; fallback: (error: Error, reset: () => void) => ReactNode },
  { error: Error | null }
> {
  state: { error: Error | null } = { error: null };

  static getDerivedStateFromError(error: Error) {
    return { error };
  }

  componentDidCatch(error: Error, info: ErrorInfo) {
    // Keep the stack in the console: the fallback deliberately shows only the
    // message, but whoever is debugging needs the component trace.
    console.error("Dashboard render error:", error, info.componentStack);
  }

  render() {
    if (this.state.error) {
      return this.props.fallback(this.state.error, () => this.setState({ error: null }));
    }
    return this.props.children;
  }
}

function Fallback({ error, reset }: { error: Error; reset: () => void }) {
  const { t } = useTranslation("common");
  return (
    <div role="alert" className="bg-red-50 border border-red-200 rounded-lg p-6 max-w-2xl">
      <div className="flex items-center gap-2 text-red-700 font-semibold mb-2">
        <AlertTriangle size={18} />
        {t("renderErrorTitle")}
      </div>
      <p className="text-sm text-red-600 mb-4">{t("renderErrorHint")}</p>
      <pre className="text-xs bg-white border border-red-100 rounded p-3 overflow-auto max-h-40 text-red-800">
        {error.message}
      </pre>
      <button
        onClick={reset}
        className="mt-4 bg-red-600 text-white px-3 py-1.5 rounded text-sm hover:bg-red-700"
      >
        {t("retry")}
      </button>
    </div>
  );
}

/** Route-scoped boundary: remounts on navigation so the error does not persist
 *  onto a page that renders fine. */
export function RouteErrorBoundary({ children }: { children: ReactNode }) {
  const { pathname } = useLocation();
  return (
    <ErrorBoundaryInner
      key={pathname}
      fallback={(error, reset) => <Fallback error={error} reset={reset} />}
    >
      {children}
    </ErrorBoundaryInner>
  );
}
