import { create } from "zustand";
import {
  feedbackDiscardRecording,
  feedbackIsTest,
  feedbackRecordStart,
  feedbackRecordStop,
  feedbackRecording,
  feedbackSend,
  type FeedbackErr,
  type RecordingInfo,
} from "../lib/commands";
import { useUiStore } from "./uiStore";
import { useToastStore } from "./toastStore";
import { useDialogStore } from "./dialogStore";
import { t } from "../locale/en";

/* The Feedback screen's state: what the user wrote, the recording that goes
   with it, and whether a recording is running.

   Recording is two steps from the user's side: Record the problem closes
   Settings and puts a pill in the status bar; Stop & send brings the Feedback
   screen back with the recording attached in a box of its own. The recording
   itself lives in a file on the Rust side, so it survives a crash and is
   offered again on the next start.

   Nothing here is kept between runs except that file. */

/** How long a recording may run; the backend stops it at the same limit. */
export const RECORDING_MAX_SEC = 5 * 60;

interface FeedbackState {
  message: string;
  contact: string;
  /** The finished recording that goes with the next report. */
  attached: RecordingInfo | null;
  /** When the running recording started (ms), or null when none runs. */
  recordingSince: number | null;
  sending: boolean;
  /** Development builds send to the service's test channel. */
  isTest: boolean;
  setMessage: (v: string) => void;
  setContact: (v: string) => void;
  startRecording: () => Promise<void>;
  stopRecording: () => Promise<void>;
  removeRecording: () => Promise<void>;
  send: () => Promise<void>;
  /** At startup: picks up a recording left from the last run. */
  init: () => Promise<void>;
}

function openFeedback() {
  const ui = useUiStore.getState();
  ui.setSettingsTab("feedback");
  ui.setScreen("settings");
}

function errText(e: unknown): string {
  const err = e as Partial<FeedbackErr> | null;
  switch (err?.kind) {
    case "invalid":
      return err.field === "contact"
        ? t("feedback.err.invalidContact")
        : t("feedback.err.invalid");
    case "tooLarge":
      return t("feedback.err.tooLarge");
    case "rateLimited":
      return t("feedback.err.rateLimited");
    case "network":
      return t("feedback.err.network");
    default:
      return t("feedback.err.server");
  }
}

export const useFeedbackStore = create<FeedbackState>((set, get) => ({
  message: "",
  contact: "",
  attached: null,
  recordingSince: null,
  sending: false,
  isTest: false,
  setMessage: (message) => set({ message }),
  setContact: (contact) => set({ contact }),

  startRecording: async () => {
    try {
      await feedbackRecordStart();
    } catch {
      useToastStore.getState().push(t("feedback.err.record"), "error");
      return;
    }
    set({ attached: null, recordingSince: Date.now() });
    // Back to the main window, where the problem happens.
    useUiStore.getState().setScreen("main");
  },

  stopRecording: async () => {
    if (get().recordingSince === null) return;
    const info = await feedbackRecordStop().catch(() => null);
    set({ attached: info, recordingSince: null });
    openFeedback();
  },

  removeRecording: async () => {
    await feedbackDiscardRecording().catch(() => {});
    set({ attached: null });
  },

  send: async () => {
    const { message, contact, attached, sending } = get();
    if (sending || (!message.trim() && !attached)) return;
    set({ sending: true });
    try {
      const id = await feedbackSend(message, contact, attached !== null);
      // The address is kept for the next report; the rest is done.
      set({ message: "", attached: null });
      useToastStore.getState().push(t("feedback.sent", { id }));
    } catch (e) {
      // Nothing is lost: the message stays in the form and the recording on disk.
      useToastStore.getState().push(errText(e), "error");
    } finally {
      set({ sending: false });
    }
  },

  init: async () => {
    feedbackIsTest()
      .then((isTest) => set({ isTest }))
      .catch(() => {});
    const info = await feedbackRecording().catch(() => null);
    if (!info) return;
    // Still running: the window was reloaded while recording.
    if (info.active) {
      set({ recordingSince: Date.now() - info.durationSec * 1000 });
      return;
    }
    // Left from the last run: offered once, and kept until sent or discarded.
    useDialogStore.getState().show({
      kind: "confirm",
      center: true,
      title: t("feedback.recovered.title"),
      body: t(info.cutShort ? "feedback.recovered.bodyCut" : "feedback.recovered.body"),
      confirmLabel: t("feedback.recovered.confirm"),
      cancelLabel: t("feedback.recovered.discard"),
      onConfirm: () => {
        useDialogStore.getState().close();
        set({ attached: info });
        openFeedback();
      },
      onCancel: () => {
        void feedbackDiscardRecording().catch(() => {});
      },
    });
  },
}));
