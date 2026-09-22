import { create } from "zustand";
import { persist, createJSONStorage } from "zustand/middleware";
import { persistStorage } from "../lib/persist";

/* Window-wide UI state: which screen is open, how the panes are arranged, what
   the user's display preferences are.

   Preferences are written to disk, the rest is not — the app always opens on
   the main screen. Loading them back is asynchronous, so the first frame shows
   the defaults and then switches. */

export type Screen = "main" | "settings";
export type Theme = "dark" | "light" | "system";
export type SettingsTab = "connections" | "appearance" | "about";

// Neither pane may shrink away completely.
const MIN_SPLIT = 0.2;
const MAX_SPLIT = 0.8;
const clampSplit = (r: number): number =>
  Number.isFinite(r) ? Math.min(MAX_SPLIT, Math.max(MIN_SPLIT, r)) : 0.5;

interface UiState {
  screen: Screen;
  setScreen: (screen: Screen) => void;

  /** Which Settings tab is open. The Settings button keeps whatever was open
      last; clicking the app name goes to About. */
  settingsTab: SettingsTab;
  setSettingsTab: (tab: SettingsTab) => void;

  /** Swaps which side of the window each pane is drawn on. */
  flipped: boolean;
  /** Single click on the flip button. */
  toggleFlipped: () => void;

  /** How much of the width the left-hand pane gets, as a fraction. */
  splitRatio: number;
  /** Set while dragging the divider; out-of-range values are clamped. */
  setSplitRatio: (ratio: number) => void;
  /** Double click on the flip button centres the divider. */
  centerSplit: () => void;

  /** Whether dot files are listed, in both panes at once. */
  showHidden: boolean;
  toggleHidden: () => void;

  /** Theme preference; "system" follows the desktop and keeps following it
      while the app is open. */
  theme: Theme;
  setTheme: (theme: Theme) => void;

  /** What a file dropped from outside does: copy straight away, or ask.
      Copying is the default because a move would delete the user's own file
      somewhere the app does not show. */
  osDropMode: "copy" | "menu";
  setOsDropMode: (mode: "copy" | "menu") => void;

  /** Which pane keyboard commands act on: 0 is remote, 1 is local. The pointer
      entering a pane sets it, and leaving does not clear it — only moving to
      the sidebar does, which is what "no active pane" means. */
  activePane: 0 | 1 | null;
  setActivePane: (i: 0 | 1 | null) => void;

  /** The sidebar's keyboard cursor, used while the sidebar is the active
      region. */
  sidebarCursor: number;
  setSidebarCursor: (i: number) => void;

  /** One counter per pane, bumped when something outside the pane's own key
      handling puts its cursor on a row for the keyboard, such as leaving the
      filter. The pane then draws its cursor. */
  cursorReveal: [number, number];
  revealCursor: (i: 0 | 1) => void;
}

export const useUiStore = create<UiState>()(
  persist(
    (set) => ({
      screen: "main",
      setScreen: (screen) => set({ screen }),

      settingsTab: "connections",
      setSettingsTab: (settingsTab) => set({ settingsTab }),

      flipped: false,
      toggleFlipped: () => set((s) => ({ flipped: !s.flipped })),

      splitRatio: 0.5,
      setSplitRatio: (ratio) => set({ splitRatio: clampSplit(ratio) }),
      centerSplit: () => set({ splitRatio: 0.5 }),

      showHidden: false,
      toggleHidden: () => set((s) => ({ showHidden: !s.showHidden })),

      theme: "dark",
      setTheme: (theme) => set({ theme }),

      osDropMode: "copy",
      setOsDropMode: (mode) => set({ osDropMode: mode }),

      activePane: null,
      setActivePane: (i) => set((s) => (s.activePane === i ? s : { activePane: i })),

      sidebarCursor: 0,
      setSidebarCursor: (i) =>
        set((s) => (s.sidebarCursor === i ? s : { sidebarCursor: i })),

      cursorReveal: [0, 0],
      revealCursor: (i) =>
        set((s) => {
          const next: [number, number] = [...s.cursorReveal];
          next[i] += 1;
          return { cursorReveal: next };
        }),
    }),
    {
      name: "ui",
      storage: createJSONStorage(() => persistStorage),
      // Only real preferences are stored; screen state and actions are not.
      partialize: (s) => ({
        flipped: s.flipped,
        splitRatio: s.splitRatio,
        showHidden: s.showHidden,
        theme: s.theme,
        osDropMode: s.osDropMode,
      }),
    },
  ),
);
