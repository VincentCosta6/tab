// What a step did lately, on its node on the builder canvas: median time,
// how often it failed, and what it spent.

import { cx } from "../../../design/components";
import { Icon } from "../../../design/icons";
import type { StepStats } from "../../../lib/types";
import { duration, MIN_RUNS, pct, plural, rate, spendLong, spendShort } from "./format";
import "./insights.css";

export function StepBadges({ stats, days }: { stats?: StepStats; days: number }) {
  if (!stats || stats.runs === 0) return null;
  const fail = rate(stats.failed, stats.runs) ?? 0;
  const thin = stats.runs < MIN_RUNS;
  const time = stats.queue_ms.n ? stats.queue_ms.p50 + stats.work_ms.p50 : stats.ms.p50;
  const spend = spendShort(stats.spend);
  const tone = thin ? "neutral" : fail >= 0.2 ? "danger" : fail > 0.05 ? "warning" : "success";
  return (
    <div className="ins-badges" aria-label={`Last ${days} days: ${plural(stats.runs, "run")}`} title={`Last ${days} days, ${plural(stats.runs, "run")}`}>
      <span className="ins-badge" title="Median time"><Icon name="clock" size={11} /> {duration(time)}</span>
      <span className={cx("ins-badge", `tone-${tone}`)} title={`${stats.failed} of ${stats.runs} runs failed`}>
        <Icon name={tone === "success" ? "check" : tone === "neutral" ? "dot" : "alert"} size={11} />
        {thin ? plural(stats.runs, "run") : stats.failed ? `${pct(fail)} fail` : "0% fail"}
      </span>
      {spend !== "Nothing" && <span className="ins-badge" title={spendLong(stats.spend)}>{spendShort(stats.spend, true)}</span>}
    </div>
  );
}
