import { Card, FormRow, Section, Toggle } from "../../../design/components";
import { useDraft } from "../draft";

/** The in-app browser. */
export function BrowserPage() {
  const { draft, set } = useDraft();
  const on = draft.browser?.import_logins === true;
  const agents = draft.browser?.agents === "read_only";
  return (
    <Section title="Browser" description="The browser beside your queue, for opening links without leaving tab.">
      <Card>
        <FormRow
          label="Import logins"
          help="Adds a button to the browser bar that copies the cookies for sites you pick from Chrome, Arc, Brave, Edge, Vivaldi or Firefox, so you're signed in here too. Nothing is copied until you pick sites. For Chrome-family browsers, macOS asks before tab can read their keychain key."
        >
          <Toggle label="Import logins" checked={on} onChange={(v) => set((c) => void (c.browser = { ...c.browser, import_logins: v }))} />
        </FormRow>
      </Card>
      <Card>
        <FormRow
          label="Let agents browse"
          help="Agents in chats you drive on this computer can open public pages beside the chat and read them, to research how to set something up. They browse logged out and read-only: no sign-ins, clicks or typing, and secrets on a page are hidden from them. Cloud jobs, workflows and other people's PRs never can."
        >
          <Toggle label="Let agents browse" checked={agents} onChange={(v) => set((c) => void (c.browser = { ...c.browser, agents: v ? "read_only" : "off" }))} />
        </FormRow>
      </Card>
    </Section>
  );
}
