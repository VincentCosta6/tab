// Where the person is, so a view opened from somewhere (a PR from a chat's
// link) can take them back there, and a way for any module to offer closing
// the task once an outcome it shows has happened (a PR merged, a ticket
// resolved). Nothing here knows about GitHub.

import { openItem } from "./links";
import type { Tone } from "./types";

/** A tab, and on Work the item that was open. */
export interface Origin {
  tab: string;
  itemId: string | null;
}

let here: Origin = { tab: "work", itemId: null };

/** The shell sets the tab; Work sets the open item. */
export function setHere(p: Partial<Origin>) {
  here = { ...here, ...p };
}

export function whereAmI(): Origin {
  return { ...here };
}

/** Shows a tab ("work", "reviews", …). The shell listens. */
export function showTab(tab: string) {
  window.dispatchEvent(new CustomEvent("tab:show", { detail: tab }));
}

/** Goes back to where a view was opened from. */
export function goBack(o: Origin) {
  if (o.tab === "work" && o.itemId) openItem(o.itemId);
  else showTab(o.tab);
}

/** An offer to close a task, shown by the shell until answered. */
export interface DoneOffer {
  itemId: string;
  /** What happened: "Pull request merged". */
  message: string;
  /** The mark beside it; a check by default. */
  icon?: string;
  tone?: Tone;
  /** Where closing it should land. On Work, the next item in the queue opens. */
  back?: Origin;
}

export function offerDone(o: DoneOffer) {
  window.dispatchEvent(new CustomEvent<DoneOffer>("tab:offer-done", { detail: o }));
}

/** An item Work should close (with its animation) when it next mounts or hears `tab:done-item`. */
let pendingDone: string | null = null;
export function takePendingDone() {
  const id = pendingDone;
  pendingDone = null;
  return id;
}

/** Closes an item from Work, so the row animates out and the next item opens. */
export function closeOnWork(id: string) {
  pendingDone = id;
  showTab("work");
  window.dispatchEvent(new CustomEvent("tab:done-item", { detail: id }));
}
