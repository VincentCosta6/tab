// The one place the UI talks to the backend. In a plain browser (vite dev
// without Tauri) it falls back to an in-memory mock so the UI can be built
// and screenshotted without the desktop shell.

import type {
  AgentKind, AgentProbe, AuditEntry, Harness, ModelOption, Chat, ChatStarted, ConnectorInfo, ContextEntry, ContextSection, EngineEvent, GhLoginCode, GhStatus,
  AgentActivity, DoneGuard, GraphNode, GraphView, LinkSummary, MyPull, IndexStatus, ItemContext, ItemDetail, Project, ProjectNote, ItemState, DepsStatus, LspEvent, LspServer, LspStarted, NewContext, NewTask, Pressure, Preview, PrRef, PullReview, QueueItem,
  RateLimitState, ReviewAction, HubStatus, SandboxStatus, Session, StorageUsage, Swept, ReviewWorkspace, SearchHit, SearchQuery, TabConfig, UsageRow, UsageSummary, WorkspaceProgress,
  Activity, Issue, Workflow, WorkflowRun, AppVersion, AppUpdate, AppUpdateProgress, Account, AccountStatus, CloudHub,
} from "./types";
import { mockApi } from "./mock";

export const inTauri = typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;

async function call<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  if (!inTauri) return mockApi.invoke<T>(cmd, args);
  const { invoke } = await import("@tauri-apps/api/core");
  return invoke<T>(cmd, args);
}

