// A workflow's spend this UTC month against its monthly caps: API dollars
// and subscription tokens, each on its own meter (never converted into
// each other). Shown on Insights when the workflow has a cap.

import { cx } from "../../../design/components";
import { Icon } from "../../../design/icons";
import type { CapUsage } from "../../../lib/types";
import { planTokens, tokens, usd } from "./format";
import "./rollups.css";

const monthName = (day: string, offset = 0) => {
  const d = new Date(`${day}T12:00:00Z`);
  d.setUTCMonth(d.getUTCMonth() + offset, 1);
  return d.toLocaleDateString(undefined, { month: "short", day: offset ? "numeric" : undefined, timeZone: "UTC" });
};

export function CapMeter({ cap }: { cap: CapUsage }) {
  const month = cap.month ?? new Date().toISOString().slice(0, 10);
  const rows = [
    cap.cap_usd > 0 && { key: "usd", label: "API spend", used: cap.spend.cost_usd, of: cap.cap_usd, text: `${usd(cap.spend.cost_usd)} of ${usd(cap.cap_usd)}` },
    cap.cap_plan_tokens > 0 && { key: "tok", label: "Plan tokens", used: planTokens(cap.spend), of: cap.cap_plan_tokens, text: `${tokens(planTokens(cap.spend))} of ${tokens(cap.cap_plan_tokens)}` },
  ].filter(Boolean) as { key: string; label: string; used: number; of: number; text: string }[];
  if (!rows.length) return null;
  const reached = rows.some((r) => r.used >= r.of);
  return (
    <section className={cx("ins-cap", reached && "reached")} aria-label="Monthly spend caps">
      <div className="ins-cap-head">
        <Icon name="gauge" size={13} />
        <strong>Monthly caps</strong>
        <span className="subtle">{monthName(month)} so far · resets {monthName(month, 1)} (UTC)</span>
      </div>
      {rows.map((r) => {
        const share = r.of ? r.used / r.of : 0;
        const tone = share >= 1 ? "danger" : share >= 0.8 ? "warning" : "ok";
        return (
          <div key={r.key} className="ins-cap-row">
            <span className="ins-cap-label">{r.label}</span>
            <span className="ins-cap-track" role="meter" aria-valuemin={0} aria-valuemax={r.of} aria-valuenow={r.used} aria-label={`${r.label}: ${r.text}`}>
              <span className={cx("ins-cap-fill", `tone-${tone}`)} style={{ width: `${Math.min(1, share) * 100}%` }} />
            </span>
            <span className={cx("ins-cap-num", tone === "danger" && "text-danger", tone === "warning" && "text-warning")}>{r.text}</span>
          </div>
        );
      })}
      {reached && (
        <p className="ins-cap-reached text-danger">
          <Icon name="octagon" size={12} /> Reached: new runs are held back until {monthName(month, 1)}. A run already going finishes.
        </p>
      )}
    </section>
  );
}

/** Says plainly what a window past the run log can and can't show. */
export function DetailNote({ since }: { since: string }) {
  const day = new Date(since).toLocaleDateString(undefined, { month: "short", day: "numeric", timeZone: "UTC" });
  return (
    <p className="ins-note">
      <Icon name="info" size={13} />
      <span>
        Runs are kept for 30 days; before {day}, this window reads daily totals. Counts, spend and the chart are exact,
        step times are close estimates, and <strong>failures and models cover only the last 30 days</strong>.
      </span>
    </p>
  );
}
