import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
import { useUiStore } from "./state/uiStore";
import { applyFontSize, followFontSize } from "./lib/zoom";
import { followTheme } from "./lib/theme";
import "./styles.css";

/* Nothing is drawn until the saved preferences are back, the window is at its
   saved font size and the theme is on the root. Drawn any earlier, the first
   frames would show the defaults and then visibly jump. From then on both
   follow the preferences without React. */

// A storage error never finishes loading the preferences, so the wait has a
// limit: the app then opens with the defaults rather than not at all.
const PREFERENCES_WAIT_MS = 1500;

function preferencesLoaded(): Promise<void> {
  const { persist } = useUiStore;
  if (persist.hasHydrated()) return Promise.resolve();
  return new Promise((resolve) => {
    const stop = persist.onFinishHydration(() => {
      stop();
      resolve();
    });
  });
}

async function start(): Promise<void> {
  await Promise.race([
    preferencesLoaded().then(() =>
      applyFontSize(useUiStore.getState().fontSize),
    ),
    new Promise((resolve) => setTimeout(resolve, PREFERENCES_WAIT_MS)),
  ]);
  followTheme();
  followFontSize();
  ReactDOM.createRoot(document.getElementById("root")!).render(
    <React.StrictMode>
      <App />
    </React.StrictMode>,
  );
}

void start();
