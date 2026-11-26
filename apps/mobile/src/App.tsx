// tab on a phone: sign in, see your chats, start one on tab cloud or one of
// your computers, and steer it. Agents never run here; see
// apps/mobile/src-tauri for where they do.

import { useCallback, useEffect, useRef, useState } from "react";
import {
  api, expectLabel, onAccount, onChanged, onEvent,
  type AccountState, type NewChat, type Overview, type Plan, type RunOn, type RunnerView, type Session, type SessionMessage,
} from "./api";

type Screen = { at: "list" } | { at: "new" } | { at: "waiting"; job: string; title: string } | { at: "chat"; session: string };

export function App() {
  const [account, setAccount] = useState<AccountState | null>(null);
  const read = useCallback(() => { api.account().then(setAccount).catch(() => {}); }, []);
  useEffect(() => {
    read();
    const un = onAccount(read);
    return () => { un.then((f) => f()); };
  }, [read]);

  if (!account) return <div className="m-center subtle">…</div>;
  if (!account.account) return <SignIn account={account} />;
  if (!account.connected) {
    return (
      <div className="m-center">
        <p>{account.error ?? "Connecting to your hub…"}</p>
        {account.error && <button className="m-btn" onClick={read}>Try again</button>}
      </div>
    );
  }
  return <Signed />;
}

function SignIn({ account }: { account: AccountState }) {
  const [busy, setBusy] = useState(false);
  const go = (p: "apple" | "google") => { setBusy(true); api.signIn(p).catch(() => setBusy(false)); };
  if (!account.configured) return <div className="m-center">Sign-in isn't set up in this build.</div>;
  return (
    <div className="m-center m-signin">
      <h1>tab</h1>
      <p className="subtle">Sign in to reach your hub in tab cloud. Your chats run on tab cloud or your computers; this phone watches and steers them.</p>
      <button className="m-btn m-primary" disabled={busy} onClick={() => go("apple")}>Continue with Apple</button>
      <button className="m-btn" disabled={busy} onClick={() => go("google")}>Continue with Google</button>
      {account.error && <p className="m-error">{account.error}</p>}
    </div>
  );
}

function Signed() {
  const [screen, setScreen] = useState<Screen>({ at: "list" });
  const [overview, setOverview] = useState<Overview | null>(null);
  const [error, setError] = useState<string | null>(null);
  const refresh = useCallback(() => {
    api.overview().then((o) => { setOverview(o); setError(null); }).catch((e) => setError(String(e)));
  }, []);
  useEffect(() => {
    refresh();
    const uns = [
      onChanged(refresh),
      onEvent((e) => { if (e.type === "session_changed") refresh(); }),
    ];
    // Runners' "awake" ages without any change arriving.
    const t = setInterval(refresh, 30_000);
    return () => { clearInterval(t); uns.forEach((u) => u.then((f) => f())); };
  }, [refresh]);

  switch (screen.at) {
    case "new":
      return <Composer overview={overview} onBack={() => setScreen({ at: "list" })} onStarted={(job, title) => { refresh(); setScreen({ at: "waiting", job, title }); }} />;
    case "waiting":
      return <Waiting job={screen.job} title={screen.title} onBack={() => setScreen({ at: "list" })} onSession={(session) => setScreen({ at: "chat", session })} />;
    case "chat":
      return <Chat id={screen.session} runners={overview?.runners ?? []} onBack={() => { refresh(); setScreen({ at: "list" }); }} />;
    case "list":
      return (
        <div className="m-screen">
          <header className="m-bar"><h1>Chats</h1><button className="m-link" onClick={() => api.signOut()}>Sign out</button></header>
          {error && <p className="m-error">{error}</p>}
          <main className="m-list">
            {overview?.pending.map((p) => (
              <button key={p.job} className="m-row" onClick={() => setScreen({ at: "waiting", job: p.job, title: p.title })}>
                <span className="m-title">{p.title}</span>
                <span className={`m-pill ${p.given_up ? "danger" : "warning"}`}>{p.given_up ? "Not taken" : "Waiting for a runner"}</span>
              </button>
            ))}
            {overview?.sessions.map((s) => (
              <button key={s.id} className="m-row" onClick={() => setScreen({ at: "chat", session: s.id })}>
                <span className="m-title">{s.title}</span>
                <span className="m-meta"><StatusPill status={s.status} /> {runnerName(overview.runners, s.device)}</span>
              </button>
            ))}
            {overview && !overview.sessions.length && !overview.pending.length && <p className="subtle m-pad">No chats yet. Start one and tab hands it to tab cloud or one of your computers.</p>}
          </main>
          <button className="m-fab" onClick={() => setScreen({ at: "new" })}>New chat</button>
        </div>
      );
  }
}

