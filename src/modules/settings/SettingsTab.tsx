import { useEffect, useMemo, useState, type ComponentType } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { keys } from "../../lib/query";
import { Button, cx } from "../../design/components";
import { Icon } from "../../design/icons";
import { api } from "../../lib/api";
import type { TabConfig } from "../../lib/types";
import { useAppUpdate } from "../updates/appUpdate";
import { DraftContext } from "./draft";
import { AgentsPage } from "./pages/Agents";
import { GeneralPage } from "./pages/General";
import { HubPage } from "./pages/Hub";
import { McpPage } from "./pages/Mcp";
import { RankingPage } from "./pages/Ranking";
import { SandboxPage } from "./pages/Sandbox";
import { StoragePage } from "./pages/Storage";
import { UpdatesPage } from "./pages/Updates";

interface Page {
  id: string;
  label: string;
  icon: string;
  component: ComponentType;
  /** The part of the config this page edits, to mark it when it has unsaved changes. */
  edits?: (c: TabConfig) => unknown;
}

// A new settings page is one entry here and one file in pages/.
const pages: Page[] = [
  { id: "general", label: "General", icon: "user", component: GeneralPage, edits: (c) => c.profile },
  { id: "agents", label: "Agents", icon: "bot", component: AgentsPage, edits: (c) => c.agent },
  { id: "ranking", label: "Ranking", icon: "filter", component: RankingPage, edits: (c) => c.ranking },
  { id: "sandbox", label: "Sandbox", icon: "shield", component: SandboxPage, edits: (c) => c.sandbox },
  { id: "storage", label: "Storage", icon: "folder", component: StoragePage, edits: (c) => c.storage },
  { id: "hub", label: "Hub", icon: "cloud", component: HubPage },
  { id: "mcp", label: "MCP server", icon: "terminal", component: McpPage },
  { id: "updates", label: "Updates", icon: "refresh", component: UpdatesPage },
];

const STORE = "tab.settings-page";
let requested: string | null = null;

/** Opens Settings at a page, from anywhere (e.g. "Set limits" opens Agents). */
export function openSettings(page: string) {
  requested = page;
  window.dispatchEvent(new CustomEvent("tab:open-settings", { detail: page }));
}

function initialPage() {
  if (requested) return requested;
  try {
    return localStorage.getItem(STORE) ?? pages[0].id;
  } catch {
    return pages[0].id;
  }
}

export function SettingsTab() {
  const [saved, setSaved] = useState<TabConfig | null>(null);
  const [draft, setDraft] = useState<TabConfig | null>(null);
  const [busy, setBusy] = useState(false);
  const [note, setNote] = useState<string | null>(null);
  const [pageId, setPageId] = useState(initialPage);
  const qc = useQueryClient();
  const { check } = useAppUpdate();

  useEffect(() => {
    api.getConfig().then((c) => (setSaved(c), setDraft(c)));
  }, []);

  useEffect(() => {
    requested = null;
    const open = (e: Event) => { setPageId((e as CustomEvent<string>).detail); requested = null; };
    window.addEventListener("tab:open-settings", open);
    return () => window.removeEventListener("tab:open-settings", open);
  }, []);

  useEffect(() => {
    try { localStorage.setItem(STORE, pageId); } catch { /* per-viewer convenience only */ }
  }, [pageId]);

  // Let a "Saved." note go after a moment; errors stay until the next change.
  useEffect(() => {
    if (!note?.startsWith("Saved")) return;
    const t = setTimeout(() => setNote(null), 4000);
    return () => clearTimeout(t);
  }, [note]);

  const ctx = useMemo(() => draft && {
    draft,
    set: (fn: (c: TabConfig) => void) => {
      const next = structuredClone(draft);
      fn(next);
      setDraft(next);
      setNote(null);
    },
  }, [draft]);

  if (!draft || !saved || !ctx) return <div className="page"><div className="skeleton" /></div>;

  const page = pages.find((p) => p.id === pageId) ?? pages[0];
  const changed = (p: Page) => !!p.edits && JSON.stringify(p.edits(draft)) !== JSON.stringify(p.edits(saved));
  const dirty = JSON.stringify(draft) !== JSON.stringify(saved);

  const save = async () => {
    setBusy(true);
    try {
      const c = await api.saveConfig(draft);
      qc.setQueryData(keys.config, c);
      setSaved(c);
      setDraft(c);
      setNote("Saved. Open items are being re-ranked; unchanged AI verdicts come from cache.");
    } catch (e) {
      setNote(`Couldn't save: ${e}`);
    } finally {
      setBusy(false);
    }
  };

  const Page = page.component;
  return (
    <div className="settings">
      <nav className="settings-nav" aria-label="Settings">
        <h1 className="settings-title">Settings</h1>
        {pages.map((p) => (
          <button key={p.id} className={cx("settings-nav-item", p.id === page.id && "active")} aria-current={p.id === page.id ? "page" : undefined} onClick={() => setPageId(p.id)}>
            <Icon name={p.icon} size={15} />
            <span>{p.label}</span>
            {changed(p) && <span className="settings-nav-dot" title="Unsaved changes" />}
            {p.id === "updates" && check.data && <span className="settings-nav-badge" title={`${check.data.version} available`}>1</span>}
          </button>
        ))}
      </nav>
      <div className="settings-body">
        <div className="settings-page">
          <DraftContext.Provider value={ctx}>
            <Page />
          </DraftContext.Provider>
        </div>
        {(dirty || note) && (
          <div className={cx("settings-savebar", !dirty && "quiet")} role="status">
            <span>{note ?? "You have unsaved changes."}</span>
            {dirty && (
              <div className="settings-savebar-actions">
                <Button size="sm" variant="ghost" disabled={busy} onClick={() => { setDraft(saved); setNote(null); }}>Discard</Button>
                <Button size="sm" variant="primary" disabled={busy} onClick={() => void save()}>{busy ? "Saving…" : "Save changes"}</Button>
              </div>
            )}
          </div>
        )}
      </div>
    </div>
  );
}
