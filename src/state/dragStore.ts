import { create } from "zustand";
import type { Entry, PaneSide } from "./paneStore";

/* Dragging inside the window: within a pane or from one pane to the other.
   Files dragged in from the operating system arrive through a different
   channel.

   Whether a drop copies or moves is chosen from a menu after the drop, not with
   a held modifier key, because the webview does not report modifiers during a
   drag.

   What is being dragged is fixed when the drag starts; the pointer position and
   the hovered target change as it moves, and together they draw the badge and
   the drop highlight. */

export interface DragPayload {
  items: Entry[];
  sourceSide: PaneSide;
  /** The source directory, relative to that pane's root. */
  sourcePath: string;
}

interface DragState {
  payload: DragPayload | null;
  cursor: { x: number; y: number };
  /** The drop target under the pointer, as the badge describes it. */
  dest: { side: PaneSide; path: string; label: string } | null;
  /** Which pane the pointer is over, for the dashed outline. */
  overSide: PaneSide | null;
  /** The folder row highlighted as the drop target, if any. */
  overFolder: string | null;

  start: (p: DragPayload) => void;
  update: (
    patch: Partial<
      Pick<DragState, "cursor" | "dest" | "overSide" | "overFolder">
    >,
  ) => void;
  end: () => void;
}

export const useDragStore = create<DragState>((set) => ({
  payload: null,
  cursor: { x: 0, y: 0 },
  dest: null,
  overSide: null,
  overFolder: null,

  start: (payload) =>
    set({ payload, dest: null, overSide: null, overFolder: null }),
  update: (patch) => set(patch),
  end: () =>
    set({ payload: null, dest: null, overSide: null, overFolder: null }),
}));
