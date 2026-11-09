// The bar across the top of the app, which is also the window's title bar:
// where you are, where your work is kept and whether that hub is reachable,
// and your account. Where a chat runs is shown beside it (DevicePill).

import { useEffect, useRef, useState, type ReactNode } from "react";
import { createPortal } from "react-dom";
import { useQuery } from "@tanstack/react-query";
import { Badge, cx } from "../../design/components";
import { Icon } from "../../design/icons";
import { api, inTauri } from "../../lib/api";
import { bytes, timeAgo } from "../../lib/format";
import type { HubStatus, Runner, Session, Tone } from "../../lib/types";
import { useAccount } from "../account/Account";
import { openSettings } from "../settings/SettingsTab";

export function AppHeader({ title, icon }: { title: string; icon: string }) {
  const hub = useQuery({ queryKey: ["hub-status"], queryFn: api.hubStatus, refetchInterval: 5000 });
  const runners = useQuery({ queryKey: ["runners"], queryFn: api.runners, refetchInterval: 30_000 });
  // The window's own title bar: drag it to move the window, double-click to
  // zoom (Tauri handles both on the drag regions). On macOS the traffic lights sit over its left end; on Windows tab
  // draws the window buttons at its right end.
  return (
    <header className={cx("app-header", `os-${os}`)} data-tauri-drag-region>
      <div className="app-header-title" data-tauri-drag-region>
        <Icon name={icon} size={15} />
        <span data-tauri-drag-region>{title}</span>
      </div>
      <div className="app-header-status">
        <HubChip hub={hub.data ?? null} loading={hub.isLoading} runners={runners.data ?? []} />
        <AccountChip />
      </div>
      {os === "windows" && <WindowButtons />}
    </header>
  );
}

/** The OS tab draws its title bar for: macOS (traffic lights inset) or Windows (no native frame). */
const os: "macos" | "windows" | "other" = !inTauri
  ? ((new URLSearchParams(location.search).get("os") as "macos" | "windows" | null) ?? "other") // ?os= previews it in a browser
  : /Mac/.test(navigator.userAgent) ? "macos" : /Windows/.test(navigator.userAgent) ? "windows" : "other";

/** This window, loaded on first use like the rest of the Tauri API. */
const win = async () => (inTauri ? (await import("@tauri-apps/api/window")).getCurrentWindow() : null);

/** Minimize, maximize and close, for Windows, where tab has no native frame. */
function WindowButtons() {
  const [max, setMax] = useState(false);
  useEffect(() => {
    let off: (() => void) | undefined;
    let live = true;
    void win().then(async (w) => {
      if (!w || !live) return;
      setMax(await w.isMaximized());
      const f = await w.onResized(() => void w.isMaximized().then(setMax));
      if (live) off = f; else f();
    });
    return () => { live = false; off?.(); };
  }, []);
  return (
    <div className="window-buttons">
      <button aria-label="Minimize" onClick={() => void win().then((w) => w?.minimize())}>
        <svg width="10" height="10" viewBox="0 0 10 10" aria-hidden="true"><path d="M0 5h10" stroke="currentColor" /></svg>
      </button>
      <button aria-label={max ? "Restore" : "Maximize"} onClick={() => void win().then((w) => w?.toggleMaximize())}>
        <svg width="10" height="10" viewBox="0 0 10 10" aria-hidden="true">
          {max ? <path d="M2.5 2.5V.5h7v7h-2M.5 2.5h7v7h-7z" fill="none" stroke="currentColor" /> : <path d="M.5.5h9v9h-9z" fill="none" stroke="currentColor" />}
        </svg>
      </button>
      <button className="close" aria-label="Close" onClick={() => void win().then((w) => w?.close())}>
        <svg width="10" height="10" viewBox="0 0 10 10" aria-hidden="true"><path d="M0 0l10 10M10 0L0 10" stroke="currentColor" /></svg>
      </button>
    </div>
  );
}

interface Look { tone: Tone; label: string; detail: string }

/**
 * Where the work is kept, and whether it's reachable right now. On tab
 * cloud that's one of: syncing, connecting (just started, no error yet),
 * signed out, or can't reach it (with why, from the app).
 */