export const api = {
  listItems: (states: ItemState[]) => call<QueueItem[]>("list_items", { states }),
  searchItems: (query: string, states: ItemState[]) => call<QueueItem[]>("search_items", { query, states }),
  itemDetail: (id: string) => call<ItemDetail | null>("item_detail", { id }),
  itemByUrl: (url: string) => call<QueueItem | null>("item_by_url", { url }),
  githubLinkSummary: (repo: string, what: string, id: string) => call<LinkSummary>("github_link_summary", { repo, what, id }),
  githubMyPulls: () => call<MyPull[]>("github_my_pulls"),
  itemPreview: (id: string) => call<Preview>("item_preview", { id }),
  doneGuards: (id: string) => call<DoneGuard[]>("done_guards", { id }),
  markDone: (id: string) => call<QueueItem | null>("mark_done", { id }),
  snooze: (id: string, minutes: number) => call<QueueItem | null>("snooze", { id, minutes }),
  reopen: (id: string) => call<QueueItem | null>("reopen", { id }),
  renameItem: (id: string, title: string) => call<QueueItem | null>("rename_item", { id, title }),
  titlerDefaultRules: () => call<string>("titler_default_rules"),
  listConnectors: () => call<ConnectorInfo[]>("list_connectors"),
  rateLimits: () => call<RateLimitState[]>("rate_limits"),
  auditLog: (query: { provider?: string; connector?: string; outcome?: string; limit?: number }) =>
    call<AuditEntry[]>("audit_log", { query }),
  apiUsage: (hours: number) => call<UsageRow[]>("api_usage", { hours }),
  getConfig: () => call<TabConfig>("get_config"),
  saveConfig: (config: TabConfig) => call<TabConfig>("save_config", { config }),
  ratePressure: () => call<Pressure[]>("rate_pressure"),
  itemContext: (id: string) => call<ItemContext | null>("item_context", { id }),
  itemsForLink: (url: string) => call<QueueItem[]>("items_for_link", { url }),
  contextAdd: (itemId: string, entry: NewContext) => call<ContextEntry>("context_add", { itemId, entry }),
  contextUpdate: (id: string, patch: { section?: ContextSection; pinned?: boolean }) =>
    call<ContextEntry | null>("context_update", { id, section: patch.section ?? null, pinned: patch.pinned ?? null }),
  contextRemove: (id: string) => call<void>("context_remove", { id }),
  createTask: (task: NewTask) => call<QueueItem>("create_task", { task }),
  chatOpen: (itemId: string) => call<Chat>("chat_open", { itemId }),
  chatSend: (itemId: string | null, text: string, projectId?: string | null, attachments: string[] = []) => call<ChatStarted>("chat_send", { itemId, text, projectId: projectId ?? null, attachments }),
  attachmentSave: async (name: string, bytes: Uint8Array): Promise<string> => {
    if (!inTauri) return mockApi.invoke<string>("attachment_save", { name, bytes });
    const { invoke } = await import("@tauri-apps/api/core");
    return invoke<string>("attachment_save", bytes, { headers: { "x-name": encodeURIComponent(name) } });
  },
  imageRead: (path: string) => call<ArrayBuffer>("image_read", { path }),
  fileRead: (path: string) => call<ArrayBuffer>("file_read", { path }),
  fileOpen: (path: string, reveal = false) => call<void>("file_open", { path, reveal }),
  startDictation: () => call<void>("start_dictation"),
  agentActivity: () => call<AgentActivity[]>("agent_activity"),
  slashCommands: () => call<string[]>("agent_slash_commands"),
  graphView: (projectId: string | null, days: number | null, limit?: number) => call<GraphView>("graph_view", { projectId, days, limit: limit ?? null }),
  graphNode: (id: number) => call<GraphNode>("graph_node", { id }),
  graphDelete: (id: number, withExclusive: boolean) => call<number>("graph_delete", { id, withExclusive }),
  askChatOpen: (projectId: string | null) => call<Chat>("ask_chat_open", { projectId }),
  askChatSend: (projectId: string | null, text: string, focus: string[], attachments: string[] = []) => call<Session>("ask_chat_send", { projectId, text, focus, attachments }),
  searchIndexStatus: () => call<IndexStatus>("search_index_status"),
  projectsList: () => call<Project[]>("projects_list"),
  projectSave: (p: { id?: string; name: string; color: string; icon: string }) =>
    call<Project>("project_save", { id: p.id ?? null, name: p.name, color: p.color, icon: p.icon }),
  projectDelete: (id: string) => call<void>("project_delete", { id }),
  itemSetProject: (id: string, projectId: string | null) => call<QueueItem | null>("item_set_project", { id, projectId }),
  projectNotes: (projectId: string) => call<ProjectNote[]>("project_notes", { projectId }),
  projectNoteSave: (projectId: string, note: { id?: string; topic: string; text: string }) =>
    call<void>("project_note_save", { projectId, id: note.id ?? null, topic: note.topic, text: note.text }),
  projectNoteDelete: (id: string) => call<void>("project_note_delete", { id }),
  reviewChatOpen: (repo: string, number: number) => call<Chat>("review_chat_open", { repo, number }),
  reviewChatSend: (target: { repo: string; number: number; title: string; url: string; head_ref: string; base_ref: string }, text: string, attachments: string[] = []) =>
    call<Session>("review_chat_send", { target, text, attachments }),
  /** Sets `agent`'s model. Before a chat starts, `forNewChats` also makes `agent` the harness new chats use. */
  setAgentModel: (model: string | null, agent?: AgentKind, forNewChats = false) => call<TabConfig>("set_agent_model", { model, agent: agent ?? null, forNewChats }),
  setAgentMode: (mode: string) => call<TabConfig>("set_agent_mode", { mode }),
  connectionSetSecrets: (id: string, values: Record<string, string>) => call<TabConfig>("connection_set_secrets", { id, values }),
  chatStop: (sessionId: string) => call<void>("chat_stop", { sessionId }),
  /** Stop the tool the turn is waiting on so steering already sent is read now. */
  chatInterrupt: (sessionId: string) => call<void>("chat_interrupt", { sessionId }),
  approvalResolve: (id: string, decision: "allow" | "always" | "deny" | "answer", message?: string, answers?: Record<string, string>) =>
    call<boolean>("approval_resolve", { id, decision, message: message ?? null, answers: answers ?? null }),
  detectAgents: () => call<AgentProbe[]>("detect_agents"),
  agentHarnesses: () => call<Harness[]>("agent_harnesses"),
  agentModels: () => call<ModelOption[]>("agent_models"),
  /** Installs a harness; its output arrives through `onAgentUpgrade`. */
  agentInstall: (kind: AgentKind) => call<void>("agent_install", { kind }),
  agentHealth: () => call<AgentProbe | null>("agent_health"),
  /** Runs the agent's own update command; its output arrives through `onAgentUpgrade`. */
  agentUpgrade: () => call<void>("agent_upgrade"),
  appVersion: () => call<AppVersion>("app_version"),
  /** A newer release of tab, or null when this is the newest. */
  appUpdateCheck: () => call<AppUpdate | null>("app_update_check"),
  /** Installs what the last check found, then restarts the app. */
  appUpdateInstall: () => call<void>("app_update_install"),
  usageSummary: () => call<UsageSummary>("usage_summary"),
  ghStatus: () => call<GhStatus>("gh_status"),
  ghLogin: () => call<GhStatus>("gh_login"),
  ghInstall: () => call<GhStatus>("gh_install"),
  /** Why a folder can't be where sessions start, or null when it can. */
  folderProblem: (path: string) => call<string | null>("folder_problem", { path }),
  mcpInfo: () => call<{ url: string | null; token: string }>("mcp_info"),
  githubPull: (repo: string, number: number, sample: boolean) => call<PullReview>("github_pull", { repo, number, sample }),
  githubFile: (repo: string, sha: string, path: string) => call<string>("github_file", { repo, sha, path }),
  githubReviewAction: (pr: PrRef, action: ReviewAction) => call<void>("github_review_action", { pr, action }),
  workspacePrepare: (target: { repo: string; number: number; head_sha: string; base_sha: string; base_ref: string }) =>
    call<ReviewWorkspace>("review_workspace_prepare", { target }),
  workspaceFile: (repo: string, sha: string, path: string) => call<string>("review_workspace_file", { repo, sha, path }),
  workspaceFiles: (repo: string, number: number, paths: string[]) => call<[string, string][]>("review_workspace_files", { repo, number, paths }),
  workspaceSearch: (repo: string, number: number, query: SearchQuery) => call<SearchHit[]>("review_workspace_search", { repo, number, query }),
  workspaceOpen: (repo: string, number: number, target: "vscode" | "folder", path?: string, line?: number) =>
    call<void>("review_workspace_open", { repo, number, target, path: path ?? null, line: line ?? null }),
  vscodeAvailable: () => call<boolean>("review_vscode_available"),
  sandboxStatus: () => call<SandboxStatus>("sandbox_status"),
  storageUsage: () => call<StorageUsage>("storage_usage"),
  hubStatus: () => call<HubStatus | null>("hub_status"),
  accountStatus: () => call<AccountStatus>("account_status"),
  /** Opens the browser and resolves once it comes back signed in. */
  accountSignIn: (provider: "apple" | "google") => call<Account>("account_sign_in", { provider }),
  accountCancelSignIn: () => call<void>("account_cancel_sign_in"),
  accountSignOut: () => call<void>("account_sign_out"),
  /** Where the signed-in person's private cloud hub is. Rejects with a CloudFailure. */
  cloudHubStatus: () => call<CloudHub>("cloud_hub_status"),
  /** Sets up (or retries) their private hub; resolves once it's ready or failed. Rejects with a CloudFailure. */
  cloudHubSetUp: () => call<CloudHub>("cloud_hub_set_up"),
  storageClean: () => call<Swept>("storage_clean"),
  /** One checkout, or every one when no repo is given. */
  storageRemoveCheckouts: (repo?: string, number?: number) =>
    call<Swept>("storage_remove_checkouts", { repo: repo ?? null, number: number ?? null }),
  lspServers: () => call<LspServer[]>("lsp_servers"),
  lspInstall: (id: string) => call<LspServer>("lsp_install", { id }),
  lspStart: (server: string, repo: string, number: number) => call<LspStarted>("lsp_start", { server, repo, number }),
  lspRequest: <T = unknown>(session: string, method: string, params: unknown) => call<T>("lsp_request", { session, method, params }),
  lspNotify: (session: string, method: string, params: unknown) => call<void>("lsp_notify", { session, method, params }),
  lspStop: (session: string) => call<void>("lsp_stop", { session }),
  depsPrepare: (repo: string, number: number, ecosystem: string) => call<DepsStatus>("deps_prepare", { repo, number, ecosystem }),
  depsTypes: (repo: string, number: number) => call<[string, string][]>("deps_types", { repo, number }),
  workflowsList: () => call<Workflow[]>("workflows_list"),
  workflowTemplates: () => call<Workflow[]>("workflow_templates"),
  workflowValidate: (workflow: Workflow) => call<Issue[]>("workflow_validate", { workflow }),
  workflowSave: (workflow: Workflow) => call<{ workflow: Workflow; issues: Issue[] }>("workflow_save", { workflow }),
  workflowSetEnabled: (id: string, enabled: boolean, allowDestructive?: boolean, allowCommands?: boolean) =>
    call<Workflow>("workflow_set_enabled", { id, enabled, allowDestructive: allowDestructive ?? null, allowCommands: allowCommands ?? null }),
  workflowDelete: (id: string) => call<void>("workflow_delete", { id }),
  workflowRuns: (workflowId: string | null, limit = 50) => call<WorkflowRun[]>("workflow_runs", { workflowId, limit }),
  workflowRun: (workflow: Workflow, itemId: string | null, dryRun: boolean) => call<WorkflowRun>("workflow_run", { workflow, itemId, dryRun }),
  workflowChatOpen: (workflowId: string | null) => call<Chat>("workflow_chat_open", { workflowId }),
  workflowChatSend: (text: string, open: Workflow | null) => call<Session>("workflow_chat_send", { text, open }),
  browserOpen: (id: string, url: string, bounds: Bounds) => call<void>("browser_open", { id, url, bounds }),
  browserBounds: (id: string, bounds: Bounds) => call<void>("browser_bounds", { id, bounds }),
  browserVisible: (id: string, visible: boolean) => call<void>("browser_visible", { id, visible }),
  browserNav: (id: string, action: "back" | "forward" | "reload" | "go", url?: string) => call<void>("browser_nav", { id, action, url: url ?? null }),
  browserClose: (id: string) => call<void>("browser_close", { id }),
  itemActivity: (id: string) => call<Activity[]>("item_activity", { id }),
};

