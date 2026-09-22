import { create } from "zustand";
import type { ReactNode } from "react";

/* The one open context menu.

   Whoever opens it supplies the items; a single menu component at the root of
   the app draws them at the pointer. */

export interface MenuItem {
  id: string;
  /** Plain text, or a node when part of the label is styled differently. */
  label?: ReactNode;
  icon?: ReactNode;
  /** Key hint shown right-aligned on the row. */
  shortcut?: string;
  disabled?: boolean;
  /** Draws the row in the destructive tone. */
  danger?: boolean;
  /** A separator line rather than a row. */
  separator?: boolean;
  onSelect?: () => void;
}

export interface MenuOptions {
  /** The point is the menu's top-right corner rather than its top-left, so the
      menu opens leftwards. For a button at the right end of a pane: the window
      edge alone would let its menu spill over the pane next to it. */
  alignRight?: boolean;
  /** The button that opened the menu. A press on it is left to the button, so
      its click can close the menu instead of closing and reopening it. */
  anchor?: HTMLElement;
}

interface ContextMenuState {
  open: boolean;
  x: number;
  y: number;
  alignRight: boolean;
  anchor: HTMLElement | null;
  items: MenuItem[];
  openMenu: (
    x: number,
    y: number,
    items: MenuItem[],
    opts?: MenuOptions,
  ) => void;
  closeMenu: () => void;
}

export const useContextMenu = create<ContextMenuState>((set) => ({
  open: false,
  x: 0,
  y: 0,
  alignRight: false,
  anchor: null,
  items: [],
  openMenu: (x, y, items, opts) =>
    set({
      open: true,
      x,
      y,
      items,
      alignRight: opts?.alignRight ?? false,
      anchor: opts?.anchor ?? null,
    }),
  closeMenu: () => set({ open: false, items: [], anchor: null }),
}));
