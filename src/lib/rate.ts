/* Transfer speed, measured over the last few seconds from the progress reports.

   The backend reports how many bytes are done; the speed is worked out here
   from how that number moved. A window of recent seconds follows the real pace
   (a fast file after many slow ones shows as fast), and it falls to zero on its
   own when the reports stop, so a stalled transfer does not keep showing its
   last speed. */

export interface RateSample {
  /** `performance.now()` when the report arrived. */
  at: number;
  bytes: number;
}

// Long enough to span two reports even on a slow line: an upload reports once
// per 8 MiB part.
export const RATE_WINDOW_MS = 10_000;

/** Adds a report and drops the ones the window no longer needs. The newest
    report from before the window stays, as the window's starting point. */
export function addSample(
  samples: RateSample[],
  sample: RateSample,
  windowMs = RATE_WINDOW_MS,
): RateSample[] {
  const all = [...samples, sample];
  const edge = sample.at - windowMs;
  let first = 0;
  for (let i = 0; i < all.length; i++) {
    if (all[i].at <= edge) first = i;
  }
  return all.slice(first);
}

/** Bytes per second over the window that ends at `now`. A transfer younger
    than the window is measured from its first report. */
export function rateOf(
  samples: RateSample[],
  now: number,
  windowMs = RATE_WINDOW_MS,
): number {
  if (samples.length === 0) return 0;
  const edge = now - windowMs;
  let start: RateSample | null = null;
  for (const s of samples) {
    if (s.at <= edge) start = s;
  }
  const from = start ?? samples[0];
  const span = start ? windowMs : now - from.at;
  if (span <= 0) return 0;
  const last = samples[samples.length - 1];
  return (Math.max(0, last.bytes - from.bytes) / span) * 1000;
}
