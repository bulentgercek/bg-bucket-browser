import { useEffect, useRef } from "react";
import {
  usePaneStore,
  type Entry,
  type PaneIndex,
  type TabState,
} from "../../state/paneStore";
import { useUiStore } from "../../state/uiStore";
import { useContextMenu } from "../../state/contextMenuStore";
import { useDialogStore } from "../../state/dialogStore";
import {
  keyboardTargets,
  registerKeyboardTargets,
} from "../../lib/keyboardTargets";

/* Moving around a pane with the keyboard, and what its keyboard actions act on.

   Bound to the window and acting only while this pane is the active one, the
   same way the transfer keys work. A text field, an open menu or a dialog
   takes precedence. Returns the cursor as the pane should draw it, and a way
   for a mouse click to end a keyboard shift-selection. */
export function usePaneKeyboard({
  index,
  active,
  remote,
  tab,
  visible,
  handleOpen,
  requestDelete,
  openFilter,
}: {
  index: PaneIndex;
  active: boolean;
  remote: boolean;
  tab: TabState;
  /** The entries as the list shows them, filtered and sorted. */
  visible: Entry[];
  handleOpen: (e: Entry) => void;
  requestDelete: (targets: Entry[], permanent: boolean) => void;
  /** Opens the filter field and puts the text cursor in it. */
  openFilter: () => void;
}): { drawnCursor: number; endShiftGesture: () => void } {
  const goParent = usePaneStore((s) => s.goParent);
  const setSelection = usePaneStore((s) => s.setSelection);
  const closeTab = usePaneStore((s) => s.closeTab);
  const beginRename = usePaneStore((s) => s.beginRename);
  const setCursor = usePaneStore((s) => s.setCursor);
  const cycleTab = usePaneStore((s) => s.cycleTab);

  const visibleRef = useRef<Entry[]>(visible);
  visibleRef.current = visible;
  const handleOpenRef = useRef<(e: Entry) => void>(() => {});
  handleOpenRef.current = handleOpen;
  const requestDeleteRef = useRef<
    (targets: Entry[], permanent: boolean) => void
  >(() => {});
  requestDeleteRef.current = requestDelete;
  const openFilterRef = useRef(openFilter);
  openFilterRef.current = openFilter;
  // Typing letters jumps to a matching entry; the letters accumulate until a
  // pause.
  const typeaheadBuf = useRef("");
  const typeaheadAt = useRef(0);
  // Whether the last key was part of an ongoing shift-selection.
  const shiftGesture = useRef(false);
  // Whether a navigation key has actually been pressed in this pane. The cursor
  // has a position at all times, but it should only be drawn once the keyboard
  // has been used here. Coming back to the pane keeps it: the cursor is where
  // it was left.
  const kbActive = useRef(false);
  // Leaving the filter is keyboard use in this pane too; the store says so by
  // bumping a counter, and this render picks it up before drawing.
  const cursorReveal = useUiStore((s) => s.cursorReveal[index]);
  const revealSeen = useRef(cursorReveal);
  if (revealSeen.current !== cursorReveal) {
    revealSeen.current = cursorReveal;
    kbActive.current = true;
  }
  // The cursor as this render draws it: its row, -1 on `..`, -2 when none is
  // drawn. Keyboard actions read this rather than the flag above, because the
  // flag can change without a render and would then disagree with the screen.
  const drawnCursor =
    active && kbActive.current && tab.viewMode === "details"
      ? tab.cursorIndex
      : -2;
  const drawnCursorRef = useRef(drawnCursor);
  drawnCursorRef.current = drawnCursor;

  useEffect(() => {
    if (!active) return;
    const PAGE = 12;
    const onKey = (e: KeyboardEvent) => {
      const ae = document.activeElement as HTMLElement | null;
      if (
        ae &&
        (ae.tagName === "INPUT" ||
          ae.tagName === "TEXTAREA" ||
          ae.isContentEditable)
      )
        return;
      if (useContextMenu.getState().open) return;
      if (useDialogStore.getState().dialog) return;

      // The shortcuts that apply whatever the pane is showing, so they come
      // before the checks below.
      if ((e.ctrlKey || e.metaKey) && !e.altKey && !e.shiftKey) {
        const ck = e.key.toLowerCase();
        if (e.key === "ArrowRight") {
          e.preventDefault();
          cycleTab(index, 1);
          return;
        }
        if (e.key === "ArrowLeft") {
          e.preventDefault();
          cycleTab(index, -1);
          return;
        }
        if (ck === "w") {
          e.preventDefault();
          closeTab(index, usePaneStore.getState().panes[index].activeTabId);
          return;
        }
        if (ck === "f") {
          e.preventDefault();
          setCursor(index, -2); // the cursor hides while the filter has focus
          openFilterRef.current();
          return;
        }
        if (ck === "a") {
          e.preventDefault();
          setSelection(
            index,
            visibleRef.current.map((r) => r.name),
            null,
          );
          return;
        }
        // Anything else with control is not ours.
      }

      const st = usePaneStore.getState().panes[index];
      const cur = st.tabs.find((tb) => tb.id === st.activeTabId);
      if (!cur || cur.status !== "ready" || cur.viewMode !== "details") return;

      const rows = visibleRef.current;
      const names = rows.map((r) => r.name);
      const max = rows.length - 1;
      // A hidden cursor still has a place to move from.
      const c = Math.max(-1, cur.cursorIndex);
      const clamp = (n: number) => Math.max(-1, Math.min(max, n));
      const clampRow = (n: number) => Math.max(0, Math.min(max, n)); // entries only, not `..`

      // Consecutive shift-arrows extend one block from one anchor; any other
      // key ends that, and the next shift-arrow starts a new block from wherever
      // the cursor is now.
      const shiftArrow =
        (e.key === "ArrowDown" || e.key === "ArrowUp") && e.shiftKey;
      if (!shiftArrow) shiftGesture.current = false;

      // The control shortcuts were handled above.
      if ((e.ctrlKey || e.metaKey) && !e.altKey) return;

      // Shift and an arrow select the block between the anchor and the cursor.
      // Coming back onto the anchor empties the selection rather than leaving
      // that one row: every step can be taken back exactly.
      if (shiftArrow) {
        e.preventDefault();
        kbActive.current = true;
        if (max < 0) return;
        const from = c < 0 ? 0 : c;
        const to = clampRow(from + (e.key === "ArrowDown" ? 1 : -1));
        // A new block anchors where the cursor is; an ongoing one keeps its
        // anchor.
        let anchor = shiftGesture.current ? cur.anchor : null;
        if (!anchor || names.indexOf(anchor) === -1) anchor = names[from] ?? null;
        const ai = anchor ? names.indexOf(anchor) : to;
        const [lo, hi] = ai < to ? [ai, to] : [to, ai];
        const nextSel = ai === to ? [] : names.slice(lo, hi + 1);
        setSelection(index, nextSel, anchor, "add");
        setCursor(index, to);
        shiftGesture.current = true;
        return;
      }

      switch (e.key) {
        case "ArrowDown":
          e.preventDefault();
          kbActive.current = true;
          setCursor(index, clamp(c + 1));
          break;
        case "ArrowUp":
          e.preventDefault();
          kbActive.current = true;
          setCursor(index, clamp(c - 1));
          break;
        case " ": {
          // Space toggles the row under the cursor, as a control-click does;
          // with no cursor on screen there is nothing to toggle.
          e.preventDefault();
          const d = drawnCursorRef.current;
          if (d < 0 || !rows[d]) break;
          const nm = rows[d].name;
          const has = cur.selection.includes(nm);
          setSelection(
            index,
            has
              ? cur.selection.filter((n) => n !== nm)
              : [...cur.selection, nm],
            nm,
            has ? "sub" : "add",
          );
          break;
        }
        case "Escape":
          e.preventDefault();
          setSelection(index, [], null);
          break;
        case "Delete":
          e.preventDefault();
          // The selection, or the row under the cursor, as for copying.
          // Shift deletes permanently; the volume has no trash either way.
          requestDeleteRef.current(
            keyboardTargets(index),
            remote || e.shiftKey,
          );
          break;
        case "F2": {
          e.preventDefault();
          // Renaming takes exactly one entry: the selected one, or the one
          // under the cursor when nothing is selected.
          const targets = keyboardTargets(index);
          if (targets.length === 1) beginRename(index, targets[0].name);
          break;
        }
        case "PageDown":
          e.preventDefault();
          kbActive.current = true;
          setCursor(index, clamp(c + PAGE));
          break;
        case "PageUp":
          e.preventDefault();
          kbActive.current = true;
          setCursor(index, clamp(c - PAGE));
          break;
        case "Home":
          e.preventDefault();
          kbActive.current = true;
          setCursor(index, -1); // the very top is the `..` row
          break;
        case "End":
          e.preventDefault();
          kbActive.current = true;
          setCursor(index, max); // an empty list leaves it on `..`
          break;
        case "Enter": {
          e.preventDefault();
          // Only a cursor on screen opens anything; with none drawn the user
          // was not pointing at a row.
          const d = drawnCursorRef.current;
          if (d === -1) void goParent(index);
          else if (d >= 0 && rows[d]) handleOpenRef.current(rows[d]);
          break;
        }
        case "Backspace":
          e.preventDefault();
          void goParent(index);
          break;
        default: {
          // Typing a letter jumps to the first entry starting with what has
          // been typed so far. A pause starts over, and repeating one letter
          // steps through the entries beginning with it.
          if (
            e.key.length !== 1 ||
            e.key === " " ||
            e.ctrlKey ||
            e.metaKey ||
            e.altKey
          )
            return;
          e.preventDefault();
          kbActive.current = true;
          const now = Date.now();
          const ch = e.key.toLowerCase();
          const fresh = now - typeaheadAt.current > 800;
          typeaheadAt.current = now;
          const cycle = !fresh && typeaheadBuf.current === ch;
          if (fresh) typeaheadBuf.current = ch;
          else if (!cycle) typeaheadBuf.current += ch;
          const buf = typeaheadBuf.current;
          const lower = names.map((n) => n.toLowerCase());
          const start = cycle ? (c < 0 ? 0 : c + 1) : 0;
          for (let k = 0; k < lower.length; k++) {
            const idx = (start + k) % lower.length;
            if (lower[idx].startsWith(buf)) {
              setCursor(index, idx);
              break;
            }
          }
          break;
        }
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [
    active,
    index,
    remote,
    setCursor,
    setSelection,
    goParent,
    beginRename,
    cycleTab,
    closeTab,
  ]);

  // What keyboard actions on entries act on in this pane (copy, cut, F5 and F6
  // in the main screen; Delete and F2 above): the selection, or the row under
  // the cursor while the cursor is drawn.
  useEffect(() => {
    registerKeyboardTargets(index, () => {
      const st = usePaneStore.getState().panes[index];
      const tb = st.tabs.find((x) => x.id === st.activeTabId) ?? st.tabs[0];
      const selected = tb.listing.filter((en) => tb.selection.includes(en.name));
      if (selected.length > 0) return selected;
      // On `..` (-1) or with no cursor drawn (-2) there is no entry.
      const row = visibleRef.current[drawnCursorRef.current];
      return row ? [row] : [];
    });
    return () => registerKeyboardTargets(index, null);
  }, [index]);

  return {
    drawnCursor,
    endShiftGesture: () => {
      shiftGesture.current = false;
    },
  };
}
