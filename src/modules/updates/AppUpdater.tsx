import { useState } from "react";
import { Icon } from "../../design/icons";
import { cx } from "../../design/components";
import { percent, useAppUpdate, useInstall } from "./appUpdate";

/**
 * Bottom-left of the nav, next to the agent's update card: a newer release of
 * tab itself, installed with one tap. The app restarts into it.
 */
export function AppUpdater() {
  const { check } = useAppUpdate();
  const { state, install } = useInstall();
  const [skipped, setSkipped] = useState<string | null>(null);
  const update = check.data;
  if (state.phase === "idle" && (!update || skipped === update.version)) return null;

  const pct = percent(state);
  return (
    <div className="updater">
      <div className={cx("updater-card", state.phase === "failed" && "failed")} role="status"
        title={state.phase === "failed" ? state.error : update ? `tab ${update.version} is ready to install` : "Updating tab"}>
        <div className="updater-head">
          <Icon name={state.phase === "failed" ? "alert" : "arrow-up"} size={11} />
          <span className="updater-name">tab</span>
          {state.phase === "idle" && update && (
            <button className="updater-x" aria-label="Not now" title="Not now" onClick={() => setSkipped(update.version)}>
              <Icon name="x" size={11} />
            </button>
          )}
        </div>
        {state.phase === "idle" && update && (
          <>
            <span className="updater-sub">{update.version}</span>
            <button className="updater-go" onClick={() => void install()} title="Download, install and restart tab">Update</button>
          </>
        )}
        {state.phase === "downloading" && (
          <>
            <div className={cx("updater-bar", pct != null && "fixed")}><span style={pct != null ? { width: `${pct}%` } : undefined} /></div>
            <span className="updater-sub">{pct != null ? `${pct}%` : "Downloading…"}</span>
          </>
        )}
        {state.phase === "restarting" && (
          <>
            <div className="updater-bar"><span /></div>
            <span className="updater-sub">Restarting…</span>
          </>
        )}
        {state.phase === "failed" && (
          <>
            <span className="updater-sub">Failed</span>
            <button className="updater-go" onClick={() => void install()}>Retry</button>
          </>
        )}
      </div>
    </div>
  );
}
