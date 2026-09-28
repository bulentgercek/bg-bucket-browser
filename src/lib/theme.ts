import { useUiStore, type Theme } from "../state/uiStore";

/* The theme is written to the document root from here, outside React: a
   change of theme needs no component to draw again, and in a long list that
   redraw alone takes a noticeable moment. "system" follows the desktop while
   the app runs, not only at startup. */

const lightQuery = () => window.matchMedia("(prefers-color-scheme: light)");

function resolve(theme: Theme): "dark" | "light" {
  if (theme !== "system") return theme;
  return lightQuery().matches ? "light" : "dark";
}

/* The theme last shown, for the splash in `index.html`, which is painted before
   the saved preferences can be read. Synchronous storage is the only kind it
   can reach in time; without it, the splash is dark. */
const SPLASH_THEME_KEY = "bgbb-theme";
function remember(resolved: "dark" | "light"): void {
  try {
    localStorage.setItem(SPLASH_THEME_KEY, resolved);
  } catch {
    // no storage: the splash falls back to dark
  }
}

/* Colour transitions are held back for a frame while the theme changes: each
   row of a long list would otherwise animate its own colours, the frames get
   slow, and the text sits in the old colour, unreadable on the new
   background. */
function write(theme: Theme): void {
  const root = document.documentElement;
  const next = resolve(theme);
  remember(next);
  if (root.dataset.theme === next) return;
  root.classList.add("theme-switching");
  root.dataset.theme = next;
  // The next frame works out the new colours with transitions off; the one
  // after gets them back.
  requestAnimationFrame(() =>
    requestAnimationFrame(() => root.classList.remove("theme-switching")),
  );
}

/** Applies the saved theme now and keeps it applied as it, or the desktop's
    preference, changes; returns the function that stops. */
export function followTheme(): () => void {
  const apply = () => write(useUiStore.getState().theme);
  apply();
  const stopStore = useUiStore.subscribe((s, prev) => {
    if (s.theme !== prev.theme) apply();
  });
  const mq = lightQuery();
  const onDesktop = () => {
    if (useUiStore.getState().theme === "system") apply();
  };
  mq.addEventListener("change", onDesktop);
  return () => {
    stopStore();
    mq.removeEventListener("change", onDesktop);
  };
}
