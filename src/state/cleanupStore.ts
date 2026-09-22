import { create } from "zustand";
import { listen } from "@tauri-apps/api/event";
import {
  scanVolume,
  cancelScan,
  deleteScanned,
  type ScanReport,
  type ScanProgress,
} from "../lib/commands";
import { useToastStore } from "./toastStore";
import { t } from "../locale/en";

/* The Volume Cleanup window: scan the volume, look at what it found, delete
   what is selected.

   A report is never kept. It describes the volume at one moment, and a stale
   one would offer to delete files that are no longer there. */

type Status = "idle" | "scanning" | "done" | "deleting";

interface CleanupState {
  open: boolean;
  status: Status;
  progress: ScanProgress | null;
  report: ScanReport | null;
  selected: Set<string>;
  show: () => void;
  close: () => void;
  runScan: () => Promise<void>;
  stopScan: () => void;
  toggle: (key: string) => void;
  selectAllReclaimable: () => void;
  clearSelection: () => void;
  runDelete: () => Promise<void>;
}

export const useCleanupStore = create<CleanupState>((set, get) => ({
  open: false,
  status: "idle",
  progress: null,
  report: null,
  selected: new Set(),

  show: () =>
    set({
      open: true,
      status: "idle",
      progress: null,
      report: null,
      selected: new Set(),
    }),
  close: () => {
    if (get().status === "scanning") void cancelScan();
    set({
      open: false,
      status: "idle",
      progress: null,
      report: null,
      selected: new Set(),
    });
  },

  runScan: async () => {
    set({ status: "scanning", progress: { scanned: 0, bytes: 0 }, report: null });
    const unlisten = await listen<ScanProgress>("cleanup-scan-progress", (e) => {
      set({ progress: e.payload });
    });
    try {
      const report = await scanVolume();
      if (report == null) {
        // Cancelled: the window stays open, back at the start.
        set({ status: "idle", progress: null });
      } else {
        set({ status: "done", report, progress: null });
      }
    } catch (e) {
      useToastStore.getState().push(String(e), "error");
      set({ status: "idle", progress: null });
    } finally {
      unlisten();
    }
  },

  stopScan: () => {
    if (get().status === "scanning") void cancelScan();
  },

  toggle: (key) =>
    set((s) => {
      const next = new Set(s.selected);
      if (next.has(key)) next.delete(key);
      else next.add(key);
      return { selected: next };
    }),
  selectAllReclaimable: () =>
    set((s) => ({
      selected: new Set(s.report?.reclaimable.map((e) => e.key) ?? []),
    })),
  clearSelection: () => set({ selected: new Set() }),

  runDelete: async () => {
    const keys = [...get().selected];
    if (keys.length === 0) return;
    set({ status: "deleting" });
    try {
      const n = await deleteScanned(keys);
      useToastStore.getState().push(t("cleanup.deleted", { count: n }));
    } catch (e) {
      useToastStore.getState().push(String(e), "error");
    }
    // Deleting invalidates the report, so the window closes with it.
    set({
      open: false,
      status: "idle",
      progress: null,
      report: null,
      selected: new Set(),
    });
  },
}));
