// The file stage: a file opened from a chat takes the middle of the pane,
// with the chat docked to its right. Chats that host one provide this;
// elsewhere files open as before (images in the viewer, others in their app).

import { createContext, useContext } from "react";
import type { ThreadFile } from "./threadFiles";

export interface StageFile {
  path: string;
  name?: string;
}

export interface Stage {
  /** Opens `files[index]` on the stage; arrows step through the rest (the chat's files, when it's one of them). */
  open: (files: (string | StageFile)[], index?: number) => void;
  /** The chat's files: attachments and files the agent named. */
  files: ThreadFile[];
  /** The path on the stage, if any. */
  current: string | null;
  /** Paths whose file changed on disk in the last few seconds. */
  updated: ReadonlySet<string>;
}

export const StageContext = createContext<Stage | null>(null);

export const useStage = () => useContext(StageContext);