export function hubLook(hub: HubStatus | null, runners: Runner[], signedIn = true): Look & { icon: string; place: string } {
  if (!hub) return { icon: "server", place: "Hub", tone: "danger", label: "Not running", detail: "This computer's hub didn't start. Restart tab." };
  const cloud = runners.find((r) => r.kind === "cloud");
  const others = runners.filter((r) => r.kind === "computer" && !r.this_app && r.awake).length;
  const devices = others ? ` ${others} other computer${others > 1 ? "s are" : " is"} awake.` : "";
  if (hub.cloud_hub) {
    const runner = cloud ? (cloud.awake ? " The cloud runner is awake." : " The cloud runner is asleep; it starts when there's work.") : "";
    const waiting = `Changes wait here${hub.pending ? ` (${hub.pending.toLocaleString()} so far)` : ""} and sync once it's back.`;
    const place = { icon: "cloud", place: "tab cloud" };
    if (hub.cloud_connected && !hub.cloud_error) {
      return hub.pending
        ? { ...place, tone: "info", label: "Syncing", detail: `Sending ${hub.pending.toLocaleString()} change${hub.pending > 1 ? "s" : ""} to tab cloud.${devices}${runner}` }
        : { ...place, tone: "success", label: "Synced", detail: `Your work is up to date with tab cloud.${devices}${runner}` };
    }
    if (hub.cloud_connected) return { ...place, tone: "warning", label: "Sync failing", detail: `Connected, but syncing failed; tab keeps retrying. ${waiting}` };
    if (!signedIn) return { ...place, tone: "warning", label: "Signed out", detail: `Sign in again to sync. ${waiting}` };
    if (!hub.cloud_error) return { ...place, tone: "info", label: "Connecting", detail: `Connecting to tab cloud. ${waiting}` };
    return { ...place, tone: "warning", label: "Offline", detail: `Can't reach tab cloud; tab keeps trying. ${waiting}` };
  }
  return { icon: "monitor", place: "This computer", tone: "success", label: "Running", detail: `Your hub runs inside this app, so it's awake while tab is open.${devices}` };
}

/** The hub chip: click for Settings → Hub, hover (or focus) for a card on how the hub is doing. */
function HubChip({ hub, loading, runners }: { hub: HubStatus | null; loading: boolean; runners: Runner[] }) {
  const { data: account } = useAccount();
  const ref = useRef<HTMLButtonElement>(null);
  const [at, setAt] = useState<DOMRect | null>(null);
  const timer = useRef<number>(undefined);
  useEffect(() => () => window.clearTimeout(timer.current), []);
  if (loading) return null;
  // Signed out only matters once the account status is in.
  const l = hubLook(hub, runners, !account || !!account.account || !account.configured);
  const show = (delay: number) => {
    window.clearTimeout(timer.current);
    timer.current = window.setTimeout(() => ref.current && setAt(ref.current.getBoundingClientRect()), delay);
  };
  const hide = () => {
    window.clearTimeout(timer.current);
    timer.current = window.setTimeout(() => setAt(null), 150);
  };
  const width = 320;
  return (
    <>
      <button
        ref={ref}
        className="header-chip"
        onClick={() => { window.clearTimeout(timer.current); setAt(null); openSettings("hub"); }}
        aria-label={`${l.place}: ${l.label}. Open Settings → Hub`}
        onMouseEnter={() => show(250)}
        onMouseLeave={hide}
        onFocus={() => show(0)}
        onBlur={hide}
      >
        <Icon name={l.icon} size={14} />
        <span className="header-chip-place">{l.place}</span>
        <span className={cx("header-chip-state", `tone-${l.tone}`)}><i className="header-dot" aria-hidden="true" />{l.label}</span>
      </button>
      {at && createPortal(
        <div
          className="link-card hub-card"
          role="tooltip"
          style={{ width, top: at.bottom + 6, left: Math.max(8, Math.min(at.right - width, window.innerWidth - width - 8)) }}
          onMouseEnter={() => window.clearTimeout(timer.current)}
          onMouseLeave={hide}
        >
          <HubCard hub={hub} runners={runners} look={l} />
        </div>,
        document.body,
      )}
    </>
  );
}

