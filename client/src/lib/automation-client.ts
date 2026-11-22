import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

export type AutomationMode = "default" | "browser_use" | "computer_use";

export interface BrowserCapability {
  available: boolean;
  browserName?: string | null;
  executable?: string | null;
  reason?: string | null;
}

export interface AccessibilityCapability {
  supported: boolean;
  authorization: "granted" | "required" | "unknown";
  probe: "unknown" | "succeeded" | "failed";
  reason?: string | null;
}

export interface ComputerCapability {
  available: boolean;
  platform: string;
  screenCapture: boolean;
  inputControl: boolean;
  desktopAvailable?: boolean | null;
  accessibility?: AccessibilityCapability;
  reason?: string | null;
}

export interface InstalledApplication { id: string; name: string; path: string }
export interface DesktopReceipt {
  id: string; tool: string; status: string; verified: boolean;
  write: boolean; evidence: boolean; atMs: number;
}
export interface ComputerTask {
  phase: "running" | "waiting_user" | "verifying" | "paused" | "stopped" | "completed" | "failed" | "result_unknown";
  reason: string | null;
  scope: string | null;
  authorized: boolean;
  generation: number;
  taskId?: string;
  promptId?: string | null;
  runId?: string;
  currentTool?: string | null;
  targets?: string[];
  receipts?: DesktopReceipt[];
  outcome?: string | null;
}
export interface AutomationStatus {
  sessionId: string;
  mode: AutomationMode;
  paused: boolean;
  computerAuthorized?: boolean;
  task?: ComputerTask;
  revision?:number;
  selectedTarget?:InstalledApplication|null;
  activeWindow?:{application?:string;identifier?:string;windowRef?:string}|null;
  extensionConnected?:boolean;
  execution?:{nativeIsolation:string;backgroundNativeActions?:boolean;backgroundPointerInput?:boolean;foregroundCoordinateInput?:boolean;humanInputPolicy?:string;inputConflictDetection?:boolean;browserSource?:string;emergencyShortcut?:boolean;backgroundInput:boolean;inputMode:string;platform:string;fileDialog:boolean;windowClick:boolean};
  allowPrivateNetwork: boolean;
  browser: BrowserCapability;
  computer: ComputerCapability;
  browserRunning: boolean;
  browserUrl?: string | null;
  browserTitle?: string | null;
  browserHasData: boolean;
}

export interface AutomationCapabilities {
  browser: BrowserCapability;
  computer: ComputerCapability;
}

export interface AutomationApproval {
  requestId: string;
  sessionId: string;
  tool: string;
  title: string;
  description: string;
  details: unknown;
}

export interface AutomationApprovalClosedEvent {
  requestId: string;
  sessionId: string;
}

export const automationStatus = (sessionId: string) =>
  invoke<AutomationStatus>("automation_status", { sessionId });

type BrowserDocument = { targetId: string; loaderId: string; url: string };
export type BrowserPageInput =
  | { kind: "pointer"; x: number; y: number; destination: [number, number] | null; button: "left" | "middle" | "right"; count: number; modifiers: number }
  | { kind: "move"; x: number; y: number }
  | { kind: "wheel"; x: number; y: number; dx: number; dy: number }
  | { kind: "key"; key: string; code: string; virtualKey: number; modifiers: number }
  | { kind: "text"; text: string };
export type BrowserPanelAction =
  | { kind: "open" | "navigate"; url: string; allowDevelopment?: boolean }
  | { kind: "new" | "back" | "forward" | "reload" | "revokeDevelopment" }
  | { kind: "select" | "close"; id: string }
  | { kind: "input"; document: BrowserDocument; frameId: string; input: BrowserPageInput };
export interface BrowserPanelView {
  tabs: { id: string; title: string; url: string }[];
  activeId: string | null;
  external: boolean;
  allowPrivateNetwork: boolean;
  developmentEndpoints: string[];
  page: {
    document: BrowserDocument;
    canGoBack: boolean;
    canGoForward: boolean;
    frameId: string | null;
    image: string | null;
    width: number;
    height: number;
    error: boolean;
  } | null;
}
export const automationBrowserView = (sessionId: string, size: [number, number] | null, knownFrame: string | null) =>
  invoke<BrowserPanelView | null>("automation_browser_view", { sessionId, size, knownFrame });
export const automationBrowserAction = (sessionId: string, targetId: string | null, action: BrowserPanelAction) =>
  invoke<void>("automation_browser_action", { sessionId, targetId, action });

export const automationCapabilities = () =>
  invoke<AutomationCapabilities>("automation_capabilities");

export const automationPause = (sessionId: string) =>
  invoke<AutomationStatus>("automation_pause", { sessionId });

export const automationResume = (sessionId: string) =>
  invoke<AutomationStatus>("automation_resume", { sessionId });

export const automationStop = (sessionId: string) =>
  invoke<AutomationStatus>("automation_stop", { sessionId });

export const automationClearBrowserData = (sessionId: string) =>
  invoke<AutomationStatus>("automation_clear_browser_data", { sessionId });

export const automationSetPrivateNetwork = (sessionId: string, allowed: boolean) =>
  invoke<AutomationStatus>("automation_set_private_network", { sessionId, allowed });

export const automationRequestComputerPermissions = () =>
  invoke<ComputerCapability>("automation_request_computer_permissions");

export const automationPendingApprovals = (sessionId?: string) =>
  invoke<AutomationApproval[]>("automation_pending_approvals", {
    sessionId: sessionId ?? null,
  });

export const automationResolveApproval = (requestId: string, approved: boolean, allowForTask = false) =>
  invoke<boolean>("automation_resolve_approval", { requestId, approved, allowForTask });

export const onAutomationStatus = (
  callback: (status: AutomationStatus) => void,
): Promise<UnlistenFn> => listen<AutomationStatus>("automation://status", (event) => callback(event.payload));

export const onAutomationApproval = (
  callback: (approval: AutomationApproval) => void,
): Promise<UnlistenFn> => listen<AutomationApproval>("automation://approval", (event) => callback(event.payload));

export const onAutomationApprovalClosed = (
  callback: (event: AutomationApprovalClosedEvent) => void,
): Promise<UnlistenFn> => listen<AutomationApprovalClosedEvent>(
  "automation://approval-closed",
  (event) => callback(event.payload),
);

export const automationTargets=()=>invoke<InstalledApplication[]>("automation_targets");
export const automationSelectTarget=(sessionId:string,applicationId:string)=>invoke<AutomationStatus>("automation_select_target",{sessionId,applicationId});
export const automationPreview=(sessionId:string)=>invoke<{data:string;meta:unknown}>("automation_preview",{sessionId});
export const automationPairBrowser=(sessionId:string)=>invoke<{endpoint:string;extensionPath:string;expiresInSeconds:number}>("automation_pair_browser",{sessionId});
export const automationDisconnectBrowser=(sessionId:string)=>invoke<void>("automation_disconnect_browser",{sessionId});
export const automationUseIsolatedBrowser=(sessionId:string)=>invoke<AutomationStatus>("automation_use_isolated_browser",{sessionId});
