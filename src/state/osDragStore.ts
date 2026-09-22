import { create } from "zustand";

/* Where a drag coming from outside the app is currently hovering.

   Kept apart from the in-window drag state because it arrives on a different
   channel; the panes read it to draw the same highlights. */

interface OsDragState {
  /** Which pane the drag is over, if any. */
  overIndex: 0 | 1 | null;
  /** The folder row under the pointer; without one the drop goes to the
      directory the pane is showing. */
  overFolder: string | null;
  setOver: (index: 0 | 1 | null, folder: string | null) => void;
}

export const useOsDragStore = create<OsDragState>((set) => ({
  overIndex: null,
  overFolder: null,
  setOver: (index, folder) =>
    set((s) =>
      s.overIndex === index && s.overFolder === folder
        ? s
        : { overIndex: index, overFolder: folder },
    ),
}));
