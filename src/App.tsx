import { useEffect, useState } from "react";
import { Icon } from "./design/icons";
import { cx } from "./design/components";
import { AppProvider } from "./lib/app-context";
import { tabs } from "./modules/registry";

export default function App() {
  const [active, setActive] = useState(() => new URLSearchParams(location.search).get("tab") ?? tabs[0].id);
  const tab = tabs.find((t) => t.id === active) ?? tabs[0];

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (!(e.metaKey || e.ctrlKey)) return;
      const t = tabs.find((t) => t.shortcut === e.key);
      if (t) {
        e.preventDefault();
        setActive(t.id);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  const navButton = (t: (typeof tabs)[number]) => (
    <button key={t.id} className={cx("nav-item", t.id === tab.id && "active")} onClick={() => setActive(t.id)} title={t.shortcut ? `${t.label} (⌘${t.shortcut})` : t.label}>
      <Icon name={t.icon} size={18} />
      <span>{t.label}</span>
    </button>
  );

  const Active = tab.component;
  return (
    <AppProvider>
      <div className="shell">
        <nav className="nav">
          <div className="nav-brand">tab</div>
          <div className="nav-group">{tabs.filter((t) => !t.footer).map(navButton)}</div>
          <div className="nav-group nav-footer">{tabs.filter((t) => t.footer).map(navButton)}</div>
        </nav>
        <main className="main">
          <Active />
        </main>
      </div>
    </AppProvider>
  );
}
