import { useEffect, useMemo, useState } from "react";
import { api, type LoginSite, type LoginSource } from "../lib/api";
import { Button, cx } from "./components";
import { Icon } from "./icons";

/** The site part of a page's host, for preselecting it: `gist.github.com` → `github.com`. */
function siteOf(url: string | undefined) {
  try {
    return new URL(url ?? "").hostname.split(".").slice(-2).join(".");
  } catch {
    return "";
  }
}

/**
 * Copies logins (cookies) for chosen sites from another browser into this
 * one. Sits between the address bar and the page, so the page shrinks while it's open.
 */
export function ImportLogins({ url, onClose, onImported }: { url?: string; onClose: () => void; onImported: () => void }) {
  const [sources, setSources] = useState<LoginSource[] | null>(null);
  const [source, setSource] = useState("");
  const [sites, setSites] = useState<LoginSite[] | null>(null);
  const [picked, setPicked] = useState<Set<string>>(new Set());
  const [filter, setFilter] = useState("");
  const [busy, setBusy] = useState(false);
  const [note, setNote] = useState<{ tone: "ok" | "err"; text: string } | null>(null);

  useEffect(() => {
    api.browserLoginSources().then((s) => {
      setSources(s);
      if (s[0]) setSource(s[0].id);
    }, (e) => setNote({ tone: "err", text: String(e) }));
  }, []);

  useEffect(() => {
    if (!source) return;
    setSites(null);
    setNote(null);
    api.browserLoginSites(source).then((s) => {
      setSites(s);
      const here = siteOf(url);
      setPicked(new Set(s.some((x) => x.site === here) ? [here] : []));
    }, (e) => setNote({ tone: "err", text: String(e) }));
  }, [source]);

  const shown = useMemo(() => (sites ?? []).filter((s) => s.site.includes(filter.trim().toLowerCase())), [sites, filter]);
  const toggle = (site: string) => setPicked((p) => {
    const n = new Set(p);
    if (n.has(site)) n.delete(site); else n.add(site);
    return n;
  });

  const run = async () => {
    setBusy(true);
    setNote(null);
    try {
      const n = await api.browserImportLogins(source, [...picked]);
      setNote({ tone: "ok", text: `Imported ${n} cookie${n === 1 ? "" : "s"}. Reloading the page.` });
      onImported();
    } catch (e) {
      setNote({ tone: "err", text: String(e) });
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="blogins" role="dialog" aria-label="Import logins">
      <div className="blogins-head">
        <Icon name="download" size={14} />
        <strong>Import logins</strong>
        {sources && sources.length > 0 && (
          <select value={source} onChange={(e) => setSource(e.target.value)} aria-label="Browser">
            {sources.map((s) => <option key={s.id} value={s.id}>{s.name}</option>)}
          </select>
        )}
        <button className="icon-btn" title="Close" onClick={onClose}><Icon name="x" size={14} /></button>
      </div>
      {sources && sources.length === 0 && <p className="blogins-note">No Chrome, Arc, Brave, Edge, Vivaldi or Firefox profile found on this computer.</p>}
      {sources && sources.length > 0 && (
        <>
          <input className="blogins-filter" value={filter} onChange={(e) => setFilter(e.target.value)} placeholder="Filter sites" spellCheck={false} aria-label="Filter sites" />
          <div className="blogins-list">
            {!sites && !note && <p className="blogins-note">Reading sites…</p>}
            {shown.map((s) => (
              <label key={s.site} className="blogins-site">
                <input type="checkbox" checked={picked.has(s.site)} onChange={() => toggle(s.site)} />
                <span>{s.site}</span>
                <span className="sub">{s.cookies}</span>
              </label>
            ))}
          </div>
          <div className="blogins-foot">
            <span className="sub">Chrome-family browsers: macOS asks once to let tab read their keychain key.</span>
            <Button variant="primary" size="sm" disabled={busy || picked.size === 0} onClick={run}>
              {busy ? "Importing…" : `Import ${picked.size || ""} site${picked.size === 1 ? "" : "s"}`}
            </Button>
          </div>
        </>
      )}
      {note && <p className={cx("blogins-note", note.tone === "err" && "err")}>{note.text}</p>}
    </div>
  );
}

/** "Bring your logins over?", asked once the first time the browser opens. */
export function ImportOffer({ browsers, onAnswer }: { browsers: string[]; onAnswer: (yes: boolean) => void }) {
  const names = browsers.length > 1 ? `${browsers.slice(0, -1).join(", ")} or ${browsers[browsers.length - 1]}` : browsers[0];
  return (
    <div className="blogins" role="dialog" aria-label="Import logins">
      <div className="blogins-head">
        <Icon name="download" size={14} />
        <span>Stay signed in here: import logins for sites you pick from {names}?</span>
      </div>
      <div className="blogins-foot">
        <span className="sub">Nothing is copied until you pick sites. You can change this in Settings → Browser.</span>
        <div className="blogins-actions">
          <Button size="sm" variant="ghost" onClick={() => onAnswer(false)}>Not now</Button>
          <Button size="sm" variant="primary" onClick={() => onAnswer(true)}>Turn on</Button>
        </div>
      </div>
    </div>
  );
}
