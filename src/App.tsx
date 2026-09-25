import { useEffect } from "react";
import { useUiStore } from "./state/uiStore";
import { useConnectionStore } from "./state/connectionStore";
import { useDeviceStore } from "./state/deviceStore";
import { initTransferEvents } from "./state/transferQueueStore";
import { useFeedbackStore } from "./state/feedbackStore";
import { initOsDrop } from "./lib/osDrop";
import MainScreen from "./features/main/MainScreen";
import SettingsScreen from "./features/settings/SettingsScreen";
import ContextMenu from "./features/context-menu/ContextMenu";
import Toast from "./features/toast/Toast";
import Dialog from "./features/dialog/Dialog";

/* The shell: one window with two screens, plus the three things that live
   above both of them — the context menu, the dialog and the notices.

   The theme is applied to the document root. Following the system means
   following it while the app runs, not only at startup.

   The webview's own context menu is suppressed, except in text fields, where
   the native copy and paste entries are worth more than ours. */
export default function App() {
  const screen = useUiStore((s) => s.screen);
  const theme = useUiStore((s) => s.theme);

  useEffect(() => {
    const root = document.documentElement;
    const apply = () => {
      const resolved =
        theme === "system"
          ? window.matchMedia("(prefers-color-scheme: light)").matches
            ? "light"
            : "dark"
          : theme;
      root.dataset.theme = resolved;
    };
    apply();
    if (theme !== "system") return;
    const mq = window.matchMedia("(prefers-color-scheme: light)");
    mq.addEventListener("change", apply);
    return () => mq.removeEventListener("change", apply);
  }, [theme]);

  useEffect(() => {
    void useConnectionStore.getState().load();
    void useDeviceStore.getState().load();
    void initTransferEvents();
    // A feedback recording left from the last run is offered again.
    void useFeedbackStore.getState().init();
    void initOsDrop();

    const onCtx = (e: MouseEvent) => {
      const el = e.target as HTMLElement | null;
      if (el?.closest("input, textarea, [contenteditable='true']")) return;
      e.preventDefault();
    };
    document.addEventListener("contextmenu", onCtx);
    return () => document.removeEventListener("contextmenu", onCtx);
  }, []);

  return (
    <>
      {screen === "settings" ? <SettingsScreen /> : <MainScreen />}
      <ContextMenu />
      <Dialog />
      <Toast />
    </>
  );
}
