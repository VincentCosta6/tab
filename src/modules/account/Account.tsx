import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useState } from "react";
import { Button } from "../../design/components";
import { Brand } from "../../design/brands";
import { api } from "../../lib/api";
import type { Account } from "../../lib/types";

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
