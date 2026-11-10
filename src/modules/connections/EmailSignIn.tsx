// "Sign in with Google / Microsoft" for the email connection: OAuth in the
// browser instead of an app password. tab sets up the connection (server,
// address) when the browser comes back; the sign-in stays in the vault.

import { useEffect, useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Button, Card, Section } from "../../design/components";
import { Brand } from "../../design/brands";
import { api, openUrl } from "../../lib/api";
import type { MailProvider } from "../../lib/types";

export const emailSignInKey = ["email-sign-in"] as const;

/** Whether the email connection signs in with Google or Microsoft rather than a password. */
export function usesMailSignIn(settings: Record<string, unknown> | undefined) {
  return settings?.auth === "google" || settings?.auth === "microsoft";
}

export function EmailSignIn({ auth }: { auth: unknown }) {
  const qc = useQueryClient();
  const [waiting, setWaiting] = useState<MailProvider | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const { data } = useQuery({
    queryKey: [...emailSignInKey, auth],
    queryFn: api.emailSignInStatus,
    refetchInterval: waiting ? 1500 : false,
  });
  const signed = data?.signed_in && data.signed_in.provider === auth ? data.signed_in : null;

  useEffect(() => {
    if (waiting && signed?.provider === waiting) setWaiting(null);
  }, [waiting, signed]);

  const offered = (data?.providers ?? []).filter((p) => p.available);
  if (!signed && !offered.length) return null;

  const start = async (p: MailProvider) => {
    setError(null);
    try {
      const page = await api.emailSignIn(p);
      setWaiting(p);
      await openUrl(page);
    } catch (e) {
      setError(String(e));
    }
  };

  return (
    <Section title="Sign in">
      <Card>
        <div className="mail-sign-in">
          {signed ? (
            <div className="mail-signed-in">
              <Brand name={signed.provider} size={18} />
              <div style={{ flex: 1, minWidth: 0 }}>
                <strong>{signed.address}</strong>
                <div className="subtle">Signed in with {signed.provider === "google" ? "Google" : "Microsoft"}. No password is kept.</div>
              </div>
              <Button
                size="sm"
                disabled={busy}
                onClick={async () => {
                  setBusy(true);
                  try {
                    await api.emailSignOut();
                    await qc.invalidateQueries({ queryKey: emailSignInKey });
                  } catch (e) {
                    setError(String(e));
                  } finally {
                    setBusy(false);
                  }
                }}
              >
                Sign out
              </Button>
            </div>
          ) : (
            <>
              <div className="toolbar">
                {offered.map((p) => (
                  <Button key={p.id} onClick={() => start(p.id)} disabled={!!waiting}>
                    <Brand name={p.id} size={15} /> Sign in with {p.name}
                  </Button>
                ))}
              </div>
              <small className="subtle">
                {waiting
                  ? "Finish signing in in your browser. tab sets up the connection when you're back."
                  : "Gmail, Google Workspace, Outlook and Microsoft 365: no app password needed. Any other host: fill in the settings below."}
              </small>
            </>
          )}
          {error && <small className="text-danger">{error}</small>}
        </div>
      </Card>
    </Section>
  );
}
