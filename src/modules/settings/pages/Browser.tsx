import { Card, FormRow, Section, Toggle } from "../../../design/components";
import { useDraft } from "../draft";

/** The in-app browser. */
export function BrowserPage() {
  const { draft, set } = useDraft();
  const on = draft.browser?.import_logins === true;
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
    </Section>
  );
}
