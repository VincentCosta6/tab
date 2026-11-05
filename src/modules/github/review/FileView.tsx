// A file from the explorer, read-only, at the PR's head. Language features
// (hover, go to definition, references) come from the shared project models.

import { useEffect, useRef } from "react";
import { ensureModel, headUri, monaco } from "../monaco";
import { useReview } from "./context";
import { useFileText } from "./data";
import { defineThemes, themeName } from "./FileDiff";

export function FileView({ path, reveal }: { path: string; reveal?: { line: number; column?: number; nonce: number } }) {
  const { src, dark, searchFor } = useReview();
  const text = useFileText(src, "head", path);
  const host = useRef<HTMLDivElement>(null);
  const editor = useRef<monaco.editor.IStandaloneCodeEditor | null>(null);

  useEffect(() => {
    if (!host.current) return;
    defineThemes();
    const ed = monaco.editor.create(host.current, {
      model: null,
      readOnly: true,
      domReadOnly: true,
      automaticLayout: true,
      minimap: { enabled: true, renderCharacters: false, scale: 1 },
      fontSize: 12.5,
      lineHeight: 19,
      scrollBeyondLastLine: false,
      theme: themeName(dark),
      stickyScroll: { enabled: true },
      renderLineHighlight: "all",
    });
    ed.addAction({
      id: "tab.review.search",
      label: "Search the repository for this",
      keybindings: [monaco.KeyMod.CtrlCmd | monaco.KeyMod.Shift | monaco.KeyCode.KeyF],
      contextMenuGroupId: "navigation",
      contextMenuOrder: 1,
      run: (x) => {
        const s = x.getSelection();
        const m = x.getModel();
        if (!s || !m) return;
        const t = s.isEmpty() ? (m.getWordAtPosition(s.getStartPosition())?.word ?? "") : m.getValueInRange(s);
        if (t.trim()) searchFor(t.split("\n")[0]);
      },
    });
    editor.current = ed;
    return () => {
      ed.setModel(null);
      ed.dispose();
      editor.current = null;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  useEffect(() => {
    const ed = editor.current;
    if (!ed || text.data == null) return;
    const model = ensureModel(headUri(path), text.data, path);
    if (ed.getModel() !== model) ed.setModel(model);
  }, [text.data, path]);

  useEffect(() => {
    const ed = editor.current;
    if (!ed || !reveal || text.data == null) return;
    const t = window.setTimeout(() => {
      ed.revealLineInCenter(reveal.line);
      ed.setSelection(new monaco.Range(reveal.line, reveal.column ?? 1, reveal.line, reveal.column ?? 1));
      ed.focus();
    }, 30);
    return () => clearTimeout(t);
  }, [reveal, text.data]);

  return (
    <div className="rv-fileview">
      {text.error && <p className="rv-error rv-pad">Couldn't load {path}: {String(text.error)}</p>}
      <div ref={host} className="rv-fileview-host" />
    </div>
  );
}
