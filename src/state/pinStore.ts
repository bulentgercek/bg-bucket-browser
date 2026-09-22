import { create } from "zustand";
import { persist, createJSONStorage } from "zustand/middleware";
import { persistStorage } from "../lib/persist";
import { useConnectionStore } from "./connectionStore";

/* Pinned locations, remote and local in one list. Each row's icon says which
   kind it is, not which side of the window it opens on.

   A remote pin belongs to the connection it was made on: the sidebar shows only
   the live connection's pins, and deleting a connection deletes them with it. A
   local pin has no connection — the disk is the same whichever volume is
   connected, and pinning Downloads once should be enough. */

export type PinSide = "remote" | "local";

export interface Pin {
  id: string;
  side: PinSide;
  /** The path as that side writes it, relative to the pane's root. */
  path: string;
  /** The short name shown in the sidebar. */
  label: string;
  /** The connection a remote pin was made on; local pins have none. */
  connId?: string;
}

interface PinStore {
  pins: Pin[];
  /** Pins a location, stamping a remote one with the live connection. */
  addPin: (pin: Omit<Pin, "id" | "connId">) => void;
  /** Removes one pin. */
  removePin: (id: string) => void;
  /** Drops every pin belonging to a connection that was deleted. */
  removePinsForConn: (connId: string) => void;
  /** Reorders by dragging: moves a pin in front of another, or to the end.
      Addressed by id rather than by position, because the visible list is
      filtered and a position in it means nothing here. */
  movePin: (id: string, beforeId: string | null) => void;
}

export const usePinStore = create<PinStore>()(
  persist(
    (set) => ({
      // A fresh install starts empty.
      pins: [],
      addPin: (pin) =>
        set((s) => ({
          // A random id, because a counter would restart at zero next run and
          // collide with the ids already on disk.
          pins: [
            ...s.pins,
            {
              ...pin,
              id: crypto.randomUUID(),
              connId:
                pin.side === "remote"
                  ? (useConnectionStore.getState().activeId ?? undefined)
                  : undefined,
            },
          ],
        })),
      removePin: (id) =>
        set((s) => ({ pins: s.pins.filter((p) => p.id !== id) })),
      removePinsForConn: (connId) =>
        set((s) => ({ pins: s.pins.filter((p) => p.connId !== connId) })),
      movePin: (id, beforeId) =>
        set((s) => {
          const idx = s.pins.findIndex((p) => p.id === id);
          if (idx === -1) return s;
          const arr = [...s.pins];
          const [item] = arr.splice(idx, 1);
          const insertAt =
            beforeId === null
              ? arr.length
              : arr.findIndex((p) => p.id === beforeId);
          arr.splice(insertAt === -1 ? arr.length : insertAt, 0, item);
          return { pins: arr };
        }),
    }),
    {
      name: "pins",
      storage: createJSONStorage(() => persistStorage),
      partialize: (s) => ({ pins: s.pins }),
    },
  ),
);
