import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useCallback, useEffect, useRef, useState } from "react";
import { Button } from "../../design/components";
import { Brand } from "../../design/brands";
import { Icon } from "../../design/icons";
import { api, openUrl } from "../../lib/api";
import type { Account, CloudFailure, CloudHub } from "../../lib/types";

export const accountKey = ["account"] as const;

/** Whether this build can sign in, and who's signed in. */
export function useAccount() {
  return useQuery({ queryKey: accountKey, queryFn: api.accountStatus, staleTime: 30_000 });
}

const providers = [
  { id: "apple", label: "Continue with Apple" },
  { id: "google", label: "Continue with Google" },
] as const;

/**
 * Sign in to a tab account with Apple or Google. Opens the browser and waits
 * for it to come back; the session stays in the app, never in the UI.
 */
export function SignInButtons({ onSignedIn }: { onSignedIn?: (a: Account) => void }) {
  const qc = useQueryClient();
  const { data: status } = useAccount();
  const [waiting, setWaiting] = useState<"apple" | "google" | null>(null);
  const [error, setError] = useState<string | null>(null);

  const signIn = async (provider: "apple" | "google") => {
    setError(null);
    setWaiting(provider);
    try {
      const account = await api.accountSignIn(provider);
      qc.setQueryData(accountKey, { configured: true, account });
      onSignedIn?.(account);
    } catch (e) {
      const msg = String(e);
      if (!/cancelled/i.test(msg)) setError(msg);
    } finally {
      setWaiting(null);
    }
  };

  if (status && !status.configured) {
    return <p className="subtle sign-in-note">Sign-in isn't set up in this build of tab. Release builds include it.</p>;
  }
  return (
    <div className="sign-in">
      <div className="sign-in-buttons">
        {providers.map((p) => (
          <Button key={p.id} className={`sign-in-${p.id}`} disabled={!!waiting} onClick={() => signIn(p.id)}>
            <Brand name={p.id} size={16} /> {waiting === p.id ? "Waiting for your browser…" : p.label}
          </Button>
        ))}
      </div>
      {waiting && (
        <span className="subtle sign-in-note">
          Finish signing in in your browser, then come back here.{" "}
          <Button size="sm" variant="ghost" onClick={() => void api.accountCancelSignIn()}>Cancel</Button>
        </span>
      )}
      {error && <small className="text-danger" role="alert">{error}</small>}
    </div>
  );
}

/** Who's signed in: their initial, name or email, and how they signed in. */
export function AccountBadge({ account }: { account: Account }) {
  const label = account.name || account.email || "Your tab account";
  const how = account.provider === "apple" ? "Apple" : account.provider === "google" ? "Google" : account.provider;
  return (
    <div className="account-badge">
      {account.avatar_url ? <img src={account.avatar_url} alt="" /> : <span className="account-initial">{label[0]?.toUpperCase()}</span>}
      <div>
        <strong>{label}</strong>
        <div className="subtle">
          {account.name && account.email ? `${account.email} · ` : ""}Signed in with {how}
        </div>
      </div>
    </div>
  );
}

/** What each kind of hub gives you, said the same way in onboarding and Settings. */
export const cloudPerks = [
  { icon: "lock", title: "Private", text: "Only you can reach your hub, from devices you've signed in on." },
  { icon: "refresh", title: "Syncs your devices", text: "Your queue, chats, projects and workflows follow you to every computer." },
  { icon: "globe", title: "Works with the mobile app", text: "Watch and steer your agents from your phone, wherever you are." },
] as const;

export const localDrawbacks = [
  "No syncing: other computers can't see your queue, chats or projects.",
  "The mobile app only works on the same network, while this computer is on and tab is open.",
  "If this computer is off, asleep or lost, so is your hub. Backups are up to you.",
  "Linking another device means copying an address and certificate fingerprint by hand.",
] as const;

export const cloudHubKey = ["cloud-hub"] as const;

/** Where the signed-in person's private cloud hub is. */
export function useCloudHub(enabled: boolean) {
  return useQuery({ queryKey: cloudHubKey, queryFn: api.cloudHubStatus, enabled, staleTime: 60_000, retry: false });
}

const regions: Record<string, string> = {
  wnam: "Western North America", enam: "Eastern North America", sam: "South America", weur: "Western Europe",
  eeur: "Eastern Europe", afr: "Africa", me: "the Middle East", apac: "Asia-Pacific", oc: "Oceania",
};

/** "in Western Europe", or nothing when Cloudflare picked. */
const whereIs = (hub: CloudHub) => (hub.region && regions[hub.region] ? ` in ${regions[hub.region]}` : "");

