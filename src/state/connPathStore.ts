import { create } from "zustand";
import { persist, createJSONStorage } from "zustand/middleware";
import { persistStorage } from "../lib/persist";

/* The last folder visited on each connection.

   Switching connections in the sidebar returns to where that volume was left,
   not to its root; a connection never visited opens at the root. Kept on disk,
   so it survives a restart. */

interface ConnPathState {
  /** Connection id to the last remote path seen there. */
  lastPath: Record<string, string>;
  record: (connId: string | null | undefined, path: string) => void;
  pathFor: (connId: string | null | undefined) => string;
}

export const useConnPathStore = create<ConnPathState>()(
  persist(
    (set, get) => ({
      lastPath: {},
      record: (connId, path) => {
        if (!connId) return;
        set((s) => ({ lastPath: { ...s.lastPath, [connId]: path } }));
      },
      pathFor: (connId) => (connId ? (get().lastPath[connId] ?? "") : ""),
    }),
    {
      name: "conn-paths",
      storage: createJSONStorage(() => persistStorage),
      partialize: (s) => ({ lastPath: s.lastPath }),
    },
  ),
);
