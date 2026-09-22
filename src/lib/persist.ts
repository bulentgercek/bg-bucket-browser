import { load, type Store } from "@tauri-apps/plugin-store";
import type { StateStorage } from "zustand/middleware";

/* The bridge between Zustand's persist middleware and the disk.

   Inside the app each store's persisted slice is one JSON string, kept in the
   app's store file under that store's own name. Outside the app — a plain
   browser preview with no Tauri behind it — the same interface falls back to
   `localStorage`, so the UI still runs.

   Which one is in use is decided by trying the plugin once, not by looking for
   a global: that global may not be injected yet while the bundle is starting. */

const FILE = "app-state.json";

let storePromise: Promise<Store | null> | null = null;
function getStore(): Promise<Store | null> {
  if (!storePromise) {
    // Writes are debounced: dragging the divider sets the ratio on every mouse
    // move, and they should reach the disk as one write.
    storePromise = load(FILE, { autoSave: 400 }).catch((e) => {
      console.warn(
        "[persist] tauri-plugin-store unavailable, falling back to localStorage",
        e,
      );
      return null;
    });
  }
  return storePromise;
}

function safeLocal<T>(fn: () => T): T | null {
  try {
    return fn();
  } catch {
    return null;
  }
}

export const persistStorage: StateStorage = {
  getItem: async (name) => {
    const store = await getStore();
    if (!store) return safeLocal(() => localStorage.getItem(name)) ?? null;
    return (await store.get<string>(name)) ?? null;
  },
  setItem: async (name, value) => {
    const store = await getStore();
    if (!store) {
      safeLocal(() => localStorage.setItem(name, value));
      return;
    }
    await store.set(name, value);
  },
  removeItem: async (name) => {
    const store = await getStore();
    if (!store) {
      safeLocal(() => localStorage.removeItem(name));
      return;
    }
    await store.delete(name);
  },
};
