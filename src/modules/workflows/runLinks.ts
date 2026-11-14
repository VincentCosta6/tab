// Opening one run in the Workflows tab's runs view, from anywhere (an
// item's runs list, a run's "Caused by").

import { showTab } from "../../lib/nav";

let pendingRun: string | null = null;

/** The run the Workflows tab should open when it next mounts or hears `tab:open-run`. */
export function takePendingRun() {
  const id = pendingRun;
  pendingRun = null;
  return id;
}

export function openRun(id: string) {
  pendingRun = id;
  showTab("workflows");
  window.dispatchEvent(new CustomEvent("tab:open-run", { detail: id }));
}
