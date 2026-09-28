import {
  useCallback,
  useLayoutEffect,
  useRef,
  useState,
  type RefObject,
} from "react";
import { flushSync } from "react-dom";

/* Which rows of a long list are near enough to the screen to be drawn.

   Every row has the same height, so the rows further away are left out and
   stood in for by the space they would take, as padding above and below the
   ones drawn. The page then holds a few dozen rows whatever the size of the
   folder. The row height, and in the icon view the tiles per row, are measured
   from the rows as drawn, since both follow the view size and the pane's
   width. */

export interface VirtualRows {
  /** The first and last row drawn, inclusive. */
  first: number;
  last: number;
  /** Items per row: one in the details view, the tiles of a row in the icons. */
  perRow: number;
  /** The space standing in for the rows left out above and below. */
  padTop: number;
  padBottom: number;
  /** Scrolls just enough to bring the row holding `item` on screen. */
  reveal: (item: number) => void;
}

// Rows drawn past each edge of the screen, so that scrolling does not uncover
// empty space before the next rows are in.
const OVERSCAN = 6;
// Rows drawn before anything has been measured.
const FIRST_GUESS = 40;

interface Metrics {
  pitch: number; // a row's height plus the gap after it
  gap: number;
  perRow: number;
}

interface View extends Metrics {
  first: number;
  last: number;
}

/** @param listRef the element holding the rows
    @param present whether that element is on the page: a loading or empty
    state takes its place, and the list is measured again when it returns
    @param count items in the list
    @param basePad the list's own padding above and below its rows */
export function useVirtualRows(
  scrollRef: RefObject<HTMLElement | null>,
  listRef: RefObject<HTMLElement | null>,
  present: boolean,
  count: number,
  basePad: number,
): VirtualRows {
  const metrics = useRef<Metrics>({ pitch: 0, gap: 0, perRow: 1 });
  // The first item on the page as last drawn.
  const renderedFrom = useRef(0);
  const [view, setView] = useState<View>({
    first: 0,
    last: FIRST_GUESS - 1,
    pitch: 0,
    gap: 0,
    perRow: 1,
  });

  // Where the first row starts inside the scrolled content.
  const listTop = useCallback(
    (sc: HTMLElement, el: HTMLElement) =>
      el.getBoundingClientRect().top -
      sc.getBoundingClientRect().top +
      sc.scrollTop +
      basePad,
    [basePad],
  );

  const measure = useCallback(() => {
    const list = listRef.current;
    const row = list?.firstElementChild as HTMLElement | null;
    if (!list || !row) return;
    const style = getComputedStyle(list);
    const gap = parseFloat(style.rowGap) || 0;
    const perRow =
      style.display === "grid"
        ? Math.max(1, style.gridTemplateColumns.split(" ").filter(Boolean).length)
        : 1;
    metrics.current = { pitch: row.offsetHeight + gap, gap, perRow };
  }, [listRef]);

  const update = useCallback(
    (sync: boolean) => {
      const sc = scrollRef.current;
      const list = listRef.current;
      const m = metrics.current;
      if (!sc || !list || m.pitch <= 0) return;
      const rows = Math.ceil(count / m.perRow);
      const top = sc.scrollTop - listTop(sc, list);
      const first = Math.max(0, Math.floor(top / m.pitch) - OVERSCAN);
      const last = Math.min(
        rows - 1,
        Math.ceil((top + sc.clientHeight) / m.pitch) + OVERSCAN,
      );
      const next = (v: View): View =>
        v.first === first &&
        v.last === last &&
        v.pitch === m.pitch &&
        v.gap === m.gap &&
        v.perRow === m.perRow
          ? v
          : { first, last, ...m };
      // While scrolling, the new rows are put in before the frame is drawn;
      // otherwise a fast scroll shows an empty band for a moment.
      if (sync) flushSync(() => setView(next));
      else setView(next);
    },
    [scrollRef, listRef, count, listTop],
  );

  useLayoutEffect(() => {
    const list = present ? listRef.current : null;
    measure();
    const m = metrics.current;
    if (list && m.pitch > 0) {
      // The list takes its full height at once. The pane restores a tab's
      // scroll position right after this, and a list still holding only its
      // first rows would cut that position short.
      const rows = Math.ceil(count / m.perRow);
      const top = Math.floor(renderedFrom.current / m.perRow);
      const drawn = Math.ceil(list.children.length / m.perRow);
      list.style.paddingTop = `${basePad + top * m.pitch}px`;
      list.style.paddingBottom = `${basePad + Math.max(0, rows - top - drawn) * m.pitch}px`;
    }
    update(false);
    const sc = scrollRef.current;
    if (!sc || !list) return;
    const onScroll = () => update(true);
    const onResize = () => {
      measure();
      update(false);
    };
    sc.addEventListener("scroll", onScroll, { passive: true });
    const ro = new ResizeObserver(onResize);
    ro.observe(sc);
    // The border box: its width changes the tiles per row, while the rows
    // coming and going inside it do not change it.
    ro.observe(list, { box: "border-box" });
    return () => {
      sc.removeEventListener("scroll", onScroll);
      ro.disconnect();
    };
  }, [measure, update, scrollRef, listRef, present, count, basePad]);

  const reveal = useCallback(
    (item: number) => {
      const sc = scrollRef.current;
      const list = listRef.current;
      const { pitch, gap, perRow } = metrics.current;
      if (!sc || !list || pitch <= 0 || item < 0) return;
      const top = listTop(sc, list) + Math.floor(item / perRow) * pitch;
      const bottom = top + pitch - gap;
      if (top < sc.scrollTop) sc.scrollTop = top;
      else if (bottom > sc.scrollTop + sc.clientHeight) {
        sc.scrollTop = bottom - sc.clientHeight;
      }
    },
    [scrollRef, listRef, listTop],
  );

  const rows = Math.ceil(count / view.perRow);
  const last = Math.min(view.last, rows - 1);
  const first = Math.min(view.first, Math.max(0, last));
  renderedFrom.current = first * view.perRow;
  return {
    first,
    last,
    perRow: view.perRow,
    padTop: first * view.pitch,
    padBottom: Math.max(0, rows - 1 - last) * view.pitch,
    reveal,
  };
}
