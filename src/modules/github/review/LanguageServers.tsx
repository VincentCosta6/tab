// Status bar items for language servers, and the dialog that installs them.

import { useEffect, useRef, useState, useSyncExternalStore } from "react";
import { Badge, Button, cx } from "../../../design/components";
import { Icon } from "../../../design/icons";
import { openUrl } from "../../../lib/api";
import type { LspServer } from "../../../lib/types";
import { installServer, LANGUAGE_NAMES, lspStore, refreshServers } from "../lsp";

/** One item per language in the checkout: running, starting, or "Install". */
export function LanguageStatus({ local, sample }: { local: boolean; sample: boolean }) {
  const s = useSyncExternalStore(lspStore.subscribe, lspStore.get);
  const [open, setOpen] = useState(false);
  if (!s.servers) return null;
  const items = s.wanted
    .map((lang) => ({ lang, server: s.servers!.find((x) => x.languages.includes(lang)) }))
    .filter((x): x is { lang: string; server: LspServer } => !!x.server);
  return (
    <>
      {items.map(({ lang, server }) => {
        const run = s.running[server.id];
        const name = LANGUAGE_NAMES[lang] ?? lang;
        let label: string;
        let icon = "code";
        let tone = "";
        if (!server.installed) {
          label = `${name}: install language server`;
          icon = "plus";
          tone = "text-warning";
        } else if (sample) label = `${name}: ${server.command} (desktop app)`;
        else if (!local) label = `${name}: needs a local copy`;
        else if (run?.state === "ready") { label = `${name}: ${server.command}`; icon = "check"; }
        else if (run?.state === "error") { label = `${name}: ${server.command} failed`; icon = "x-circle"; tone = "text-danger"; }
        else { label = `${name}: starting ${server.command}`; icon = "clock"; }
        return (
          <button key={lang} className={cx("rv-status-btn", tone)} onClick={() => setOpen(true)} title={run?.error ?? server.note}>
            <Icon name={icon} size={12} /> {label}
          </button>
        );
      })}
      <button className="rv-status-btn" onClick={() => setOpen(true)} title="Language servers"><Icon name="settings" size={12} /></button>
      {open && <ServersDialog onClose={() => setOpen(false)} />}
    </>
  );
}

function ServersDialog({ onClose }: { onClose: () => void }) {
  const s = useSyncExternalStore(lspStore.subscribe, lspStore.get);
  const close = useRef(onClose);
  close.current = onClose;
  useEffect(() => {
    void refreshServers();
    const esc = (e: KeyboardEvent) => e.key === "Escape" && close.current();
    window.addEventListener("keydown", esc);
    return () => window.removeEventListener("keydown", esc);
  }, []);
  const servers = [...(s.servers ?? [])].sort((a, b) => Number(b.languages.some((l) => s.wanted.includes(l))) - Number(a.languages.some((l) => s.wanted.includes(l))));
  return (
    <div className="rv-modal-backdrop" onMouseDown={(e) => e.target === e.currentTarget && onClose()}>
      <div className="rv-modal rv-lsp" role="dialog" aria-label="Language servers">
        <h3>Language servers</h3>
        <p className="muted">
          TypeScript and JavaScript work out of the box. For other languages, tab runs a language server in the pull request's checkout, so hover, go to definition, references and errors work across the whole repository. Servers install with the language's own tools and run on this computer.
        </p>
        <ul className="rv-lsp-list">
          {servers.map((sv) => {
            const run = s.running[sv.id];
            const used = sv.languages.some((l) => s.wanted.includes(l));
            const err = s.installErrors[sv.id];
            const busy = s.installing[sv.id];
            return (
              <li key={sv.id}>
                <div className="rv-lsp-head">
                  <strong>{sv.name}</strong>
                  {used && <Badge tone="info">In this repo</Badge>}
                  <span className="rv-flex" />
                  {sv.installed ? (
                    <Badge tone={run?.state === "error" ? "danger" : "success"} icon={run?.state === "error" ? "x-circle" : "check"}>
                      {run?.state === "ready" ? "Running" : run?.state === "starting" ? "Starting" : run?.state === "error" ? "Failed" : "Installed"}
                    </Badge>
                  ) : sv.install_command ? (
                    <Button size="sm" variant={used ? "primary" : "secondary"} disabled={busy} onClick={() => installServer(sv.id)}>{busy ? "Installing…" : "Install"}</Button>
                  ) : (
                    <Button size="sm" variant="ghost" icon="external" onClick={() => sv.needs_url && openUrl(sv.needs_url)}>Get {sv.needs}</Button>
                  )}
                </div>
                <div className="subtle">{sv.note}</div>
                {sv.installed && sv.path && <code className="rv-lsp-cmd">{sv.path}</code>}
                {!sv.installed && sv.install_command && <code className="rv-lsp-cmd">$ {sv.install_command}</code>}
                {!sv.installed && !sv.install_command && sv.needs && <div className="subtle">Installing {sv.command} needs {sv.needs} first. Install it, then reopen this dialog.</div>}
                {busy && <div className="subtle">This can take a minute or two while it downloads and builds.</div>}
                {err && <p className="rv-error">{err}</p>}
                {run?.state === "error" && run.error && <p className="rv-error">{run.error}</p>}
              </li>
            );
          })}
        </ul>
        <div className="rv-modal-actions">
          <Button variant="ghost" onClick={onClose}>Done</Button>
        </div>
      </div>
    </div>
  );
}
