import { create } from "zustand";

/* Which status bar popup is open, if any.

   The two popups exclude each other without knowing about each other: opening
   one writes its own name here, and the other closes itself when it sees a name
   that is not its own. */

export type StatusBarPopup = "transfers" | "clipboard" | null;

interface StatusBarPopupState {
  active: StatusBarPopup;
  setActive: (v: StatusBarPopup) => void;
}

export const useStatusBarPopupStore = create<StatusBarPopupState>((set) => ({
  active: null,
  setActive: (active) => set({ active }),
}));
