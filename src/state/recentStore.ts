import { create } from "zustand";
import { persist, createJSONStorage } from "zustand/middleware";
import { persistStorage } from "../lib/persist";
import type { PaneSide } from "./paneStore";

/* The directories the sidebar lists as recent.

   Visiting a directory again moves it to the top rather than adding a second
   row, and the list is capped. Kept on disk between runs. */

const CAP = 12;

export interface RecentItem {
  side: PaneSide;
  /** The path as that side writes it, relative to the pane's root. */
  path: string;
  /** What the sidebar row shows. */
  label: string;
  /** When it was last visited. */
  ts: number;
}

interface RecentState {
  items: RecentItem[];
  /** Records a visit. Roots are skipped: they are one click away anyway. */
  visit: (side: PaneSide, path: string) => void;
  clear: () => void;
}

const isRoot = (p: string) => p === "" || p === "~" || p === "/";

export const useRecentStore = create<RecentState>()(
  persist(
    (set) => ({
      items: [],
      visit: (side, path) => {
        if (isRoot(path)) return;
        set((s) => {
          const rest = s.items.filter(
            (it) => !(it.side === side && it.path === path),
          );
          const next: RecentItem = { side, path, label: path, ts: Date.now() };
          return { items: [next, ...rest].slice(0, CAP) };
        });
      },
      clear: () => set({ items: [] }),
    }),
    {
      name: "recent",
      storage: createJSONStorage(() => persistStorage),
      partialize: (s) => ({ items: s.items }),
    },
  ),
);
