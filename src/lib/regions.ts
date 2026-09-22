import { useUiStore } from "../state/uiStore";

/* Moving focus between the three regions with the keyboard.

   `Tab` loops between the two panes and never visits the sidebar; `Shift+Tab`
   enters the sidebar and leaves it again. Leaving a filter or rename field with
   `Tab` returns to the list of the same pane.

   `lastPane` remembers where to come back to. It holds the store index rather
   than what is visually left or right, so swapping the panes does not confuse
   it. */

let lastPane: 0 | 1 = 0;

function focusEl(selector: string): void {
  (document.querySelector(selector) as HTMLElement | null)?.focus();
}

/** Moves to the other pane; from the sidebar, back to the last pane used. */
export function toOtherPane(): void {
  const { activePane, setActivePane } = useUiStore.getState();
  const target: 0 | 1 =
    activePane === 0 ? 1 : activePane === 1 ? 0 : lastPane;
  lastPane = target;
  setActivePane(target);
  focusEl(`[data-pane="${target}"]`);
}

/** Enters the sidebar, or leaves it again if focus is already there. */
export function toSidebar(): void {
  const { activePane, setActivePane, setSidebarCursor } = useUiStore.getState();
  if (activePane === null) {
    setActivePane(lastPane);
    focusEl(`[data-pane="${lastPane}"]`);
    return;
  }
  lastPane = activePane;
  setSidebarCursor(0); // cursor starts on the first row
  setActivePane(null);
  focusEl('[data-region="sidebar"]');
}

/** Returns focus from a filter or rename field to that pane's list. */
export function backToPaneList(index: 0 | 1): void {
  lastPane = index;
  useUiStore.getState().setActivePane(index);
  focusEl(`[data-pane="${index}"]`);
}
