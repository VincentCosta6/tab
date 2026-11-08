// The bar across the top of the app: where you are, where your work is kept
// and whether that hub is reachable, where the chat on screen runs and
// whether it's connected, and your account.

import { useQuery } from "@tanstack/react-query";
import { cx } from "../../design/components";
import { Icon } from "../../design/icons";
import { api } from "../../lib/api";
import { timeAgo } from "../../lib/format";
import type { HubStatus, Runner, Session, Tone } from "../../lib/types";
import { useViewedSession } from "../../lib/viewing";
import { useAccount } from "../account/Account";
import { openSettings } from "../settings/SettingsTab";

export function AppHeader({ title, icon }: { title: string; icon: string }) {
  const hub = useQuery({ queryKey: ["hub-status"], queryFn: api.hubStatus, refetchInterval: 5000 });
  const runners = useQuery({ queryKey: ["runners"], queryFn: api.runners, refetchInterval: 30_000 });
  const session = useViewedSession();
  return (
    <header className="app-header">
      <div className="app-header-title">
        <Icon name={icon} size={15} />
        <span>{title}</span>
      </div>
      <div className="app-header-status">
        {session && <SessionChip session={session} runners={runners.data ?? []} hub={hub.data ?? null} />}
        <HubChip hub={hub.data ?? null} loading={hub.isLoading} runners={runners.data ?? []} />
        <AccountChip />
      </div>
    </header>
  );
}

interface Look { tone: Tone; label: string; detail: string }

/** Where the work is kept, and whether it's reachable right now. */
export function hubLook(hub: HubStatus | null, runners: Runner[]): Look & { icon: string; place: string } {
  if (!hub) return { icon: "server", place: "Hub", tone: "danger", label: "Not running", detail: "This computer's hub didn't start. Restart tab." };
  const cloud = runners.find((r) => r.kind === "cloud");
  const others = runners.filter((r) => r.kind === "computer" && !r.this_app && r.awake).length;
  const devices = others ? ` ${others} other computer${others > 1 ? "s are" : " is"} awake.` : "";
  if (hub.cloud_hub) {
    const runner = cloud ? (cloud.awake ? " The cloud runner is awake." : " The cloud runner is asleep; it starts when there's work.") : "";
    return hub.cloud_connected
      ? { icon: "cloud", place: "tab cloud", tone: "success", label: "Connected", detail: `Your work syncs with tab cloud.${devices}${runner}` }
      : { icon: "cloud", place: "tab cloud", tone: "warning", label: "Offline", detail: `Can't reach tab cloud. Changes wait here${hub.pending ? ` (${hub.pending.toLocaleString()} so far)` : ""} and sync when it reconnects.` };
  }
  return { icon: "monitor", place: "This computer", tone: "success", label: "Running", detail: `Your hub runs inside this app, so it's awake while tab is open.${devices}` };
}

function HubChip({ hub, loading, runners }: { hub: HubStatus | null; loading: boolean; runners: Runner[] }) {
  if (loading) return null;
  const l = hubLook(hub, runners);
  return (
    <button className="header-chip" onClick={() => openSettings("hub")} title={`${l.detail}\nOpen Settings → Hub`}>
      <Icon name={l.icon} size={14} />
      <span className="header-chip-place">{l.place}</span>
      <span className={cx("header-chip-state", `tone-${l.tone}`)}><i className="header-dot" aria-hidden="true" />{l.label}</span>
    </button>
  );
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

function SessionChip({ session, runners, hub }: { session: Session; runners: Runner[]; hub: HubStatus | null }) {
  const l = sessionLook(session, runners, hub);
  return (
    <span className="header-chip static" title={l.detail}>
      <Icon name="bot" size={14} />
      <span className="header-chip-place">Chat on {l.place === "This computer" ? "this computer" : l.place}</span>
      <span className={cx("header-chip-state", `tone-${l.tone}`)}><i className="header-dot" aria-hidden="true" />{l.label}</span>
    </span>
  );
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