function runnerName(runners: RunnerView[], device: string | null | undefined): string {
  if (!device) return "";
  const r = runners.find((r) => r.id === device);
  return r ? (r.kind === "cloud" ? "tab cloud" : r.name || r.id) : "";
}

function StatusPill({ status }: { status: Session["status"] }) {
  const [label, tone] = {
    idle: ["Idle", "neutral"], working: ["Working", "accent"], needs_input: ["Needs you", "warning"],
    monitoring: ["Watching", "info"], failed: ["Failed", "danger"],
  }[status] ?? [status, "neutral"];
  return <span className={`m-pill ${tone}`}>{label}</span>;
}

/** The choices the Run on picker offers: Auto first, then each runner. */
function choices(o: Overview | null): { on: RunOn; label: string; online?: boolean }[] {
  const out: { on: RunOn; label: string; online?: boolean }[] = [
    { on: { type: "auto" }, label: "Auto" },
  ];
  if (o && o.cloud !== "not_on_plan") out.push({ on: { type: "cloud" }, label: "tab cloud", online: o.cloud === "awake" });
  out.push({ on: { type: "computers" }, label: "Any computer" });
  for (const r of o?.runners ?? []) {
    if (r.kind === "computer") out.push({ on: { type: "runner", id: r.id }, label: r.name || r.id, online: r.awake });
  }
  return out;
}

const same = (a: RunOn, b: RunOn) => a.type === b.type && (a.type !== "runner" || (b.type === "runner" && a.id === b.id));

function Composer({ overview, onBack, onStarted }: { overview: Overview | null; onBack: () => void; onStarted: (job: string, title: string) => void }) {
  const [prompt, setPrompt] = useState("");
  const [repo, setRepo] = useState("");
  const [on, setOn] = useState<RunOn>({ type: "auto" });
  const [plan, setPlan] = useState<Plan | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const chat: NewChat = { prompt, repo, on };

  // Where it would run, as the choice and the repo change.
  useEffect(() => {
    const t = setTimeout(() => { api.planChat({ prompt: "", repo, on }).then(setPlan).catch(() => setPlan(null)); }, 250);
    return () => clearTimeout(t);
  }, [repo, on]);

  const send = () => {
    setBusy(true);
    setError(null);
    api.startChat(chat)
      .then((r) => onStarted(r.job, prompt.trim().split("\n")[0]))
      .catch((e) => { setError(String(e)); setBusy(false); });
  };

  return (
    <div className="m-screen">
      <header className="m-bar"><button className="m-link" onClick={onBack}>Cancel</button><h1>New chat</h1><span /></header>
      <main className="m-pad m-form">
        <textarea autoFocus rows={6} placeholder="What should the agent do?" value={prompt} onChange={(e) => setPrompt(e.target.value)} />
        <input placeholder="Repo (owner/name), optional" autoCapitalize="off" autoCorrect="off" value={repo} onChange={(e) => setRepo(e.target.value)} />
        <div className="m-label">Run on</div>
        <div className="m-choices">
          {choices(overview).map((c) => (
            <button key={c.label + (c.on.type === "runner" ? c.on.id : "")} className={`m-choice ${same(c.on, on) ? "on" : ""}`} onClick={() => setOn(c.on)}>
              {c.online !== undefined && <span className={`m-dot ${c.online ? "online" : ""}`} />}
              {c.label}
            </button>
          ))}
        </div>
        {plan && <p className={`m-plan ${plan.expect.type === "never" ? "m-error" : "subtle"}`}>{expectLabel(plan.expect)}{on.type === "auto" && plan.placement.type === "prefer_cloud" ? `. If tab cloud doesn't start it within ${plan.placement.minutes} min, a computer that's online can.` : ""}</p>}
        {error && <p className="m-error">{error}</p>}
        <button className="m-btn m-primary" disabled={busy || !prompt.trim() || plan?.expect.type === "never"} onClick={send}>Send</button>
      </main>
    </div>
  );
}

