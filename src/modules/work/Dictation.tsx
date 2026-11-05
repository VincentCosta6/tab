import { useEffect, useRef, useState, type RefObject } from "react";
import { cx } from "../../design/components";
import { Icon } from "../../design/icons";
import { api, inTauri } from "../../lib/api";

type Recognition = {
  continuous: boolean;
  interimResults: boolean;
  lang: string;
  onresult: ((e: { resultIndex: number; results: ArrayLike<{ isFinal: boolean; 0: { transcript: string } }> }) => void) | null;
  onerror: ((e: { error: string }) => void) | null;
  onend: (() => void) | null;
  start: () => void;
  stop: () => void;
};

const Speech = (globalThis as unknown as { SpeechRecognition?: new () => Recognition; webkitSpeechRecognition?: new () => Recognition }).SpeechRecognition
  ?? (globalThis as unknown as { webkitSpeechRecognition?: new () => Recognition }).webkitSpeechRecognition;

/**
 * Dictation into the composer. In the desktop app, the system's own
 * dictation (macOS), which types straight into the focused box; in a plain
 * browser, its speech recognition.
 */
export function Dictation({ onText, target }: { onText: (text: string) => void; target: RefObject<HTMLTextAreaElement | null> }) {
  const [on, setOn] = useState(false);
  const [note, setNote] = useState<string | null>(null);
  const rec = useRef<Recognition | null>(null);

  useEffect(() => () => rec.current?.stop(), []);
  useEffect(() => {
    if (!note) return;
    const t = window.setTimeout(() => setNote(null), 5000);
    return () => window.clearTimeout(t);
  }, [note]);

  const system = async () => {
    target.current?.focus();
    try {
      await api.startDictation();
    } catch (e) {
      setNote(String(e));
    }
  };

  const toggle = () => {
    if (on) return rec.current?.stop();
    // In the app, the system's dictation: it needs no microphone permission
    // of tab's own, and the webview's speech API would.
    if (inTauri) return void system();
    if (!Speech) return setNote("This browser has no dictation.");
    const r = new Speech();
    r.continuous = true;
    r.interimResults = false;
    r.lang = navigator.language || "en-US";
    r.onresult = (e) => {
      for (let i = e.resultIndex; i < e.results.length; i++) {
        const res = e.results[i];
        if (res.isFinal) onText(res[0].transcript.trim());
      }
    };
    r.onerror = (e) => {
      setOn(false);
      // The webview can't listen here: hand over to the system's dictation.
      if (inTauri && /not-allowed|service-not-allowed|audio-capture/.test(e.error)) void system();
      else if (e.error !== "aborted" && e.error !== "no-speech") setNote(`Dictation stopped: ${e.error}`);
    };
    r.onend = () => setOn(false);
    rec.current = r;
    try {
      r.start();
      setOn(true);
      target.current?.focus();
    } catch {
      if (inTauri) void system();
    }
  };

  return (
    <span className="dictation">
      <button className={cx("bar-icon", on && "on")} onClick={toggle} title={on ? "Stop dictation" : "Dictate"} aria-label={on ? "Stop dictation" : "Dictate"} aria-pressed={on}>
        <Icon name="mic" size={14} />
      </button>
      {note && <span className="dictation-note" role="status">{note}</span>}
    </span>
  );
}
