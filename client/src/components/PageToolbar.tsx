import { createContext, useContext, type ReactNode } from "react";
import {
  SessionNavigation,
  type SessionNavigationProps,
} from "./SessionHeader";

const PageChromeContext = createContext<
  SessionNavigationProps & { macOS?: boolean; nested?: boolean }
>({});

/** Page headers own window navigation once, including when the sidebar is hidden. */
export function PageChromeProvider({
  children,
  ...value
}: SessionNavigationProps & { macOS?: boolean; children: ReactNode }) {
  return (
    <PageChromeContext.Provider value={value}>
      {children}
    </PageChromeContext.Provider>
  );
}

/** A section navigation already supplies the window controls for its child pages. */
export function NestedPageChrome({ children }: { children: ReactNode }) {
  const value = useContext(PageChromeContext);
  return (
    <PageChromeContext.Provider value={{ ...value, nested: true }}>
      {children}
    </PageChromeContext.Provider>
  );
}

export function PageToolbar({
  className = "",
  children,
}: {
  className?: string;
  children: ReactNode;
}) {
  const { nested, macOS, ...navigation } = useContext(PageChromeContext);
  const hasNavigation = !nested && navigation.sidebarCollapsed;
  return (
    <header
      className={`page-toolbar ${className}${hasNavigation && macOS ? " page-toolbar--native-controls" : ""}`}
      data-tauri-drag-region
    >
      {hasNavigation && (
        <div className="page-toolbar__navigation">
          <SessionNavigation {...navigation} />
        </div>
      )}
      {children}
    </header>
  );
}
