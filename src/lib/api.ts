// The one place the UI talks to the backend. In a plain browser (vite dev
// without Tauri) it falls back to an in-memory mock so the UI can be built
// and screenshotted without the desktop shell.

import type {
  AgentKind, AgentProbe, AuditEntry, Harness, ModelOption, Chat, ChatStarted, ConnectorInfo, ContextEntry, ContextSection, EngineEvent, GhLoginCode, GhStatus,
  AgentActivity, ContextStatus, DoneGuard, GraphNode, GraphView, LinkSummary, MyPull, IndexStatus, ItemContext, ItemDetail, Project, ProjectNote, ProjectRepo, ProjectResource, ResourceAccess, ResourceInput, ItemState, DepsStatus, LspEvent, LspServer, LspStarted, NewContext, NewTask, Pressure, Preview, PrRef, PullReview, QueueItem, SlashCommand, Subagent,
  RateLimitState, ReviewAction, HubStatus, MoveOutcome, SandboxStatus, Session, StorageUsage, Swept, ReviewWorkspace, SearchHit, SearchQuery, TabConfig, UsageRow, UsageSummary, WorkspaceProgress,
  Activity, Issue, Workflow, WorkflowRun, AppVersion, AppUpdate, AppUpdateProgress, Account, AccountStatus, Job, Runner, RunsOn,
  AgentMcpServer, McpCatalogEntry, McpCloudState, McpImportCandidate, McpImportResult, McpProbe, McpServerInfo, McpSignInEvent, McpTool, ToolPolicy, JobGaps,
  EmailSignInStatus, MailProvider,
} from "./types";
import { mockApi } from "./mock";
import { HubListSchema, HubStatusSchema, RunnerStatusSchema, decode } from "./protocol/cloud";

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
  /** Per path, `[modified ms, size]`, or null once the file is gone. */
  fileStamps: (paths: string[]) => call<([number, number] | null)[]>("file_stamps", { paths }),
  fileOpen: (path: string, reveal = false) => call<void>("file_open", { path, reveal }),
  startDictation: () => call<void>("start_dictation"),
  agentActivity: () => call<AgentActivity[]>("agent_activity"),
  contextStatus: (ids: string[]) => call<Record<string, ContextStatus[]>>("context_status", { ids }),
  slashCommands: () => call<SlashCommand[]>("agent_slash_commands"),
  subagents: () => call<Subagent[]>("agent_subagents"),
  /** Values for a slash command argument from a chat's folder: its git branches, or the paths `typed` could go on to. */
  slashArgValues: (cwd: string, kind: "branch" | "path", typed: string) => call<string[]>("slash_arg_values", { cwd, kind, typed }),
  /** The MCP servers a chat's agent reported at its last turn. */
  agentMcpServers: (sessionId: string) => call<AgentMcpServer[]>("agent_mcp_servers", { sessionId }),
  graphView: (projectId: string | null, days: number | null, limit?: number) => call<GraphView>("graph_view", { projectId, days, limit: limit ?? null }),
  graphNode: (id: number) => call<GraphNode>("graph_node", { id }),
  graphDelete: (id: number, withExclusive: boolean) => call<number>("graph_delete", { id, withExclusive }),
  coordinatorChatOpen: (projectId: string) => call<Chat>("coordinator_chat_open", { projectId }),
  coordinatorChatSend: (projectId: string, text: string, attachments: string[] = []) => call<Session>("coordinator_chat_send", { projectId, text, attachments }),
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
  projectSetProfile: (id: string, p: { about?: string; rules?: string }) =>
    call<Project | null>("project_set_profile", { id, about: p.about ?? null, rules: p.rules ?? null }),
  projectRepos: (projectId: string) => call<ProjectRepo[]>("project_repos", { projectId }),
  projectRepoSave: (projectId: string, r: { repo: string; path?: string | null; note?: string | null }) =>
    call<ProjectRepo>("project_repo_save", { projectId, repo: r.repo, path: r.path ?? null, note: r.note ?? null }),
  projectRepoDelete: (projectId: string, id: string) => call<void>("project_repo_delete", { projectId, id }),
  projectResources: (projectId: string) => call<ProjectResource[]>("project_resources", { projectId }),
  projectResourceSave: (projectId: string, resource: ResourceInput) => call<ProjectResource>("project_resource_save", { projectId, resource }),
  projectResourceDelete: (projectId: string, id: string) => call<boolean>("project_resource_delete", { projectId, id }),
  projectResourceReindex: (id: string) => call<void>("project_resource_reindex", { id }),
  /** Puts a file in the hub and adds it to the project's Files. */
  projectFileAdd: async (projectId: string, name: string, bytes: Uint8Array, access: ResourceAccess): Promise<ProjectResource> => {
    if (!inTauri) return mockApi.invoke<ProjectResource>("project_file_add", { projectId, name, bytes, access });
    const { invoke } = await import("@tauri-apps/api/core");
    const headers = { "x-project": encodeURIComponent(projectId), "x-name": encodeURIComponent(name), "x-access": access };
    return invoke<ProjectResource>("project_file_add", bytes, { headers });
  },
  projectItems: (projectId: string) => call<QueueItem[]>("project_items", { projectId }),
  reviewChatOpen: (repo: string, number: number) => call<Chat>("review_chat_open", { repo, number }),
  reviewChatSend: (target: { repo: string; number: number; title: string; url: string; head_ref: string; base_ref: string }, text: string, attachments: string[] = []) =>
    call<Session>("review_chat_send", { target, text, attachments }),
  /** Sets `agent`'s model. Before a chat starts, `forNewChats` also makes `agent` the harness new chats use. */
  setSessionModel: (sessionId: string, model: string | null) => call<Session>("set_session_model", { sessionId, model }),
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
  /** An image's bytes at a commit, for the review's image diff: from the local clone, or the API. */
  reviewFileBytes: (repo: string, sha: string, path: string, local: boolean) => call<ArrayBuffer>("review_file_bytes", { repo, sha, path, local }),
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
  /** The signed-in person's personal cloud hub. Rejects with a CloudFailure. */
  cloudHubStatus: async () => decode(HubStatusSchema, await call<ArrayBuffer>("cloud_hub_status")),
  /** Every cloud hub they belong to, personal first. Rejects with a CloudFailure. */
  cloudHubs: async () => decode(HubListSchema, await call<ArrayBuffer>("cloud_hubs")),
  /** Sets up (or retries) their personal hub; resolves once it's ready or failed. Rejects with a CloudFailure. */
  cloudHubSetUp: async () => decode(HubStatusSchema, await call<ArrayBuffer>("cloud_hub_set_up")),
  /** Moves this computer's work into their private cloud hub and syncs with it from then on. Rejects with a CloudFailure. */
  hubMoveToCloud: () => call<MoveOutcome>("hub_move_to_cloud"),
  /** Brings the work back from tab cloud into this computer's own hub and stops syncing. Resolves with the rows copied. */
  hubMoveToLocal: () => call<number>("hub_move_to_local"),
  /** This computer, the person's other computers and the cloud runner, as the hub knows them. */
  runners: () => call<Runner[]>("runners"),
  /** Jobs workflows handed to runners, newest first. */
  jobsRecent: () => call<Job[]>("jobs_recent"),
  /** Services tab knows the MCP server of. */
  mcpCatalog: () => call<McpCatalogEntry[]>("mcp_catalog"),
  /** Connected remote MCP servers and whether each is ready. */
  mcpServers: () => call<McpServerInfo[]>("mcp_servers"),
  /** What's at a URL: an MCP server, and does it want a sign-in? */
  mcpProbe: (url: string) => call<McpProbe>("mcp_probe", { url }),
  /** Adds a server. Resolves with the sign-in page to open, when it wants one. */
  mcpAdd: (a: { id: string; name: string; url: string; signIn: boolean; catalog: string | null }) =>
    call<string | null>("mcp_add", a),
  /** Starts a sign-in again. Resolves with the page to open; `onMcpSignIn` says when it's done. */
  mcpSignIn: (id: string) => call<string>("mcp_sign_in", { id }),
  mcpSignOut: (id: string) => call<void>("mcp_sign_out", { id }),
  /** Which mail providers this build can sign in with, and who's signed in. */
  emailSignInStatus: () => call<EmailSignInStatus>("email_sign_in_status"),
  /** Starts "Sign in with Google / Microsoft". Resolves with the page to open; tab sets up the email connection when the browser comes back. */
  emailSignIn: (provider: MailProvider) => call<string>("email_sign_in", { provider }),
  emailSignOut: () => call<void>("email_sign_out"),
  mcpRemove: (id: string) => call<void>("mcp_remove", { id }),
  mcpSetEnabled: (id: string, enabled: boolean) => call<void>("mcp_set_enabled", { id, enabled }),
  /** Whether its work fills the queue, and what it watches (for recipes that ask). */
  mcpSetQueue: (id: string, on: boolean, query: string | null) => call<void>("mcp_set_queue", { id, on, query }),
  /** A server's tools with the person's choices; `refresh` asks the server again. */
  mcpTools: (id: string, refresh: boolean) => call<McpTool[]>("mcp_tools", { id, refresh }),
  /** Allowed, ask or off for one tool; null goes back to the default. */
  mcpSetTool: (id: string, tool: string, policy: ToolPolicy | null) => call<void>("mcp_set_tool", { id, tool, policy }),
  /** Where each connection stands for tab cloud jobs (asks the hub which cloud sign-ins it keeps). */
  mcpCloudStates: () => call<McpCloudState[]>("mcp_cloud_states"),
  /** "Allow in cloud" for a connection. Resolves with the tab cloud sign-in page to open, when it needs one. */
  mcpSetCloud: (id: string, on: boolean) => call<string | null>("mcp_set_cloud", { id, on }),
  /** Starts a connection's sign-in for tab cloud (a grant of its own). Resolves with the page to open. */
  mcpCloudSignIn: (id: string) => call<string>("mcp_cloud_sign_in", { id }),
  /** Allowed or off for one tool in cloud jobs; null goes back to the default. */
  mcpSetCloudTool: (id: string, tool: string, policy: ToolPolicy | null) => call<void>("mcp_set_cloud_tool", { id, tool, policy }),
  /** Recent cloud jobs that needed a connection or tool they didn't have. */
  cloudJobGaps: () => call<JobGaps[]>("cloud_job_gaps"),
  /** MCP servers set up in Claude Code, with what tab already has marked. */
  mcpImportScan: () => call<McpImportCandidate[]>("mcp_import_scan"),
  /** Copies the chosen Claude Code servers (by key) into tab. */
  mcpImport: (keys: string[]) => call<McpImportResult[]>("mcp_import", { keys }),
  /** Sets where a connection runs and saves the config. Rejects with a CloudFailure. */
  connectionRunsOn: (connection: string, runsOn: RunsOn) => call<TabConfig>("connection_runs_on", { connection, runsOn }),
  /** The hub's cloud runner. Rejects with a CloudFailure. */
  cloudRunnerStatus: async () => decode(RunnerStatusSchema, await call<ArrayBuffer>("cloud_runner_status")),
  /** Starts the hub's cloud runner now. Rejects with a CloudFailure. */
  cloudRunnerWake: async () => decode(RunnerStatusSchema, await call<ArrayBuffer>("cloud_runner_wake")),
  /** Names (never values) of the secrets the cloud hub keeps for its runner. Rejects with a CloudFailure. */
  cloudSecretNames: () => call<string[]>("cloud_secret_names"),
  /** Stores, or with null removes, a harness key (`harness/<id>/<ENV>`) or git access (`git/token`). Rejects with a CloudFailure. */
  cloudSecretSet: (name: string, value: string | null) => call<string[]>("cloud_secret_set", { name, value }),
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

/** A remote MCP sign-in finished in the browser (or failed). */
export async function onMcpSignIn(cb: (e: McpSignInEvent) => void): Promise<() => void> {
  if (!inTauri) return mockApi.onMcpSignIn(cb);
  const { listen } = await import("@tauri-apps/api/event");
  return listen<McpSignInEvent>("tab://mcp-sign-in", (e) => cb(e.payload));
}

/** Download progress while `appUpdateInstall` runs. */
export async function onAppUpdateProgress(cb: (p: AppUpdateProgress) => void): Promise<() => void> {
  if (!inTauri) return mockApi.onAppUpdate(cb);
  const { listen } = await import("@tauri-apps/api/event");
  return listen<AppUpdateProgress>("tab://app-update", (e) => cb(e.payload));
}

/** `[done, total]` changes while `hubMoveToCloud` runs. */
export async function onHubMove(cb: (p: [number, number]) => void): Promise<() => void> {
  if (!inTauri) return () => {};
  const { listen } = await import("@tauri-apps/api/event");
  return listen<[number, number]>("tab://hub-move", (e) => cb(e.payload));
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
