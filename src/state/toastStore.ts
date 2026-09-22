import { create } from "zustand";
import { devlogToast, devlogVerbose } from "../lib/commands";

/* Short notices: something was created, something failed, something is being
   deleted. They disappear on their own, or when clicked.

   A sticky toast stays until it is settled, and pushing the same id again edits
   the text in place — that is how a running count reports progress. */

export type ToastTone = "info" | "error";

export interface Toast {
  id: string;
  text: string;
  tone: ToastTone;
  /** A progress toast, which waits to be settled instead of timing out. */
  sticky?: boolean;
}

interface ToastState {
  toasts: Toast[];
  /** Shows a toast, or updates the one with this id, and returns its id. */
  push: (
    text: string,
    tone?: ToastTone,
    opts?: { id?: string; sticky?: boolean },
  ) => string;
  /** Ends a progress toast with a final message, which then times out. */
  settle: (id: string, text: string, tone?: ToastTone) => void;
  dismiss: (id: string) => void;
}

export const useToastStore = create<ToastState>((set, get) => ({
  toasts: [],
  push: (text, tone = "info", opts) => {
    const id = opts?.id ?? crypto.randomUUID();
    const sticky = opts?.sticky ?? false;
    // Every toast goes to the development log. Progress updates of an already visible
    // sticky toast go to the verbose branch only, so toast.log stays short.
    const updatingSticky = sticky && get().toasts.some((t) => t.id === id);
    if (updatingSticky) devlogVerbose("toast", `${tone} ${text}`);
    else devlogToast(tone, text);
    set((s) => {
      const exists = s.toasts.some((t) => t.id === id);
      return {
        toasts: exists
          ? s.toasts.map((t) =>
              t.id === id ? { ...t, text, tone, sticky } : t,
            )
          : [...s.toasts, { id, text, tone, sticky }],
      };
    });
    if (!sticky) {
      setTimeout(() => {
        set((s) => ({ toasts: s.toasts.filter((t) => t.id !== id) }));
      }, 4000);
    }
    return id;
  },
  settle: (id, text, tone = "info") => {
    devlogToast(tone, text);
    set((s) => ({
      toasts: s.toasts.map((t) =>
        t.id === id ? { ...t, text, tone, sticky: false } : t,
      ),
    }));
    setTimeout(() => {
      set((s) => ({ toasts: s.toasts.filter((t) => t.id !== id) }));
    }, 4000);
  },
  dismiss: (id) => set((s) => ({ toasts: s.toasts.filter((t) => t.id !== id) })),
}));
