import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useEffect, useState } from "react";
import { Button, TextInput } from "../../design/components";
import { Icon } from "../../design/icons";
import { api, onGhCode, openUrl } from "../../lib/api";
import type { GhLoginCode, GhStatus } from "../../lib/types";

const ghKey = ["gh"] as const;

/** The GitHub CLI's state on this computer. Always re-checked when shown. */
export function useGh() {
  return useQuery({ queryKey: ghKey, queryFn: api.ghStatus, staleTime: 0, gcTime: 0 });
}

/**
 * Connect GitHub through the GitHub CLI: install it if it's missing, sign in
 * with gh's browser flow, and show who you're signed in as. A personal access
 * token stays available as a fallback.
 */
export function GhConnect({
  token,
  onToken,
  onSignedIn,
}: {
  token: string;
  onToken: (t: string) => void;
  /** Called when gh reports a login: `fresh` is true right after the user signed in or installed here. */
  onSignedIn: (s: GhStatus, fresh: boolean) => void;
}) {
  const qc = useQueryClient();
  const gh = useGh();
  const [code, setCode] = useState<GhLoginCode | null>(null);
  const [copied, setCopied] = useState(false);
  const [useToken, setUseToken] = useState(!!token);

  const done = (s: GhStatus) => {
    qc.setQueryData(ghKey, s);
    setCode(null);
    if (s.logged_in) onSignedIn(s, true);
  };
  const install = useMutation({ mutationFn: api.ghInstall, onSuccess: done });
  const login = useMutation({ mutationFn: api.ghLogin, onSuccess: done, onSettled: () => setCode(null) });

  useEffect(() => {
    let off: (() => void) | undefined;
    onGhCode(setCode).then((f) => (off = f));
    return () => off?.();
  }, []);

  const s = gh.data;
  // Already signed in when this mounts: tell the caller once.
  useEffect(() => {
    if (s?.logged_in) onSignedIn(s, false);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [s?.logged_in, s?.user]);

  const copy = async () => {
    if (!code) return;
    try {
      await navigator.clipboard.writeText(code.code);
      setCopied(true);
      setTimeout(() => setCopied(false), 1500);
    } catch { /* clipboard can be blocked; the code is on screen */ }
  };

  const error = install.error ?? login.error;

  return (
    <div className="gh-connect">
      {gh.isLoading || !s ? (
        <div className="subtle">Looking for the GitHub CLI…</div>
      ) : s.logged_in ? (
        <div className="gh-row text-success">
          <Icon name="check" size={14} /> Signed in{s.user ? <> as <strong>@{s.user}</strong></> : null} through the GitHub CLI
        </div>
      ) : !s.installed ? (
        <>
          <p className="subtle">tab signs in through the GitHub CLI, so there's no token to create. It isn't installed yet.</p>
          <div className="gh-row">
            {s.install_command ? (
              <Button variant="primary" icon="terminal" onClick={() => install.mutate()} disabled={install.isPending}>
                {install.isPending ? "Installing…" : "Install GitHub CLI"}
              </Button>
            ) : (
              <Button variant="primary" icon="external" onClick={() => openUrl("https://cli.github.com")}>Get the GitHub CLI</Button>
            )}
            {s.install_command && <code className="subtle">{s.install_command}</code>}
            <Button variant="ghost" size="sm" icon="undo" onClick={() => gh.refetch()} disabled={gh.isFetching}>Check again</Button>
          </div>
        </>
      ) : code ? (
        <div className="gh-code">
          <p className="subtle">Enter this code on GitHub, then come back. tab picks up the login by itself.</p>
          <div className="gh-row">
            <span className="gh-code-value">{code.code}</span>
            <Button size="sm" icon={copied ? "check" : "copy"} onClick={copy}>{copied ? "Copied" : "Copy"}</Button>
            <Button size="sm" variant="ghost" icon="external" onClick={() => openUrl(code.url)}>Open GitHub</Button>
          </div>
        </div>
      ) : (
        <>
          <p className="subtle">GitHub CLI {s.version} found. Sign in once and tab uses that login.</p>
          <div className="gh-row">
            <Button variant="primary" icon="github" onClick={() => login.mutate()} disabled={login.isPending}>
              {login.isPending ? "Starting…" : "Sign in with GitHub"}
            </Button>
          </div>
        </>
      )}

      {error && <p className="chat-error">{String(error)}</p>}

      {!s?.logged_in && (
        useToken ? (
          <TextInput
            type="password"
            placeholder="Personal access token (notifications, repo read)"
            value={token}
            onChange={(e) => onToken(e.target.value)}
          />
        ) : (
          <button className="link-button subtle" onClick={() => setUseToken(true)}>Use a personal access token instead</button>
        )
      )}
    </div>
  );
}
