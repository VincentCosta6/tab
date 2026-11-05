// Modules add top-level tabs (and, later, item surfaces, settings panes and
// automation actions). The core app is just modules too, so a connector with
// heavy plugin UI (GitHub's review tool) plugs in the same way.

import type { ComponentType } from "react";
import { WorkTab } from "./work/WorkTab";
import { ConnectionsTab } from "./connections/ConnectionsTab";
import { SettingsTab } from "./settings/SettingsTab";

export interface TabDef {
  id: string;
  label: string;
  icon: string;
  component: ComponentType;
  /** Shown in the bottom group of the nav. */
  footer?: boolean;
  shortcut?: string;
}

export interface TabModule {
  id: string;
  tabs: TabDef[];
}

export const modules: TabModule[] = [
  { id: "core.work", tabs: [{ id: "work", label: "Work", icon: "inbox", component: WorkTab, shortcut: "1" }] },
  { id: "core.connections", tabs: [{ id: "connections", label: "Connections", icon: "plug", component: ConnectionsTab, shortcut: "2" }] },
  { id: "core.settings", tabs: [{ id: "settings", label: "Settings", icon: "settings", component: SettingsTab, footer: true }] },
];

export const tabs: TabDef[] = modules.flatMap((m) => m.tabs);
