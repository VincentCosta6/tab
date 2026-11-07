import { createContext, useContext } from "react";
import type { TabConfig } from "../../lib/types";

/** The config being edited. Every page edits the same draft; one Save writes it. */
export interface Draft {
  draft: TabConfig;
  set: (fn: (c: TabConfig) => void) => void;
}

export const DraftContext = createContext<Draft | null>(null);

export function useDraft(): Draft {
  const d = useContext(DraftContext);
  if (!d) throw new Error("useDraft outside Settings");
  return d;
}