/** "Your free trial runs until Nov 21.", or nothing on a paid plan. */
export function planNote(hub: CloudHub): string {
  if (hub.plan.plan !== "trial" || !hub.plan.ends_at) return "";
  const end = new Date(hub.plan.ends_at * 1000).toLocaleDateString(undefined, { month: "short", day: "numeric" });
  return `Your free trial runs until ${end}.`;
}

const asFailure = (e: unknown): CloudFailure =>
  e && typeof e === "object" && "code" in e && "message" in e ? (e as CloudFailure) : { code: "server", message: String(e) };

/** One setup at a time, shared by everything that asks (and by React's dev double mount). */
let settingUp: Promise<CloudHub> | null = null;
const setUpHub = () => (settingUp ??= api.cloudHubSetUp().finally(() => (settingUp = null)));

type Setup = { kind: "working" } | { kind: "ready"; hub: CloudHub } | { kind: "failed"; failure: CloudFailure };

/**
 * Sets up the person's private hub as soon as it's shown, and says how that's
 * going: their own Durable Object with its own database and key, made the
 * first time they pick tab cloud. Calling it again is safe, so a remount or a
 * retry never makes a second hub.
 */
export function CloudHubSetup({ onReady, onNotReady }: { onReady?: (hub: CloudHub) => void; onNotReady?: () => void }) {
  const qc = useQueryClient();
  const [setup, setSetup] = useState<Setup>({ kind: "working" });
  const live = useRef(true);
  const run = useCallback(async () => {
    setSetup({ kind: "working" });
    onNotReady?.();
    try {
      const hub = await setUpHub();
      if (!live.current) return;
      qc.setQueryData(cloudHubKey, hub);
      if (hub.state === "ready") {
        setSetup({ kind: "ready", hub });
        onReady?.(hub);
      } else {
        const message = hub.error ?? (hub.state === "provisioning" ? "Your hub is taking longer than usual to start. Try again in a moment." : "tab couldn't set up your hub.");
        setSetup({ kind: "failed", failure: { code: "server", message } });
      }
    } catch (e) {
      if (live.current) setSetup({ kind: "failed", failure: asFailure(e) });
    }
    // onReady and onNotReady are callbacks from the parent; run once per mount.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [qc]);
  useEffect(() => {
    live.current = true;
    void run();
    return () => void (live.current = false);
  }, [run]);

  if (setup.kind === "working") {
    return (
      <div className="cloud-setup" role="status">
        <span className="step-mark in_progress" />
        <div>
          <strong>Setting up your private hub</strong>
          <p className="subtle">Your own hub, with its own database and encryption key, that only your devices can reach. This takes a few seconds.</p>
        </div>
      </div>
    );
  }
  if (setup.kind === "ready") {
    return (
      <div className="cloud-setup ready" role="status">
        <span className="step-mark completed"><Icon name="check" size={10} /></span>
        <div>
          <strong>Your private hub is ready{whereIs(setup.hub)}</strong>
          {planNote(setup.hub) && <p className="subtle">{planNote(setup.hub)}</p>}
        </div>
      </div>
    );
  }
  const { failure } = setup;
  return (
    <div className="cloud-setup failed" role="alert">
      <Icon name="alert" size={14} />
      <div>
        <strong>{failure.code === "plan_required" ? "tab cloud needs a plan" : "Your hub isn't set up yet"}</strong>
        <p className="subtle">{failure.message}</p>
        <div className="cloud-setup-actions">
          {failure.code === "plan_required" ? (
            <Button size="sm" variant="primary" onClick={() => void openUrl("https://tabqueue.com/pricing")}>See plans</Button>
          ) : failure.code !== "locked" && failure.code !== "not_signed_in" ? (
            <Button size="sm" icon="undo" onClick={() => void run()}>Try again</Button>
          ) : null}
          {failure.code === "plan_required" && <Button size="sm" variant="ghost" onClick={() => void run()}>I've subscribed</Button>}
        </div>
      </div>
    </div>
  );
}

/** One line on the cloud hub for Settings, with a button to set it up when it isn't. */
export function CloudHubLine() {
  const { data: hub, error, isLoading } = useCloudHub(true);
  const [settingUp, setSettingUp] = useState(false);
  if (settingUp) return <CloudHubSetup />;
  if (isLoading) return <span className="subtle">Checking your private hub…</span>;
  if (error) return <span className="subtle">Couldn't check your private hub: {asFailure(error).message}</span>;
  if (hub?.state === "ready") return <span className="subtle">Your private hub is set up{whereIs(hub)}. {planNote(hub)}</span>;
  return (
    <span className="subtle">
      Your private hub isn't set up yet. <Button size="sm" variant="ghost" onClick={() => setSettingUp(true)}>Set it up</Button>
    </span>
  );
}
