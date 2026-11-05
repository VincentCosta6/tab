// Modules add top-level tabs (and, later, item surfaces, settings panes and
// automation actions). The core app is just modules too, so a connector with
// heavy plugin UI (GitHub's review tool) plugs in the same way: its tab only
// shows while its connector is installed.

import type { ComponentType } from "react";
import type { ConnectorInfo } from "../lib/types";
import { ConnectionsTab } from "./connections/ConnectionsTab";
import { ReviewsTab } from "./github/ReviewsTab";
import { SettingsTab } from "./settings/SettingsTab";
import { WorkTab } from "./work/WorkTab";

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
  {
    id: "github",
    // Sample data includes PRs, so the review tool can be tried without a token.
    tabs: [{ id: "reviews", label: "Reviews", icon: "git-pull-request", component: ReviewsTab, shortcut: "2", when: installed("github", "demo") }],
  },
  { id: "core.connections", tabs: [{ id: "connections", label: "Connections", icon: "plug", component: ConnectionsTab, shortcut: "3" }] },
  { id: "core.settings", tabs: [{ id: "settings", label: "Settings", icon: "settings", component: SettingsTab, footer: true }] },
];

export const tabs: TabDef[] = modules.flatMap((m) => m.tabs);
