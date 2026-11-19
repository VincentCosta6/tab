import { Card, FormRow, Section, TextInput, Toggle } from "../../../design/components";
import { useDraft } from "../draft";

/** The in-app browser. */
export function BrowserPage() {
  const { draft, set } = useDraft();
  const on = draft.browser?.import_logins === true;
  const level = draft.browser?.agents ?? "off";
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
          label="Agents in the browser"
          help="What agents in chats you drive on this computer may do in the browser beside the chat. Cloud jobs, workflows and other people's PRs never can. Agents never see secrets and never type passwords; deleting, paying or granting access always asks you."
        >
          <select className="select" value={level} onChange={(e) => set((c) => void (c.browser = { ...c.browser, agents: e.target.value as typeof level }))}>
            <option value="off">Off</option>
            <option value="read_only">Read only: open and read pages, logged out</option>
            <option value="plan">Plan approval: you approve a plan once, matching steps go through</option>
            <option value="ask">Ask for everything: every click and typed value asks</option>
          </select>
        </FormRow>
        {(level === "plan" || level === "ask") && (
          <FormRow label="Second check" help="A quick check by your agent's small model that a plan and any loose match fit what you asked. It only ever adds a question; it sees your messages and the plan, never the page.">
            <Toggle label="Second check" checked={!draft.browser?.agent_critic_off} onChange={(v) => set((c) => void (c.browser = { ...c.browser, agent_critic_off: !v }))} />
          </FormRow>
        )}
        {level !== "off" && (
          <FormRow label="Dev servers" help="Ports on this computer agents may open, for your own dev servers (e.g. 1430, 3000). Nothing else on this computer or your network, and never tab's own.">
            <TextInput defaultValue={(draft.browser?.agent_dev_ports ?? []).join(", ")} placeholder="None" onBlur={(e) => set((c) => void (c.browser = { ...c.browser, agent_dev_ports: e.target.value.split(/[\s,]+/).map(Number).filter((n) => Number.isInteger(n) && n > 0 && n < 65536) }))} />
          </FormRow>
        )}
      </Card>
    </Section>
  );
}
