import { create } from "zustand";
import type { ReactNode } from "react";

/* The one open modal: a question to answer, something to read, or a name
   clash to resolve.

   A modal opens over the pane it is about, so the answer appears where the user
   is looking. Asked without a pane, it follows the active one; a window that
   belongs to no pane centres itself instead. */
interface DialogBase {
  /** Open over this pane rather than wherever focus happens to be. */
  paneIndex?: 0 | 1;
  /** Always centre on the window; for dialogs that belong to no pane. */
  center?: boolean;
}

export interface ConfirmDialog extends DialogBase {
  kind: "confirm";
  title: string;
  body: string;
  confirmLabel: string;
  /** Overrides the dismiss button's wording.
      Needed where a plain "Cancel" would be ambiguous: in "Cancel the scan?"
      it reads as either answer. */
  cancelLabel?: string;
  danger?: boolean;
  onConfirm: () => void;
}

export interface InfoDialog extends DialogBase {
  kind: "info";
  title: string;
  content: ReactNode;
}

export type ConflictAction = "overwrite" | "rename" | "skip" | "cancel";

/* A name that already exists at the destination. With more items still to
   come, the answer can be applied to all of them; dismissing the dialog cancels
   the transfer. */
export interface ConflictDialog extends DialogBase {
  kind: "conflict";
  name: string;
  destLabel: string;
  remaining: number;
  onResolve: (action: ConflictAction, applyToAll: boolean) => void;
}

type Dialog = ConfirmDialog | InfoDialog | ConflictDialog;

interface DialogState {
  dialog: Dialog | null;
  show: (d: Dialog) => void;
  close: () => void;
}

export const useDialogStore = create<DialogState>((set) => ({
  dialog: null,
  show: (d) => set({ dialog: d }),
  close: () => set({ dialog: null }),
}));