export interface Bounds { x: number; y: number; w: number; h: number }

/** Page loads, address and title changes from the in-app browser's webviews. */
export async function onBrowserEvent(cb: (e: { id: string; kind: "url" | "title" | "loading" | "loaded"; value: string }) => void): Promise<() => void> {
  if (!inTauri) return () => {};
  const { listen } = await import("@tauri-apps/api/event");
  return listen<{ id: string; kind: "url" | "title" | "loading" | "loaded"; value: string }>("tab://browser", (e) => cb(e.payload));
}

export async function openUrl(url: string) {
  // A bare host ("mail.example.com/webmail") from a settings field.
  if (!/^[a-z][a-z0-9+.-]*:/i.test(url)) url = `https://${url}`;
  if (!inTauri) {
    window.open(url, "_blank", "noopener");
    return;
  }
  const { openUrl } = await import("@tauri-apps/plugin-opener");
  await openUrl(url);
}

/** The one-time code `gh auth login` wants typed at github.com/login/device. */
export async function onGhCode(cb: (c: GhLoginCode) => void): Promise<() => void> {
  if (!inTauri) return mockApi.onGhCode(cb);
  const { listen } = await import("@tauri-apps/api/event");
  return listen<GhLoginCode>("tab://gh-code", (e) => cb(e.payload));
}

