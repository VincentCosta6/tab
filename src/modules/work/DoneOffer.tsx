// "Pull request merged. Close the task?" Any module can raise this with
// offerDone() once something it shows has finished; the shell renders it over
// whatever view is up. Closing on Work animates the row out and opens the next
// item; anywhere else the item is just marked done. Done's guards still apply.

import { useQueryClient } from "@tanstack/react-query";
import { useEffect, useState } from "react";
import { Button, cx } from "../../design/components";
import { Icon } from "../../design/icons";
import { useItem } from "../../lib/hooks";
import { closeOnWork, goBack, type DoneOffer as Offer } from "../../lib/nav";
import { actOnItem } from "./optimistic";

export function DoneOffer() {
  const [offer, setOffer] = useState<Offer | null>(null);
  useEffect(() => {
    const on = (e: Event) => setOffer((e as CustomEvent<Offer>).detail);
    window.addEventListener("tab:offer-done", on);
    return () => window.removeEventListener("tab:offer-done", on);
  }, []);
  return offer ? <Card key={offer.itemId} offer={offer} onClose={() => setOffer(null)} /> : null;
}

function Card({ offer, onClose }: { offer: Offer; onClose: () => void }) {
  const qc = useQueryClient();
  const { data: item } = useItem(offer.itemId);
  const [leaving, setLeaving] = useState(false);
  const dismiss = () => (setLeaving(true), setTimeout(onClose, 160));

  const close = () => {
    const back = offer.back;
    if (!back || back.tab === "work") closeOnWork(offer.itemId);
    else {
      goBack(back);
      if (item) void actOnItem(qc, item, "done").catch((e) => console.warn("item action failed", e));
    }
    dismiss();
  };

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") (e.preventDefault(), e.stopPropagation(), dismiss());
      else if (e.key === "Enter" && !(e.target instanceof HTMLTextAreaElement || e.target instanceof HTMLInputElement)) (e.preventDefault(), e.stopPropagation(), close());
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  });

  // Already closed (by hand, or a workflow got there first): nothing to offer.
  useEffect(() => {
    if (item?.state === "done") onClose();
  }, [item?.state, onClose]);
  if (item === null || item?.state === "done") return null;

  return (
    <div className={`done-offer${leaving ? " leaving" : ""}`} role="dialog" aria-label="Close the task?">
      <span className={cx("done-offer-icon", `tone-${offer.tone ?? "success"}`)}><Icon name={offer.icon ?? "check"} size={16} /></span>
      <div className="done-offer-text">
        <strong>{offer.message}</strong>
        <span title={item?.title}>Close {item ? <>“{item.title}”</> : "the task"} too?</span>
      </div>
      <div className="done-offer-actions">
        <Button size="sm" variant="ghost" onClick={dismiss}>Keep open</Button>
        <Button size="sm" variant="primary" icon="check" shortcut="↵" onClick={close}>Close task</Button>
      </div>
    </div>
  );
}
