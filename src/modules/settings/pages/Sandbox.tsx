import { useQuery } from "@tanstack/react-query";
import { Card, FormRow, Section, TextArea, Toggle } from "../../../design/components";
import { api } from "../../../lib/api";
import type { TabConfig } from "../../../lib/types";
import { useDraft } from "../draft";

/** The sandbox, as a section of Settings → Agents. */
export function SandboxSettings() {
  const { draft, set } = useDraft();
  return <SandboxSection config={draft.sandbox} onChange={(v) => set((c) => void (c.sandbox = v))} />;
}

/** What agents, language servers and workflow commands may touch. */
function SandboxSection({ config, onChange }: { config: TabConfig["sandbox"]; onChange: (v: TabConfig["sandbox"]) => void }) {
  const status = useQuery({ queryKey: ["sandbox"], queryFn: api.sandboxStatus });
  const s = status.data;
  const list = (v: string[]) => v.join("\n");
  return (
    <Section
      title="Sandbox"
      description="Agents, language servers on pull requests, and workflow commands run with limits. Your own chats work as before; they just can't change tab itself or your shell setup, or read stored credentials."
    >
      <Card>
        <FormRow label="On" help="Applies from the next message or run.">
          <Toggle label="Sandbox" checked={config.enabled} onChange={(v) => onChange({ ...config, enabled: v })} />
        </FormRow>
        <FormRow label="On this computer">
          <span className="subtle">{s ? <>{s.summary}{s.fix && <> {s.fix}</>}</> : "Checking…"}</span>
        </FormRow>
        <FormRow label="GitHub in chats" help="gh and git reach GitHub through tab, signed in with your GitHub connection. Force pushes (except to a branch the chat created), merges and deletes ask you first, and agents never see the token.">
          <span className="subtle">Always on while the sandbox is on.</span>
        </FormRow>
        <FormRow label="SSH keys in chats" help="Off keeps ~/.ssh and your SSH agent out of chats, so GitHub is only reachable through tab. Leave on if agents push to other hosts over SSH.">
          <Toggle label="SSH keys in chats" checked={config.ssh_in_chats ?? true} onChange={(v) => onChange({ ...config, ssh_in_chats: v })} />
        </FormRow>
        <FormRow label="Only what's needed" help="Agents can't read, list or change anything in your home folder, other people's or external drives except the chat's own folder, the folders below, its project's repos, toolchains and caches. Your Desktop, Documents, Mail, Photos and other chats stay out of reach.">
          <Toggle label="Only what's needed" checked={config.confine ?? true} onChange={(v) => onChange({ ...config, confine: v })} />
        </FormRow>
        {(config.confine ?? true) && (
          <FormRow label="Folders agents can use" help="Your code folders, one per line. Agents in your own chats can read and change these; agents reviewing someone else's code only get their own checkout.">
            <TextArea rows={4} placeholder={"~/code"} value={list(config.folders ?? [])}
              onChange={(e) => onChange({ ...config, folders: e.target.value.split("\n") })} />
          </FormRow>
        )}
        <FormRow label="Also protect" help="Folders agents may read but not change, one per line. tab's own data and, in a dev build, its checkout are always protected.">
          <TextArea rows={3} placeholder={"~/code/production-config"} value={list(config.protected)}
            onChange={(e) => onChange({ ...config, protected: e.target.value.split("\n") })} />
        </FormRow>
        <FormRow label="Also hide" help="Files or folders agents can't read at all, one per line. Cloud credentials, browser profiles and tab's config are always hidden.">
          <TextArea rows={3} placeholder={"~/.config/acme/credentials"} value={list(config.hidden)}
            onChange={(e) => onChange({ ...config, hidden: e.target.value.split("\n") })} />
        </FormRow>
        <FormRow label="Language servers may reach" help="Hosts code intelligence on a pull request may fetch dependencies from, one per line. *.example.com covers subdomains.">
          <TextArea rows={4} value={list(config.lsp_domains)}
            onChange={(e) => onChange({ ...config, lsp_domains: e.target.value.split("\n") })} />
        </FormRow>
        <FormRow label="Containers" help={s?.container ? `Workflow commands that name an image run in ${s.container}.` : "Workflow commands that name an image need Docker or Podman running."}>
          <select className="select" value={config.container_runtime} onChange={(e) => onChange({ ...config, container_runtime: e.target.value })}>
            <option value="">Whichever is installed</option>
            <option value="docker">Docker</option>
            <option value="podman">Podman</option>
          </select>
        </FormRow>
      </Card>
    </Section>
  );
}
