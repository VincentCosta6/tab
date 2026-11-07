// The file stage: a file opened from a chat takes the middle of the pane,
// with the chat docked to its right. Chats that host one provide this;
// elsewhere files open as before (images in the viewer, others in their app).

import { createContext, useContext } from "react";

export interface StageFile {
  path: string;
  name?: string;
}

export interface Stage {
  /** Opens `files[index]` on the stage; arrows step through the rest. */
  open: (files: (string | StageFile)[], index?: number) => void;
}

export const StageContext = createContext<Stage | null>(null);

export const useStage = () => useContext(StageContext);
