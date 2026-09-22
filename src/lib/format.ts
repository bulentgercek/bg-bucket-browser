/* How sizes and dates are written wherever the app shows them. */

const SIZE_UNITS = ["B", "KB", "MB", "GB", "TB"] as const;

/** Bytes as a readable size, in decimal units so the numbers match what S3 and
 *  the RunPod dashboard report. Directories have no size and show a dash. */
export function formatSize(bytes: number | null): string {
  if (bytes === null) return "—";
  let value = bytes;
  let unit = 0;
  while (value >= 1000 && unit < SIZE_UNITS.length - 1) {
    value /= 1000;
    unit += 1;
  }
  const shown = unit === 0 ? value : Math.round(value * 10) / 10;
  return unit === 0 ? `${shown} B` : `${shown} ${SIZE_UNITS[unit]}`;
}

const MONTHS = [
  "Jan", "Feb", "Mar", "Apr", "May", "Jun",
  "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
] as const;

/** A timestamp in the format the design asks for: short month, unpadded day,
 *  24-hour clock, no year, in the viewer's own time zone. Anything unusable
 *  shows a dash.
 *
 *  Older files are indistinguishable from this year's; a year would go here if
 *  that ever matters. */
export function formatDate(epochMs: number | null): string {
  if (epochMs === null || !Number.isFinite(epochMs)) return "—";
  const d = new Date(epochMs);
  if (Number.isNaN(d.getTime())) return "—";
  const hh = String(d.getHours()).padStart(2, "0");
  const mm = String(d.getMinutes()).padStart(2, "0");
  return `${MONTHS[d.getMonth()]} ${d.getDate()} ${hh}:${mm}`;
}
