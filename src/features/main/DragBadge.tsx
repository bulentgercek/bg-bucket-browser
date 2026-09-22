import { useEffect } from "react";
import { createPortal } from "react-dom";
import { FilesIcon, ProhibitIcon } from "@phosphor-icons/react";
import { useDragStore } from "../../state/dragStore";
import { IS_WINDOWS } from "../../lib/platform";
import { t } from "../../locale/en";

/* The badge that follows the pointer during a drag: how many items, and where
   they would land.

   No verb: whether it copies or moves is chosen after the drop.

   Over anything that is not a valid target it says so instead. On Windows it
   simply disappears there, because the system cursor already shows a refusal
   and two signs saying the same thing is one too many; on Linux the system
   cursor cannot be relied on, and this badge is the only feedback there is. */
export default function DragBadge() {
  const payload = useDragStore((s) => s.payload);
  const cursor = useDragStore((s) => s.cursor);
  const dest = useDragStore((s) => s.dest);

  // Keeps one cursor for the whole drag. During an HTML5 drag the system, not
  // CSS, decides the cursor, and it flickers between shapes wherever nothing
  // accepts the drag — so the whole window accepts it, and a drop outside a
  // pane is swallowed.
  useEffect(() => {
    if (!payload) return;
    document.body.classList.add("dnd-dragging");
    // Caught on the way down, so a child that stops the event cannot get past
    // it. Entering has to be handled as well as moving: without it the cursor
    // resets between the two and flickers.
    const lock = (e: DragEvent) => {
      e.preventDefault();
      // The position is updated everywhere, not only over a pane, so the badge
      // never freezes in place when the pointer leaves one.
      useDragStore.getState().update({ cursor: { x: e.clientX, y: e.clientY } });
      if (e.dataTransfer) e.dataTransfer.dropEffect = "move";
    };
    const swallow = (e: DragEvent) => e.preventDefault();
    window.addEventListener("dragenter", lock, true);
    window.addEventListener("dragover", lock, true);
    window.addEventListener("drop", swallow, true);
    return () => {
      document.body.classList.remove("dnd-dragging");
      window.removeEventListener("dragenter", lock, true);
      window.removeEventListener("dragover", lock, true);
      window.removeEventListener("drop", swallow, true);
    };
  }, [payload]);

  if (!payload) return null;
  // Nothing to show on Windows without a target; see the note at the top.
  if (!dest && IS_WINDOWS) return null;

  const n = payload.items.length;
  const noun = t(n === 1 ? "dnd.file" : "dnd.files");

  // The badge is centred on the pointer rather than aligned to either edge:
  // its text changes length as the target changes, and anchoring one edge makes
  // the other one jump.
  const half = 140; // half the widest the badge can get, plus a margin
  const centerX = Math.min(Math.max(cursor.x + 14, half), window.innerWidth - half);
  // Near the bottom of the window the badge goes above the pointer instead of
  // below it, where it used to be clipped.
  const pillHeight = 30;
  const top =
    cursor.y + 16 + pillHeight > window.innerHeight
      ? Math.max(8, cursor.y - pillHeight - 8)
      : cursor.y + 16;

  // With no target the badge itself says the drop would go nowhere, and turns
  // back on its own over a real one — both read from the same value.
  return createPortal(
    <div
      className={
        "pointer-events-none fixed z-[300] flex max-w-[270px] items-center gap-1.5 rounded-full border bg-surface px-2.5 py-[5px] text-[11.5px] shadow-md " +
        (dest
          ? "border-accent-600 text-accent-200"
          : "border-[var(--color-danger)] text-[var(--color-danger)]")
      }
      style={{ left: centerX, top, transform: "translateX(-50%)" }}
    >
      {dest ? (
        <>
          <FilesIcon size={13} className="shrink-0" />
          <span className="truncate text-accent-200">
            {n} {noun}
            <span className="text-accent-300"> → {dest.label}</span>
          </span>
        </>
      ) : (
        <>
          <ProhibitIcon size={13} className="shrink-0" />
          <span className="truncate">{t("dnd.cantDropHere")}</span>
        </>
      )}
    </div>,
    document.body,
  );
}
