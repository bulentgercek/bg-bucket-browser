import { useCallback, useRef, useState } from "react";
import { ArrowsLeftRightIcon } from "@phosphor-icons/react";
import { useUiStore } from "../../state/uiStore";
import { t } from "../../locale/en";

/* The strip between the panes, and the button sitting in it.

   Dragging either one resizes the panes; double clicking either one centres
   them; clicking the button swaps the panes over. One gesture handler serves
   both, because a press on the button reaches the strip anyway.

   The three gestures have to be told apart from one another: movement past a
   few pixels makes it a drag, a drag that started on the button swallows the
   click that follows it, and a single click waits briefly to see whether a
   second one is coming. */

const DIVIDER_PX = 30;
const DRAG_THRESHOLD = 4; // px of movement before a press counts as a drag
const CLICK_DELAY = 220; // ms to wait for a second click before acting on the first

export default function Divider() {
  const toggleFlipped = useUiStore((s) => s.toggleFlipped);
  const centerSplit = useUiStore((s) => s.centerSplit);
  const setSplitRatio = useUiStore((s) => s.setSplitRatio);

  const stripRef = useRef<HTMLDivElement>(null);
  const buttonRef = useRef<HTMLButtonElement>(null);
  const clickTimer = useRef<number | null>(null);
  const suppressClick = useRef(false); // a drag that ended on the button

  // True while the button is held, only so that it can show the resize cursor:
  // its own cursor style wins over the one set on the document.
  const [pressing, setPressing] = useState(false);

  // ── Dragging, from the strip or from the button ──
  const beginDrag = useCallback(
    (e: React.MouseEvent) => {
      if (e.button !== 0) return;
      const area = stripRef.current?.parentElement;
      if (!area) return;
      e.preventDefault();

      const startedOnButton = !!buttonRef.current?.contains(e.target as Node);
      const rect = area.getBoundingClientRect(); // taken once; the window cannot resize mid-drag
      const usable = rect.width - DIVIDER_PX;
      const startX = e.clientX;
      let moved = false;
      setPressing(true);

      const move = (ev: MouseEvent) => {
        if (!moved && Math.abs(ev.clientX - startX) < DRAG_THRESHOLD) return;
        moved = true;
        // The pointer sits in the middle of the strip, so its width comes off.
        const ratio = (ev.clientX - rect.left - DIVIDER_PX / 2) / usable;
        setSplitRatio(ratio); // clamped where it is stored
      };
      const up = () => {
        window.removeEventListener("mousemove", move);
        window.removeEventListener("mouseup", up);
        document.body.style.cursor = "";
        setPressing(false);
        if (moved && startedOnButton) suppressClick.current = true;
      };

      window.addEventListener("mousemove", move);
      window.addEventListener("mouseup", up);
      // The resize cursor appears on press, before any movement.
      document.body.style.cursor = "col-resize";
    },
    [setSplitRatio],
  );

  // ── The button: click swaps, double click centres ──
  const onBtnClick = useCallback(() => {
    if (suppressClick.current) {
      suppressClick.current = false; // this click is the tail of a drag
      return;
    }
    if (clickTimer.current !== null) return; // a double click is being resolved
    clickTimer.current = window.setTimeout(() => {
      clickTimer.current = null;
      toggleFlipped();
    }, CLICK_DELAY);
  }, [toggleFlipped]);

  const onBtnDoubleClick = useCallback(
    (e: React.MouseEvent) => {
      e.stopPropagation(); // the strip would otherwise centre twice
      if (clickTimer.current !== null) {
        window.clearTimeout(clickTimer.current);
        clickTimer.current = null;
      }
      centerSplit();
    },
    [centerSplit],
  );

  return (
    <div
      ref={stripRef}
      onMouseDown={beginDrag}
      onDoubleClick={centerSplit}
      className="flex cursor-col-resize items-center justify-center border-x border-neutral-900 bg-chrome"
    >
      <button
        ref={buttonRef}
        type="button"
        onClick={onBtnClick}
        onDoubleClick={onBtnDoubleClick}
        className={
          "flex h-6 w-6 items-center justify-center rounded-full border border-accent-700 bg-bg text-accent-300 transition-colors hover:border-accent-600 hover:bg-accent-900 hover:text-accent-200 " +
          (pressing ? "cursor-col-resize" : "cursor-pointer")
        }
        aria-label={t("main.flip")}
      >
        <ArrowsLeftRightIcon size={12} />
      </button>
    </div>
  );
}
