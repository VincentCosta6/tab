import { useQueryClient } from "@tanstack/react-query";
import { useState } from "react";
import { Button, Card, FormRow, Section, TextInput } from "../../../design/components";
import { api } from "../../../lib/api";
import { AccountBadge, SignInButtons, accountKey, useAccount } from "../../account/Account";
import { useDraft } from "../draft";

/** Who you are: your tab account, and how the rules and the AI layer know you. */
export function AccountPage() {
  return (
    <>
      <SignedIn />
      <Profile />
    </>
  );
}

/** The tab account your cloud hub belongs to, or a way to sign in. */
function SignedIn() {
  const qc = useQueryClient();
  const { data: status } = useAccount();
  const [busy, setBusy] = useState(false);
  if (!status) return null;
  if (!status.configured) return null;
  const signOut = async () => {
    setBusy(true);
    try {
      await api.accountSignOut();
      qc.setQueryData(accountKey, { configured: status.configured, account: null });
    } finally {
      setBusy(false);
    }
  };
  return (
    <Section
      title="Your tab account"
      description={status.account ? "tab cloud keeps your hub under it. Sign in with it on another computer to see the same work." : "Sign in to keep your work in tab cloud and reach it from your other computers and your phone. tab works without it."}
    >
      <Card>
        {status.account ? (
          <div className="hub-row">
            <AccountBadge account={status.account} />
            <Button size="sm" variant="ghost" disabled={busy} onClick={signOut}>{busy ? "Signing out…" : "Sign out"}</Button>
          </div>
        ) : (
          <div style={{ maxWidth: 320 }}><SignInButtons /></div>
        )}
      </Card>
    </Section>
  );
}

function Profile() {
  const { draft, set } = useDraft();
  return (
    <Section title="About you" description="Used by the rules (mentions of your handles) and given to the AI layer as context.">
      <Card>
        <FormRow label="Name">
          <TextInput value={draft.profile.name} onChange={(e) => set((c) => void (c.profile.name = e.target.value))} />
        </FormRow>
        <FormRow label="Role" help="e.g. Staff engineer on payments">
          <TextInput value={draft.profile.role} onChange={(e) => set((c) => void (c.profile.role = e.target.value))} />
        </FormRow>
        <FormRow label="Handles" help="Your usernames elsewhere, comma separated">
          <TextInput
            value={draft.profile.handles.join(", ")}
            onChange={(e) => set((c) => void (c.profile.handles = e.target.value.split(",").map((h) => h.trim()).filter(Boolean)))}
          />
        </FormRow>
      </Card>
    </Section>
  );
}
