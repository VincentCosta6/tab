// What a pasted link or label is, so adding a resource is mostly pasting.

import type { ProjectResource, ResourceAccess, ResourceKind, Tone } from "../../lib/types";

export const kindLook: Record<ResourceKind, { label: string; plural: string; icon: string }> = {
  website: { label: "Website", plural: "Websites", icon: "globe" },
  service: { label: "Service", plural: "Services", icon: "cloud" },
  link: { label: "Link", plural: "Links", icon: "link" },
  email: { label: "Email", plural: "Email", icon: "mail" },
  file: { label: "File", plural: "Files", icon: "file" },
  note: { label: "Note", plural: "Notes", icon: "edit" },
};

export const kindOrder: ResourceKind[] = ["website", "service", "link", "email", "file", "note"];

export const accessLook: Record<ResourceAccess, { label: string; help: string; tone: Tone; icon?: string }> = {
  all: { label: "All threads", help: "Every chat in this project can read it.", tone: "neutral" },
  coordinator: { label: "Coordinator only", help: "Only the project's coordinator reads it. Other threads ask you first.", tone: "warning" },
  vault: { label: "Vault", help: "End-to-end encrypted, read only by the coordinator after you unlock it. Coming soon.", tone: "danger", icon: "lock" },
};

export const statusLook: Record<ProjectResource["status"], { label: string; tone: Tone; help: string }> = {
  pending: { label: "Not indexed yet", tone: "neutral", help: "Waiting to be read. Agents can open the link meanwhile." },
  indexed: { label: "Indexed", tone: "success", help: "Its contents are searchable." },
  live: { label: "Live", tone: "success", help: "Agents read it live through its connection." },
  failed: { label: "Failed", tone: "danger", help: "The last read failed; what was indexed before is still searchable." },
  needs_sign_in: { label: "Needs sign-in", tone: "warning", help: "Connect the service so agents can read it." },
};

/** Services tab knows by their links: the name to show and what agents can do with them. */
const services: { host: RegExp; name: string; title?: (u: URL) => string | null; watch?: string }[] = [
  {
    host: /(^|\.)dash\.cloudflare\.com$/,
    name: "Cloudflare",
    title: (u) => {
      const m = u.pathname.match(/\/(pages|workers)\/(?:view|services\/view)\/([^/]+)/);
      return m ? `Cloudflare · ${m[2]}` : null;
    },
    watch: "Watch deploys and put failures in the project queue",
  },
  { host: /(^|\.)dashboard\.stripe\.com$/, name: "Stripe" },
  { host: /(^|\.)vercel\.com$/, name: "Vercel", watch: "Watch deploys and put failures in the project queue" },
  { host: /(^|\.)app\.netlify\.com$/, name: "Netlify", watch: "Watch deploys and put failures in the project queue" },
  { host: /(^|\.)console\.aws\.amazon\.com$/, name: "AWS" },
  { host: /(^|\.)console\.cloud\.google\.com$/, name: "Google Cloud" },
  { host: /(^|\.)linear\.app$/, name: "Linear" },
  { host: /(^|\.)sentry\.io$/, name: "Sentry", watch: "Put new issues in the project queue" },
  { host: /(^|\.)app\.posthog\.com$|(^|\.)posthog\.com$/, name: "PostHog" },
  { host: /(^|\.)supabase\.com$/, name: "Supabase" },
  { host: /(^|\.)fly\.io$/, name: "Fly.io" },
];

/** Documents that need a sign-in to read. */
const docs: { host: RegExp; name: string }[] = [
  { host: /^docs\.google\.com$/, name: "Google Doc" },
  { host: /^drive\.google\.com$/, name: "Google Drive" },
  { host: /(^|\.)notion\.(so|site)$/, name: "Notion" },
  { host: /(^|\.)dropbox\.com$/, name: "Dropbox" },
  { host: /(^|\.)sharepoint\.com$|^onedrive\.live\.com$/, name: "OneDrive" },
  { host: /(^|\.)slack\.com$/, name: "Slack" },
  { host: /(^|\.)figma\.com$/, name: "Figma" },
];

export interface Detected {
  kind: ResourceKind;
  title: string;
  url: string;
  /** For repos: add it as a repo instead. */
  repo?: string;
  /** A sentence on what tab recognised. */
  what: string;
  /** A watch option the service supports. */
  watch?: string;
}

/**
 * What `text` is: a GitHub repo, a known service, a document, a website, or
 * an email label or sender. Null when it isn't any of them.
 */
export function detect(text: string): Detected | null {
  const t = text.trim();
  if (!t) return null;
  const label = t.match(/^label:\s*(.+)$/i);
  if (label) return { kind: "email", title: `Email · ${label[1]}`, url: `label:${label[1].trim()}`, what: "Email with this label, from your email connection" };
  const from = t.match(/^(?:from:\s*)?([\w.+-]+@[\w-]+(\.[\w-]+)+)$/i);
  if (from) return { kind: "email", title: `Email from ${from[1]}`, url: `from:${from[1].toLowerCase()}`, what: "Email from this sender, from your email connection" };
  const domainOnly = /^[\w-]+(\.[\w-]+)+(\/\S*)?$/.test(t);
  let u: URL;
  try {
    u = new URL(domainOnly ? `https://${t}` : t);
  } catch {
    return null;
  }
  if (!/^https?:$/.test(u.protocol)) return null;
  const host = u.hostname.replace(/^www\./, "");
  const url = u.toString().replace(/\/$/, "");

  if (host === "github.com") {
    const [owner, name] = u.pathname.split("/").filter(Boolean);
    if (owner && name) return { kind: "link", title: `${owner}/${name}`, url, repo: `${owner}/${name.replace(/\.git$/, "")}`, what: "A GitHub repo: added to the project's repos, where agents work" };
  }
  const svc = services.find((s) => s.host.test(host));
  if (svc) return { kind: "service", title: svc.title?.(u) ?? svc.name, url, what: `${svc.name}, read live through its connection`, watch: svc.watch };
  const doc = docs.find((d) => d.host.test(host));
  if (doc) return { kind: "link", title: doc.name, url, what: `A ${doc.name} link: agents read it through your ${doc.name} connection` };
  const path = u.pathname.replace(/\/$/, "");
  return { kind: "website", title: path && path !== "/" ? `${host}${path}` : host, url, what: "A website: its pages are kept fresh and searchable" };
}
