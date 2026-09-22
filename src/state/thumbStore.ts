import { create } from "zustand";
import type { PaneSide } from "./paneStore";
import { getThumbnail } from "../lib/commands";

/* Thumbnails for the tiles that are on screen.
 *
 * The lasting cache is on the Rust side; this is only what this session has
 * asked for, plus the queue that keeps the app from asking for everything at
 * once.
 *
 * A tile asks when it becomes visible and lets go when it scrolls away. A
 * request that has not left yet is dropped when its tile goes: scrolling fast
 * through a large folder would otherwise decode hundreds of images nobody sees.
 * Entries for tiles that are gone are pruned once there are too many; fetching
 * them again from the disk cache is cheap. */

export type ThumbState =
  | { status: "loading" }
  | { status: "ready"; uri: string }
  /** There is legitimately no preview: unsupported, too large, or empty. */
  | { status: "none" }
  | { status: "error" };

const MAX_INFLIGHT = 5;
const MAP_CAP = 500;
const MAX_ATTEMPTS = 2;

/** The key a thumbnail is remembered under; it carries the modification time,
    so an edited file asks again on its own. */
export function thumbKey(
  side: PaneSide,
  path: string,
  modified: number | null,
): string {
  return `${side} ${path} ${modified ?? 0}`;
}

interface ThumbStore {
  map: Record<string, ThumbState>;
  /** A tile became visible and wants its thumbnail. */
  request: (side: PaneSide, path: string, modified: number | null) => void;
  /** A tile went away; a request still waiting in the queue is dropped. */
  release: (side: PaneSide, path: string, modified: number | null) => void;
}

let inflight = 0;
const inflightKeys = new Set<string>();
const queue: string[] = [];
const jobs: Record<string, { side: PaneSide; path: string }> = {};
/** How many visible tiles want this key: the same file can be in both panes. */
const refs: Record<string, number> = {};
const attempts: Record<string, number> = {};
/** Insertion order, so pruning starts with the oldest. */
const order: string[] = [];

export const useThumbStore = create<ThumbStore>((set, get) => {
  function prune() {
    if (order.length <= MAP_CAP) return;
    const { map } = get();
    const overflow = order.length - MAP_CAP;
    const drop: string[] = [];
    for (const k of order) {
      if (drop.length >= overflow) break;
      const st = map[k];
      if (!st || st.status === "loading") continue; // never drop one in flight
      if ((refs[k] ?? 0) > 0) continue; // nor one a visible tile still wants
      drop.push(k);
    }
    if (drop.length === 0) return;
    const next = { ...map };
    for (const k of drop) {
      delete next[k];
      delete attempts[k];
      const i = order.indexOf(k);
      if (i >= 0) order.splice(i, 1);
    }
    set({ map: next });
  }

  function pump() {
    while (inflight < MAX_INFLIGHT && queue.length > 0) {
      const key = queue.shift();
      if (key === undefined) break;
      const job = jobs[key];
      delete jobs[key];
      // The tile may have gone while this waited in the queue.
      if (!job || get().map[key]?.status !== "loading") continue;

      inflight += 1;
      inflightKeys.add(key);
      getThumbnail(job.side, job.path)
        .then((uri) =>
          set((s) => ({
            map: {
              ...s.map,
              [key]: uri ? { status: "ready", uri } : { status: "none" },
            },
          })),
        )
        .catch((err) => {
          console.warn("thumbnail failed:", job.path, err);
          set((s) => ({ map: { ...s.map, [key]: { status: "error" } } }));
        })
        .finally(() => {
          inflight -= 1;
          inflightKeys.delete(key);
          prune();
          pump();
        });
    }
  }

  function enqueue(key: string, side: PaneSide, path: string) {
    if (!order.includes(key)) order.push(key);
    set((s) => ({ map: { ...s.map, [key]: { status: "loading" } } }));
    jobs[key] = { side, path };
    if (!queue.includes(key)) queue.push(key);
    pump();
  }

  return {
    map: {},

    request: (side, path, modified) => {
      const key = thumbKey(side, path, modified);
      refs[key] = (refs[key] ?? 0) + 1;

      const st = get().map[key];
      if (!st) {
        enqueue(key, side, path);
        return;
      }
      if (st.status === "error" && (attempts[key] ?? 0) < MAX_ATTEMPTS) {
        attempts[key] = (attempts[key] ?? 0) + 1;
        enqueue(key, side, path);
      }
      // Anything else is either on its way, settled, or out of retries.
    },

    release: (side, path, modified) => {
      const key = thumbKey(side, path, modified);
      const left = Math.max(0, (refs[key] ?? 0) - 1);
      if (left > 0) {
        refs[key] = left;
        return;
      }
      delete refs[key];

      // Only a request that has not left yet is cancelled; one already in
      // flight is left to finish and pruned later like any other entry.
      if (get().map[key]?.status === "loading" && !inflightKeys.has(key)) {
        const qi = queue.indexOf(key);
        if (qi >= 0) queue.splice(qi, 1);
        delete jobs[key];
        const oi = order.indexOf(key);
        if (oi >= 0) order.splice(oi, 1);
        set((s) => {
          const next = { ...s.map };
          delete next[key];
          return { map: next };
        });
      }
    },
  };
});
