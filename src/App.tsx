import { useEffect } from "react";
import { useUiStore } from "./state/uiStore";
import { useConnectionStore } from "./state/connectionStore";
import { useDeviceStore } from "./state/deviceStore";
import { initTransferEvents } from "./state/transferQueueStore";
import { useFeedbackStore } from "./state/feedbackStore";
import { initOsDrop } from "./lib/osDrop";
import { initZoomControls } from "./lib/zoom";
import { initReloadGuard } from "./lib/reloadGuard";
import MainScreen from "./features/main/MainScreen";
import SettingsScreen from "./features/settings/SettingsScreen";
import ContextMenu from "./features/context-menu/ContextMenu";
import Toast from "./features/toast/Toast";
import Dialog from "./features/dialog/Dialog";

/* The shell: one window with two screens, plus the three things that live
   above both of them — the context menu, the dialog and the notices.

   The theme and the font size are not applied here: they are in place before
   the first frame (`main.tsx`) and follow the preferences from outside React
   (`lib/theme.ts`, `lib/zoom.ts`), so changing either redraws nothing.

   The webview's own context menu is suppressed, except in text fields, where
   the native copy and paste entries are worth more than ours, and its reload
   keys never apply (`lib/reloadGuard.ts`). */
export default function App() {
  const screen = useUiStore((s) => s.screen);

  useEffect(() => {
    const stopReload = initReloadGuard();
    void useConnectionStore.getState().load();
    void useDeviceStore.getState().load();
    void initTransferEvents();
    // A feedback recording left from the last run is offered again.
    void useFeedbackStore.getState().init();
    void initOsDrop();
    const stopZoom = initZoomControls();

    const onCtx = (e: MouseEvent) => {
      const el = e.target as HTMLElement | null;
      if (el?.closest("input, textarea, [contenteditable='true']")) return;
      e.preventDefault();
    };
    document.addEventListener("contextmenu", onCtx);
    return () => {
      stopReload();
      stopZoom();
      document.removeEventListener("contextmenu", onCtx);
    };
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
