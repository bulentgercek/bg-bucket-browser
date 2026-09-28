import { getCurrentWebview } from "@tauri-apps/api/webview";
import { useUiStore, type FontSize } from "../state/uiStore";
import { usePaneStore, type PaneIndex, type ViewSize } from "../state/paneStore";
import { useDialogStore } from "../state/dialogStore";
import { useCleanupStore } from "../state/cleanupStore";
import { devlogVerbose } from "./commands";

/* Control with + / - or the mouse wheel makes things larger or smaller, and
   what it resizes follows the active pane, the one the frame is drawn around:
   that pane's rows or tiles. With no active pane it is the whole window. The
   Settings screen has no panes, and a window open on top covers them, so both
   resize the whole window.

   The window's size is the webview's own zoom. Every size and position on the
   page stays in the same CSS pixels, so the menus, the drag badge and the
   divider need no arithmetic for it. */

/** The four font sizes, smallest first. */
export const FONT_SIZES: FontSize[] = ["small", "medium", "large", "xlarge"];

const FONT_ZOOM: Record<FontSize, number> = {
  small: 0.9,
  medium: 1,
  large: 1.15,
  xlarge: 1.3,
};

const VIEW_SIZES: ViewSize[] = ["s", "m", "l"];

let applied: FontSize | null = null;

/** Draws the window at `size`; the promise settles once the webview has taken
    it, and never rejects. A size already applied is not sent again. In a plain
    browser preview there is no webview to zoom, and the page stays as it is. */
export function applyFontSize(size: FontSize): Promise<void> {
  if (size === applied) return Promise.resolve();
  applied = size;
  const zoom = FONT_ZOOM[size] ?? 1;
  return Promise.resolve()
    .then(() => getCurrentWebview().setZoom(zoom))
    .then(
      () => devlogVerbose("ui", `font size ${size} (zoom ${zoom})`),
      (e: unknown) => {
        applied = null; // so the next call tries again
        devlogVerbose("ui", `font size ${size}: zoom failed: ${String(e)}`);
      },
    );
}

/** Keeps the webview at the saved font size as it changes; returns the
    function that stops. Done here rather than in a component, so a change
    does not make the whole window draw again before the zoom. */
export function followFontSize(): () => void {
  return useUiStore.subscribe((s, prev) => {
    if (s.fontSize !== prev.fontSize) void applyFontSize(s.fontSize);
  });
}

/** One step of the window's font size; past either end nothing changes. */
function stepFontSize(dir: 1 | -1): void {
  const { fontSize, setFontSize } = useUiStore.getState();
  const cur = FONT_SIZES.indexOf(fontSize);
  const next = Math.max(0, Math.min(FONT_SIZES.length - 1, cur + dir));
  if (next !== cur) setFontSize(FONT_SIZES[next]);
}

/** One step of the row or tile size in the open tab of pane `index`. */
function stepViewSize(index: PaneIndex, dir: 1 | -1): void {
  const { panes, setViewSize } = usePaneStore.getState();
  const pane = panes[index];
  const tab = pane.tabs.find((tb) => tb.id === pane.activeTabId);
  if (!tab) return;
  const cur = VIEW_SIZES.indexOf(tab.viewSize);
  const next = Math.max(0, Math.min(VIEW_SIZES.length - 1, cur + dir));
  if (next !== cur) setViewSize(index, VIEW_SIZES[next]);
}

/** The pane to resize, or `null` for the whole window. */
function paneToResize(): PaneIndex | null {
  const { screen, activePane } = useUiStore.getState();
  if (screen !== "main") return null;
  if (useDialogStore.getState().dialog || useCleanupStore.getState().open) {
    return null;
  }
  return activePane;
}

/** Listens for control with + / - and the wheel; returns the function that
    stops listening. */
export function initZoomControls(): () => void {
  // Caught on the way down, at the window: an open window stops every key
  // there, and the whole window must still be resizable under it. Stopping the
  // event does not hold back other listeners on the window itself.
  const onKey = (e: KeyboardEvent) => {
    if (!(e.ctrlKey || e.metaKey) || e.altKey) return;
    const dir =
      e.key === "+" || e.key === "="
        ? 1
        : e.key === "-" || e.key === "_"
          ? -1
          : 0;
    if (dir === 0) return;
    e.preventDefault();
    e.stopPropagation();
    if (e.repeat) return; // a held key takes one step, not all of them
    const pane = paneToResize();
    if (pane === null) stepFontSize(dir);
    else stepViewSize(pane, dir);
  };

  // Not passive, because the event has to be cancelled: otherwise whatever is
  // under the pointer scrolls at the same time.
  let paneAt = 0;
  let windowAt = 0;
  const onWheel = (e: WheelEvent) => {
    if (!e.ctrlKey && !e.metaKey) return;
    e.preventDefault();
    if (e.deltaY === 0) return; // sideways, which resizes nothing
    const dir = e.deltaY < 0 ? 1 : -1;
    const pane = paneToResize();
    const now = Date.now();
    if (pane !== null) {
      if (now - paneAt < 90) return; // a trackpad pinch sends far too many
      paneAt = now;
      stepViewSize(pane, dir);
    } else {
      // Four steps cover the whole range, so one turn of the wheel should not
      // run through all of them.
      if (now - windowAt < 250) return;
      windowAt = now;
      stepFontSize(dir);
    }
  };

  window.addEventListener("keydown", onKey, true);
  window.addEventListener("wheel", onWheel, { passive: false });
  return () => {
    window.removeEventListener("keydown", onKey, true);
    window.removeEventListener("wheel", onWheel);
  };
}