/** How the hub is doing, what it holds, and which devices are around. */
function HubCard({ hub, runners, look: l }: { hub: HubStatus | null; runners: Runner[]; look: ReturnType<typeof hubLook> }) {
  const facts: [string, ReactNode][] = [];
  if (hub) {
    if (hub.cloud_hub) {
      facts.push(["Syncing", hub.pending ? <span className="tone-text-warning">{hub.pending.toLocaleString()} change{hub.pending > 1 ? "s" : ""} waiting</span> : hub.cloud_connected ? "Up to date" : "Nothing waiting"]);
      if (hub.cloud_error && !hub.cloud_connected) facts.push(["Why", <span className="hub-card-error">{hub.cloud_error}</span>]);
      const held = (hub.synced ?? []).reduce((n, c) => n + c.count, 0);
      facts.push(["Here", `${held.toLocaleString()} things synced to this computer`]);
    } else {
      const others = hub.connections.filter((c) => !c.this_app);
      const behind = others.filter((c) => c.behind > 0).length;
      if (hub.started_at) facts.push(["Up", since(hub.started_at)]);
      facts.push(["Address", hub.url ? <span className="mono">{hub.url}{hub.http3 ? " · HTTP/3" : ""}</span> : "Not listening"]);
      facts.push(["Connected", others.length ? `${others.length} device${others.length > 1 ? "s" : ""}${behind ? `, ${behind} catching up` : ", all up to date"}` : "Only this app"]);
      if (hub.pending) facts.push(["Syncing", <span className="tone-text-warning">{hub.pending.toLocaleString()} waiting</span>]);
    }
    // The app's own hub stops changing once the work moves to tab cloud,
    // so its numbers would only mislead there.
    if (!hub.cloud_hub) {
      facts.push(["Holds", `${hub.records.toLocaleString()} records · ${bytes(hub.storage_bytes)}`]);
      if (hub.files) facts.push(["Files", `${hub.files.toLocaleString()} · ${bytes(hub.file_bytes)}`]);
    }
    if (!hub.cloud_hub && (hub.cpu_percent != null || hub.memory_bytes != null)) {
      const cpu = hub.cpu_percent == null ? null : hub.cpu_percent / Math.max(1, hub.cpus);
      facts.push(["App uses", [cpu != null && `${cpu.toFixed(1)}% CPU`, hub.memory_bytes != null && bytes(hub.memory_bytes)].filter(Boolean).join(" · ")]);
    }
  }
  const devices = [...runners].sort((a, b) => Number(b.this_app) - Number(a.this_app) || Number(b.awake) - Number(a.awake));
  return (
    <>
      <div className="link-card-head">
        <Icon name={l.icon} size={14} />
        <span>{l.place}</span>
        <Badge tone={l.tone}>{l.label}</Badge>
      </div>
      <span className="link-card-meta">{l.detail}</span>
      {!!facts.length && <dl className="link-card-facts hub-card-facts">{facts.map(([k, v]) => <div key={k}><dt>{k}</dt><dd>{v}</dd></div>)}</dl>}
      {!!devices.length && (
        <ul className="hub-card-devices">
          {devices.map((r) => {
            const awake = r.this_app || r.awake;
            return (
              <li key={r.id}>
                <span className={cx("header-chip-state", `tone-${awake ? "success" : "neutral"}`)}><i className="header-dot" aria-hidden="true" /></span>
                <Icon name={r.kind === "cloud" ? "cloud" : "monitor"} size={12} />
                <span className="hub-card-device-name">{r.this_app ? "This computer" : r.kind === "cloud" ? "Cloud runner" : r.name || "Another computer"}</span>
                <span className="hub-card-device-state">{awake ? "Awake" : r.seen_at ? `Seen ${ago(r.seen_at)}` : "Asleep"}</span>
              </li>
            );
          })}
        </ul>
      )}
      <span className="link-card-hint">Click for Settings → Hub</span>
    </>
  );
}

/** "3h ago", or "just now". */
function ago(iso: string): string {
  const t = timeAgo(iso);
  return /^\d+[mhd]$/.test(t) ? `${t} ago` : t;
}

/** How long the hub has been up: "for 3h", or "just now". */
function since(iso: string): string {
  const t = timeAgo(iso);
  return /^\d+[mhd]$/.test(t) ? `for ${t}` : t;
}

/**
 * Where a chat runs and whether it's connected. A chat runs on one device
 * (this computer, another one, or, later, tab cloud); others watch and steer
 * it through the hub, so it's live only while that device is awake and the
 * hub can reach it.
 */
export function sessionLook(s: Session, runners: Runner[], hub: HubStatus | null): Look & { icon: string; place: string } {
  const me = runners.find((r) => r.this_app);
  const here = !s.device || s.device === me?.id;
  if (here) return { icon: "monitor", place: "This computer", tone: "success", label: "Connected", detail: "This chat runs on this computer." };
  const r = runners.find((x) => x.id === s.device);
  const place = r ? (r.kind === "cloud" ? "tab cloud" : r.name || "Another computer") : "Another device";
  const icon = r?.kind === "cloud" ? "cloud" : "monitor";
  if (hub?.cloud_hub && !hub.cloud_connected) return { icon, place, tone: "warning", label: "Offline", detail: `It runs on ${place}, and tab cloud can't be reached from here right now.` };
  if (!r) return { icon, place, tone: "neutral", label: "Unknown", detail: "It runs on a device this hub hasn't heard from." };
  return r.awake
    ? { icon, place, tone: "success", label: "Connected", detail: `It runs on ${place}. You can watch and steer it from here.` }
    : { icon, place, tone: "neutral", label: "Asleep", detail: `It runs on ${place}, last heard from ${r.seen_at ? timeAgo(r.seen_at) : "a while ago"}. It carries on when that device wakes.` };
}

function AccountChip() {
  const { data } = useAccount();
  const a = data?.account;
  const label = a ? a.name || a.email || "Your account" : "Sign in";
  return (
    <button className="header-account" onClick={() => openSettings("account")} title={a ? `${label} · Settings → Account` : "Sign in to tab cloud"}>
      {a?.avatar_url ? <img src={a.avatar_url} alt="" referrerPolicy="no-referrer" /> : <span className="account-initial">{a ? label[0]?.toUpperCase() : <Icon name="user" size={13} />}</span>}
    </button>
  );
}