function Waiting({ job, title, onBack, onSession }: { job: string; title: string; onBack: () => void; onSession: (s: string) => void }) {
  useEffect(() => {
    let live = true;
    const look = () => api.jobSession(job).then((s) => { if (live && s) onSession(s); }).catch(() => {});
    look();
    const t = setInterval(look, 2000);
    const un = onChanged(look);
    return () => { live = false; clearInterval(t); un.then((f) => f()); };
  }, [job, onSession]);
  return (
    <div className="m-screen">
      <header className="m-bar"><button className="m-link" onClick={onBack}>Chats</button><h1>{title}</h1><span /></header>
      <div className="m-center subtle">Waiting for a runner to start it…</div>
    </div>
  );
}

function Chat({ id, runners, onBack }: { id: string; runners: RunnerView[]; onBack: () => void }) {
  const [session, setSession] = useState<Session | null>(null);
  const [messages, setMessages] = useState<SessionMessage[]>([]);
  const [streaming, setStreaming] = useState("");
  const [text, setText] = useState("");
  const [error, setError] = useState<string | null>(null);
  const end = useRef<HTMLDivElement>(null);

  const read = useCallback(() => {
    api.session(id).then(setSession).catch(() => {});
    api.messages(id).then(setMessages).catch(() => {});
  }, [id]);
  useEffect(() => {
    read();
    const uns = [
      onChanged(read),
      onEvent((e) => {
        if (e.type === "session_delta" && e.session_id === id) setStreaming((s) => s + e.text);
        if (e.type === "session_message" && e.message.session_id === id) {
          setMessages((m) => [...m, e.message]);
          if (e.message.role === "assistant") setStreaming("");
        }
        if (e.type === "session_changed" && e.session.id === id) setSession(e.session);
      }),
    ];
    return () => uns.forEach((u) => u.then((f) => f()));
  }, [id, read]);
  useEffect(() => { end.current?.scrollIntoView({ block: "end" }); }, [messages, streaming]);

  const send = () => {
    const t = text.trim();
    if (!t) return;
    setText("");
    api.steer(id, t).catch((e) => { setError(String(e)); setText(t); });
  };
  const busy = session?.status === "working" || session?.status === "monitoring";

  return (
    <div className="m-screen">
      <header className="m-bar">
        <button className="m-link" onClick={onBack}>Chats</button>
        <h1>{session?.title ?? ""}</h1>
        {busy ? <button className="m-link" onClick={() => api.stop(id)}>Stop</button> : <span />}
      </header>
      <p className="m-where subtle">{session && <><StatusPill status={session.status} /> {runnerName(runners, session.device)}</>}</p>
      <main className="m-messages">
        {messages.map((m) => <Message key={m.id + m.created_at} m={m} />)}
        {streaming && <div className="m-msg assistant">{streaming}</div>}
        <div ref={end} />
      </main>
      {error && <p className="m-error m-pad">{error}</p>}
      <footer className="m-composer">
        <textarea rows={1} placeholder={busy ? "Steer it…" : "Reply…"} value={text} onChange={(e) => setText(e.target.value)} />
        <button className="m-btn m-primary" disabled={!text.trim()} onClick={send}>Send</button>
      </footer>
    </div>
  );
}

function Message({ m }: { m: SessionMessage }) {
  switch (m.kind) {
    case "text": return <div className={`m-msg ${m.role}`}>{m.content}</div>;
    case "tool": return <div className="m-tool subtle">{m.content}</div>;
    case "error": return <div className="m-msg m-error">{m.content}</div>;
    case "status": case "note": return <div className="m-tool subtle">{m.content}</div>;
    default: return null;
  }
}
