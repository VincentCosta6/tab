import { useEffect, useState } from "react";
import { Icon } from "./design/icons";
import { TabMark } from "./design/brands";
import { cx } from "./design/components";
import { AppProvider, useApp } from "./lib/app-context";
import { useConfig } from "./lib/hooks";
import { QueryProvider } from "./lib/query";
import { Onboarding } from "./modules/onboarding/Onboarding";
import { tabs } from "./modules/registry";
import { UsageMeter } from "./modules/usage/UsageMeter";

export default function App() {
  return (
    <QueryProvider>
      <AppProvider>
        <Shell />
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
      <Icon name={t.icon} size={18} />
      <span>{t.label}</span>
    </button>
  );

  const Active = tab.component;
  return (
    <div className="shell">
      <nav className="nav">
        <div className="nav-brand" title="TAB Annihilates Backlogs"><TabMark size={26} /></div>
        <div className="nav-group">{visible.filter((t) => !t.footer).map(navButton)}</div>
        <div className="nav-group nav-footer">
          <UsageMeter onSettings={() => setActive("settings")} />
          {visible.filter((t) => t.footer).map(navButton)}
        </div>
      </nav>
      <main className="main">
        <Active />
      </main>
      {onboarding && <Onboarding onClose={() => setOnboarding(false)} />}
    </div>
  );
}
