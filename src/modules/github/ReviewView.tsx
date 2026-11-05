// The GitHub module's review tool: a VS Code editor (Monaco) with
// side-by-side diffs of every changed file, existing review comments, and
// line comments posted back to the PR. File contents are fetched by commit
// SHA through the gateway, so each file costs one request ever.

import { DiffEditor, type DiffOnMount } from "@monaco-editor/react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useEffect, useMemo, useRef, useState } from "react";
import { Badge, Button, cx } from "../../design/components";
import { Brand } from "../../design/brands";
import { Icon } from "../../design/icons";
import { api, openUrl } from "../../lib/api";
import { keys } from "../../lib/query";
import type { PrFile, PullReview, ReviewComment } from "../../lib/types";
import { languageFor, monaco } from "./monaco";

function useDark() {
  const q = typeof window !== "undefined" ? window.matchMedia("(prefers-color-scheme: dark)") : null;
  const [dark, setDark] = useState(!!q?.matches);
  useEffect(() => {
    if (!q) return;
    const on = () => setDark(q.matches);
    q.addEventListener("change", on);
    return () => q.removeEventListener("change", on);
  }, [q]);
  return dark;
}

function useFileSides(pr: PullReview | undefined, file: PrFile | undefined) {
  const enabled = !!pr && !!file && !pr.sample;
  const orig = useQuery({
    queryKey: keys.file(pr?.repo ?? "", pr?.base_sha ?? "", file?.previous_filename ?? file?.filename ?? ""),
    queryFn: () => (file!.status === "added" ? Promise.resolve("") : api.githubFile(pr!.repo, pr!.base_sha, file!.previous_filename ?? file!.filename)),
    enabled,
  });
  const mod = useQuery({
    queryKey: keys.file(pr?.repo ?? "", pr?.head_sha ?? "", file?.filename ?? ""),
    queryFn: () => (file!.status === "removed" ? Promise.resolve("") : api.githubFile(pr!.repo, pr!.head_sha, file!.filename)),
    enabled,
  });
  if (pr?.sample) return { original: file?.original ?? "", modified: file?.modified ?? "", loading: false };
  return { original: orig.data ?? "", modified: mod.data ?? "", loading: orig.isLoading || mod.isLoading };
}

