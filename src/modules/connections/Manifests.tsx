import { useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Badge, Button, Card, Section } from "../../design/components";
import { api } from "../../lib/api";
import { keys } from "../../lib/query";
import type { ManifestSource, ManifestStatus, MarketListingView, MarketPreview } from "../../lib/types";

const sourceLabel: Record<ManifestSource, string> = { built_in: "Built in", yours: "Yours", marketplace: "Marketplace" };

/** After the manifests change: the catalog, the servers' queue settings and this list are stale. */
function useRefresh() {
  const qc = useQueryClient();
  return (status?: ManifestStatus) => {
    if (status) qc.setQueryData(keys.manifests, status);
    void qc.invalidateQueries({ queryKey: keys.manifests });
    void qc.invalidateQueries({ queryKey: keys.mcpCatalog });
    void qc.invalidateQueries({ queryKey: keys.mcpServers });
    void qc.invalidateQueries({ queryKey: keys.marketplace });
  };
}

/** Connections → Installed: the manifests tab runs, the person's folder, and what didn't load. */
export function ManifestsSection() {
  const { data } = useQuery({ queryKey: keys.manifests, queryFn: api.manifests });
  const refresh = useRefresh();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const run = async (f: () => Promise<ManifestStatus | void>) => {
    setBusy(true);
    setError(null);
    try {
      refresh((await f()) ?? undefined);
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };
  if (!data) return null;
  const theirs = data.manifests.filter((m) => m.source !== "built_in");
  return (
    <Section
      title="Manifests"
      description={<>Each service is a manifest: what agents can use, and how it fills your queue. Add your own as JSON files in your manifest folder, then reload. See <code>docs/recipe-manifests.md</code>.</>}
      actions={
        <>
          <Button size="sm" variant="ghost" icon="folder" disabled={busy} onClick={() => void run(api.manifestsOpenFolder)}>Open folder</Button>
          <Button size="sm" icon="undo" disabled={busy} onClick={() => void run(api.manifestsReload)}>Reload</Button>
        </>
      }
    >
      <Card padded={false}>
        {theirs.length === 0 && data.errors.length === 0 && (
          <div className="mcp-tools-empty subtle">Only the built-in manifests. {data.dir && <>Yours go in <code>{data.dir}</code>.</>}</div>
        )}
        {theirs.map((m) => (
          <div key={m.id} className="mcp-tool">
            <div className="mcp-tool-main">
              <code>{m.id}</code>
              <span>{m.name}</span>
              {m.path && <span className="subtle mcp-tool-desc" title={m.path}>{m.path}</span>}
            </div>
            {m.plugin && <Badge tone="warning" title="Runs a program from your folder, in the sandbox">plugin</Badge>}
            {m.replaces_built_in && <Badge tone="info">replaces built-in</Badge>}
            <Badge tone={m.source === "marketplace" ? "accent" : "neutral"}>{sourceLabel[m.source]}</Badge>
          </div>
        ))}
        {data.errors.map((e) => (
          <div key={e.path} className="mcp-tool">
            <div className="mcp-tool-main">
              <code title={e.path}>{e.path.split(/[\\/]/).pop()}</code>
              <span className="chat-error mcp-tool-desc" title={e.error}>{e.error}</span>
            </div>
            <Badge tone="danger">didn't load</Badge>
          </div>
        ))}
      </Card>
      {error && <p className="chat-error">{error}</p>}
    </Section>
  );
}

/** Connections → Marketplace: signed manifests to install. */
export function MarketplaceSection() {
  const listings = useQuery({ queryKey: keys.marketplace, queryFn: api.marketplaceListings, retry: false, staleTime: 5 * 60_000 });
  const refresh = useRefresh();
  const [preview, setPreview] = useState<MarketPreview | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const run = async (id: string, f: () => Promise<void>) => {
    setBusy(id);
    setError(null);
    try {
      await f();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(null);
    }
  };
  const ask = (l: MarketListingView) => run(l.id, async () => setPreview(await api.marketplacePreview(l)));
  const install = (p: MarketPreview) =>
    run(p.listing.id, async () => {
      refresh(await api.marketplaceInstall(p.listing));
      setPreview(null);
    });
  const remove = (l: MarketListingView) => run(l.id, async () => refresh(await api.marketplaceUninstall(l.id)));

  return (
    <Section title="Community manifests" description="Signed by tab and checked before they install. They only read: they can't change anything in the service, and can't run programs.">
      <Card padded={false}>
        {listings.isLoading && <div className="mcp-tools-empty subtle">Looking at the marketplace…</div>}
        {listings.error && <div className="mcp-tools-empty subtle">{String(listings.error)}</div>}
        {listings.data?.length === 0 && <div className="mcp-tools-empty subtle">Nothing here yet.</div>}
        {listings.data?.map((l) => (
          <div key={l.id}>
            <div className="mcp-tool">
              <div className="mcp-tool-main">
                <span>{l.name}</span>
                <span className="subtle mcp-tool-desc" title={l.description}>{l.description}</span>
              </div>
              <span className="subtle">{l.author ? `${l.author} · ` : ""}v{l.version}</span>
              {l.installed ? (
                <>
                  {l.installed !== l.version && <Button size="sm" disabled={busy === l.id} onClick={() => void ask(l)}>Update</Button>}
                  <Button size="sm" variant="ghost" disabled={busy === l.id} onClick={() => void remove(l)}>Remove</Button>
                </>
              ) : (
                <Button size="sm" disabled={busy === l.id} onClick={() => void ask(l)}>Install</Button>
              )}
            </div>
            {preview?.listing.id === l.id && (
              <div className="mcp-tool">
                <div className="mcp-tool-main" style={{ flexDirection: "column", alignItems: "flex-start" }}>
                  <span>
                    Checked: signed by tab. It will call {preview.calls.length ? preview.calls.map((c, i) => <span key={c}>{i > 0 && ", "}<code>{c}</code></span>) : "nothing"} on <code>{preview.url}</code>
                    {preview.replaces_built_in && ", in place of tab's built-in manifest"}.
                  </span>
                </div>
                <Button size="sm" variant="ghost" onClick={() => setPreview(null)}>Cancel</Button>
                <Button size="sm" variant="primary" disabled={busy === l.id} onClick={() => void install(preview)}>Install</Button>
              </div>
            )}
          </div>
        ))}
      </Card>
      {error && <p className="chat-error">{error}</p>}
    </Section>
  );
}
