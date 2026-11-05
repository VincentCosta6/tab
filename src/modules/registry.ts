// Modules add top-level tabs (and, later, item surfaces, settings panes and
// automation actions). The core app is just modules too, so a connector with
// heavy plugin UI (GitHub's review tool) plugs in the same way: its tab only
// shows while its connector is installed.

import type { ComponentType } from "react";
import type { ConnectorInfo } from "../lib/types";
import { AskTab } from "./ask/AskTab";
import { ConnectionsTab } from "./connections/ConnectionsTab";
import { ReviewsTab } from "./github/ReviewsTab";
import { githubLinks } from "./github/pulls";
import { registerLinks } from "../lib/links";
import { SettingsTab } from "./settings/SettingsTab";
import { WorkTab } from "./work/WorkTab";
import { WorkflowsTab } from "./workflows/WorkflowsTab";

export interface TabDef {
  id: string;
  label: string;
  icon: string;
  component: ComponentType;
  /** Shown in the bottom group of the nav. */
  footer?: boolean;
  shortcut?: string;
  /** Only shown when this returns true (e.g. its connector is installed). */
  when?: (connectors: ConnectorInfo[]) => boolean;
}

export interface TabModule {
  id: string;
  tabs: TabDef[];
}

const installed = (...ids: string[]) => (cs: ConnectorInfo[]) => cs.some((c) => ids.includes(c.id) && c.enabled);

export const modules: TabModule[] = [
  { id: "core.work", tabs: [{ id: "work", label: "Work", icon: "inbox", component: WorkTab, shortcut: "1" }] },
  { id: "core.ask", tabs: [{ id: "ask", label: "Ask", icon: "graph", component: AskTab, shortcut: "2" }] },
  {
    id: "github",
    // Sample data includes PRs, so the review tool can be tried without a token.
    tabs: [{ id: "reviews", label: "Reviews", icon: "git-pull-request", component: ReviewsTab, shortcut: "3", when: installed("github") }],
  },
  { id: "core.workflows", tabs: [{ id: "workflows", label: "Workflows", icon: "workflow", component: WorkflowsTab, shortcut: "5" }] },
  { id: "core.connections", tabs: [{ id: "connections", label: "Connections", icon: "plug", component: ConnectionsTab, shortcut: "4", footer: true }] },
  { id: "core.settings", tabs: [{ id: "settings", label: "Settings", icon: "settings", component: SettingsTab, footer: true }] },
];

export const tabs: TabDef[] = modules.flatMap((m) => m.tabs);

// URLs modules understand, so links render as what they are and open in tab.
registerLinks(githubLinks);
