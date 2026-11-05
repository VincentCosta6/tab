import { createContext, useContext } from "react";
import type { PullReview, ReviewAction } from "../../../lib/types";
import type { Ai } from "./ai";
import type { Source } from "./data";

export type Side = "LEFT" | "RIGHT";
export interface ComposerTarget { path: string; side: Side; start: number; end: number }
export type DiffMode = "split" | "unified";

export interface ReviewCtx {
  pr: PullReview;
  src: Source;
  act: (a: ReviewAction) => Promise<void>;
  mode: DiffMode;
  dark: boolean;
  composer: ComposerTarget | null;
  setComposer: (c: ComposerTarget | null) => void;
  /** Opens a file in an editor tab (a diff tab when it changed). */
  openFile: (path: string, line?: number, column?: number) => void;
  /** Scrolls the combined changes view to a file. */
  showChange: (path: string) => void;
  /** Runs a workspace search for some text. */
  searchFor: (text: string) => void;
  notify: (text: string, tone?: "danger" | "success") => void;
  /** The PR's agent chat, Ask AI and the fix list. */
  ai: Ai;
}

export const Ctx = createContext<ReviewCtx | null>(null);

export function useReview() {
  const c = useContext(Ctx);
  if (!c) throw new Error("useReview outside the review workspace");
  return c;
}