export default function ReviewView({ repo, number, sample, onBack }: { repo: string; number: number; sample: boolean; onBack: () => void }) {
  const qc = useQueryClient();
  const dark = useDark();
  const { data: pr, error } = useQuery({ queryKey: keys.pull(repo, number), queryFn: () => api.githubPull(repo, number, sample) });
  const [path, setPath] = useState<string | null>(null);
  const file = pr?.files.find((f) => f.filename === path) ?? pr?.files[0];
  const sides = useFileSides(pr, file);
  const [line, setLine] = useState<number | null>(null);
  const [draft, setDraft] = useState("");
  const [posting, setPosting] = useState(false);
  const [postError, setPostError] = useState<string | null>(null);
  const [localComments, setLocalComments] = useState<ReviewComment[]>([]);
  const editorRef = useRef<monaco.editor.IStandaloneDiffEditor | null>(null);
  const decorations = useRef<monaco.editor.IEditorDecorationsCollection | null>(null);

  const comments = useMemo(
    () => [...(pr?.comments ?? []), ...localComments].filter((c) => c.path === file?.filename),
    [pr, localComments, file],
  );

  // Mark commented lines in the gutter of the new side.
  useEffect(() => {
    const ed = editorRef.current?.getModifiedEditor();
    if (!ed) return;
    decorations.current?.clear();
    decorations.current = ed.createDecorationsCollection(
      comments
        .filter((c) => c.line)
        .map((c) => ({
          range: new monaco.Range(c.line!, 1, c.line!, 1),
          options: { isWholeLine: true, className: "review-commented-line", glyphMarginClassName: "review-comment-glyph", glyphMarginHoverMessage: { value: `**${c.user}**: ${c.body}` } },
        })),
    );
  }, [comments, sides.modified]);

  const onMount: DiffOnMount = (editor) => {
    editorRef.current = editor;
    const mod = editor.getModifiedEditor();
    mod.updateOptions({ glyphMargin: true });
    mod.onDidChangeCursorPosition((e) => setLine(e.position.lineNumber));
    mod.onMouseDown((e) => {
      if (e.target.position && (e.target.type === monaco.editor.MouseTargetType.GUTTER_GLYPH_MARGIN || e.target.type === monaco.editor.MouseTargetType.GUTTER_LINE_NUMBERS))
        setLine(e.target.position.lineNumber);
    });
  };

  const post = async () => {
    if (!pr || !file || !line || !draft.trim()) return;
    setPosting(true);
    setPostError(null);
    const comment: ReviewComment = { id: -Date.now(), path: file.filename, line, side: "RIGHT", body: draft.trim(), user: "you", created_at: new Date().toISOString() };
    try {
      if (!pr.sample) {
        await api.githubComment({ repo, number, commitId: pr.head_sha, path: file.filename, line, side: "RIGHT", body: draft.trim() });
        await qc.invalidateQueries({ queryKey: keys.pull(repo, number) });
      } else {
        setLocalComments((c) => [...c, comment]);
      }
      setDraft("");
    } catch (e) {
      setPostError(String(e));
    } finally {
      setPosting(false);
    }
  };

  if (error) return <div className="review"><div className="review-bar"><Button size="sm" icon="arrow-left" onClick={onBack}>Back</Button></div><p className="chat-error">Couldn't load the pull request: {String(error)}</p></div>;

  return (
    <div className="review">
      <div className="review-bar">
        <Button size="sm" variant="ghost" icon="arrow-left" onClick={onBack}>Back</Button>
        <Brand name="github" size={16} />
        <div className="review-title">
          <strong>{pr?.title ?? "Loading…"}</strong>
          <span className="subtle">{repo}#{number}{pr && ` · ${pr.author}`}</span>
        </div>
        {pr?.sample && <Badge tone="warning">Sample data</Badge>}
        {pr && <Button size="sm" icon="external" onClick={() => openUrl(pr.url)}>GitHub</Button>}
      </div>
      <div className="review-body">
        <nav className="review-files">
          <div className="label-sm">{pr?.files.length ?? 0} files</div>
          {pr?.files.map((f) => (
            <button key={f.filename} className={cx("review-file", f.filename === file?.filename && "active")} onClick={() => setPath(f.filename)} title={f.filename}>
              <Icon name="file" size={13} />
              <span className="review-file-name">{f.filename}</span>
              <span className="review-file-stat"><span className="text-success">+{f.additions}</span> <span className="text-danger">−{f.deletions}</span></span>
            </button>
          ))}
        </nav>
        <div className="review-editor">
          {file && (
            <DiffEditor
              key={file.filename}
              original={sides.original}
              modified={sides.modified}
              language={languageFor(file.filename)}
              theme={dark ? "vs-dark" : "vs"}
              onMount={onMount}
              loading={<div className="skeleton" />}
              options={{ readOnly: true, renderSideBySide: true, useInlineViewWhenSpaceIsLimited: false, minimap: { enabled: false }, fontSize: 12.5, scrollBeyondLastLine: false, automaticLayout: true, renderOverviewRuler: false }}
            />
          )}
        </div>
        <aside className="review-comments">
          <div className="label-sm">Comments on this file</div>
          {!comments.length && <p className="subtle">No comments yet.</p>}
          {comments.map((c) => (
            <div key={c.id} className="review-comment">
              <div className="subtle"><strong>{c.user}</strong> · line {c.line ?? "?"}</div>
              <p>{c.body}</p>
            </div>
          ))}
          <div className="review-compose">
            <div className="label-sm">{line ? `Comment on line ${line}` : "Click a line to comment"}</div>
            <textarea className="textarea" rows={4} value={draft} placeholder="Leave a review comment…" onChange={(e) => setDraft(e.target.value)} disabled={!line} />
            {postError && <p className="text-danger">{postError}</p>}
            <Button size="sm" variant="primary" icon="send" disabled={!line || !draft.trim() || posting} onClick={post}>
              {pr?.sample ? "Add comment" : "Comment on GitHub"}
            </Button>
          </div>
        </aside>
      </div>
    </div>
  );
}
