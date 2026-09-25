import { useEffect, useState } from "react";
import { RECORDING_MAX_SEC, useFeedbackStore } from "../../state/feedbackStore";
import { t } from "../../locale/en";

/* The pill shown in the status bar while a feedback recording runs: how long
   it has been going, out of how long it may, and the one button that ends it.

   At the limit it stops by itself and opens the Feedback screen, the same as a
   press would. */

function mmss(sec: number): string {
  const m = Math.floor(sec / 60);
  const s = sec % 60;
  return `${String(m).padStart(2, "0")}:${String(s).padStart(2, "0")}`;
}

export default function RecordingIndicator() {
  const since = useFeedbackStore((s) => s.recordingSince);
  const stop = useFeedbackStore((s) => s.stopRecording);
  const [now, setNow] = useState(() => Date.now());

  useEffect(() => {
    if (since === null) return;
    setNow(Date.now());
    const id = window.setInterval(() => setNow(Date.now()), 1000);
    return () => window.clearInterval(id);
  }, [since]);

  const elapsed = since === null ? 0 : Math.max(0, Math.floor((now - since) / 1000));

  useEffect(() => {
    if (since !== null && elapsed >= RECORDING_MAX_SEC) void stop();
  }, [since, elapsed, stop]);

  if (since === null) return null;

  return (
    <button
      type="button"
      onClick={() => void stop()}
      className="flex items-center gap-1.5 rounded-full border border-[var(--color-danger)] px-2 py-0.5 text-[var(--color-danger)] hover:bg-[color-mix(in_srgb,var(--color-danger)_12%,transparent)]"
    >
      <span className="h-[7px] w-[7px] shrink-0 animate-pulse rounded-full bg-[var(--color-danger)]" />
      <span className="tabular-nums">
        {t("feedback.pill", {
          elapsed: mmss(Math.min(elapsed, RECORDING_MAX_SEC)),
          max: mmss(RECORDING_MAX_SEC),
        })}
      </span>
      <span className="text-neutral-500">·</span>
      <span>{t("feedback.pill.stop")}</span>
    </button>
  );
}