/** Download progress while `appUpdateInstall` runs. */
export async function onAppUpdateProgress(cb: (p: AppUpdateProgress) => void): Promise<() => void> {
  if (!inTauri) return mockApi.onAppUpdate(cb);
  const { listen } = await import("@tauri-apps/api/event");
  return listen<AppUpdateProgress>("tab://app-update", (e) => cb(e.payload));
}

/** Lines the agent's updater prints while `agentUpgrade` runs. */
export async function onAgentUpgrade(cb: (line: string) => void): Promise<() => void> {
  if (!inTauri) return mockApi.onAgentUpgrade(cb);
  const { listen } = await import("@tauri-apps/api/event");
  return listen<string>("tab://agent-upgrade", (e) => cb(e.payload));
}

export async function onEngineEvent(cb: (e: EngineEvent) => void): Promise<() => void> {
  if (!inTauri) return mockApi.subscribe(cb);
  const { listen } = await import("@tauri-apps/api/event");
  return listen<EngineEvent>("tab://event", (e) => cb(e.payload));
}

/** Clone and checkout progress while a review workspace is prepared. */
export async function onWorkspaceProgress(cb: (p: WorkspaceProgress) => void): Promise<() => void> {
  if (!inTauri) return mockApi.onWorkspace(cb);
  const { listen } = await import("@tauri-apps/api/event");
  return listen<WorkspaceProgress>("tab://workspace", (e) => cb(e.payload));
}

/** Notifications from running language servers (diagnostics, mostly). */
export async function onLsp(cb: (e: LspEvent) => void): Promise<() => void> {
  if (!inTauri) return () => {};
  const { listen } = await import("@tauri-apps/api/event");
  return listen<LspEvent>("tab://lsp", (e) => cb(e.payload));
}
