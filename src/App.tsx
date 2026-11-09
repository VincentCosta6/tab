import { setReviewsOpener } from "./modules/github/pulls";
import { BrowserDock } from "./design/BrowserDock";
import { ImageViewer } from "./design/ImageViewer";
import { DropOverlay } from "./modules/work/DropZone";
import { useEffect, useState } from "react";
import { Icon } from "./design/icons";
import { TabMark } from "./design/brands";
import { cx } from "./design/components";
import { ErrorBoundary } from "./design/ErrorBoundary";
import { AppProvider, useApp } from "./lib/app-context";
import { useAgentActivity, useConfig } from "./lib/hooks";
import { QueryProvider } from "./lib/query";
import { Onboarding } from "./modules/onboarding/Onboarding";
import { tabs } from "./modules/registry";
import { UsageMeter } from "./modules/usage/UsageMeter";
import { Updater } from "./modules/updates/Updater";
import { AppUpdater } from "./modules/updates/AppUpdater";
import { openSettings } from "./modules/settings/SettingsTab";
import { AppHeader } from "./modules/shell/AppHeader";

export default function App() {
  return (
    <QueryProvider>
      <AppProvider>
        <ErrorBoundary label="tab">
          <Shell />
          <ImageViewer />
        </ErrorBoundary>
      </AppProvider>
    </QueryProvider>
  );
}

function Shell() {
  const { connectors } = useApp();
  const { data: config } = useConfig();
  const visible = tabs.filter((t) => !t.when || t.when(connectors));
  const [active, setActive] = useState(() => new URLSearchParams(location.search).get("tab") ?? tabs[0].id);
  const [onboarding, setOnboarding] = useState(false);
  const tab = visible.find((t) => t.id === active) ?? visible[0];

  useEffect(() => {
    if (config && !config.onboarded && new URLSearchParams(location.search).get("onboarding") !== "skip") setOnboarding(true);
  }, [config]);

  useEffect(() => {
    const work = () => setActive("work");
    window.addEventListener("tab:open-item", work);
    return () => window.removeEventListener("tab:open-item", work);
  }, []);

  // GitHub PR links open in Reviews while that tab is available.
  const reviews = visible.some((t) => t.id === "reviews");
  useEffect(() => {
    setReviewsOpener(reviews ? () => setActive("reviews") : null);
    return () => setReviewsOpener(null);
  }, [reviews]);

  useEffect(() => {
    const settings = () => setActive("settings");
    window.addEventListener("tab:open-settings", settings);
    return () => window.removeEventListener("tab:open-settings", settings);
  }, []);

  useEffect(() => {
    const open = () => setOnboarding(true);
    window.addEventListener("tab:onboarding", open);
    return () => window.removeEventListener("tab:onboarding", open);
  }, []);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (!(e.metaKey || e.ctrlKey)) return;
      const t = visible.find((t) => t.shortcut === e.key);
      if (t) {
        e.preventDefault();
        setActive(t.id);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [visible]);

  const navButton = (t: (typeof tabs)[number]) => (
    <button key={t.id} className={cx("nav-item", t.id === tab.id && "active")} onClick={() => setActive(t.id)} title={t.shortcut ? `${t.label} (Ctrl ${t.shortcut})` : t.label}>
      <span className="nav-icon">
        {t.id === "work" ? <TabMark size={22} /> : <Icon name={t.icon} size={18} />}
        {t.id === "work" && <AgentBadge />}
      </span>
      <span>{t.label}</span>
    </button>
  );

  const Active = tab.component;
  return (
    <div className="shell">
      <AppHeader title={tab.label} icon={tab.icon} />
      <nav className="nav">
        <div className="nav-group">{visible.filter((t) => !t.footer).map(navButton)}</div>
        <div className="nav-group nav-footer">
          <AppUpdater />
          <Updater />
          <UsageMeter onSettings={() => openSettings("agents")} />
          {visible.filter((t) => t.footer).map(navButton)}
        </div>
      </nav>
      <main className="main">
        <div className="main-body">
          <ErrorBoundary key={tab.id} label={tab.label}>
            <Active />
          </ErrorBoundary>
        </div>
      </main>
      <BrowserDock />
      <DropOverlay />
      {onboarding && <Onboarding onClose={() => setOnboarding(false)} />}
    </div>
  );
}

/** On Work: how many agents need the person (amber) or are working (pulse). */
function AgentBadge() {
  const activity = useAgentActivity();
  const all = [...activity.values()];
  const needs = all.filter((a) => a.status === "needs_input").length;
  const working = all.filter((a) => a.status === "working").length;
  if (needs) return <span className="nav-badge needs" title={`${needs} agent${needs > 1 ? "s" : ""} need${needs > 1 ? "" : "s"} you`}>{needs}</span>;
  if (working) return <span className="nav-badge working" title={`${working} agent${working > 1 ? "s" : ""} working`} />;
  return null;
}
