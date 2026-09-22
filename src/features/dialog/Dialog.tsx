import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { useDialogStore } from "../../state/dialogStore";
import { useUiStore } from "../../state/uiStore";
import { t } from "../../locale/en";

/* The modal window: a question, something to read, or a name clash.

   It opens centred on the pane it is about, so the answer appears where the
   user is already looking; with no pane to speak of it centres on the window.
   The backdrop always covers everything. */

const MARGIN = 8; // keeps the box off the window edges

export default function Dialog() {
  const dialog = useDialogStore((s) => s.dialog);
  const close = useDialogStore((s) => s.close);
  const activePane = useUiStore((s) => s.activePane);
  const boxRef = useRef<HTMLDivElement>(null);
  const [pos, setPos] = useState<{ left: number; top: number } | null>(null);
  const [applyAll, setApplyAll] = useState(false);
  // Which button the keyboard is on, so a question can be answered without
  // reaching for the mouse.
  const [choice, setChoice] = useState<"cancel" | "confirm">("confirm");
  const choiceRef = useRef(choice);
  choiceRef.current = choice;

  // Dismissing a name clash is itself an answer: cancel the transfer.
  const dismiss = () => {
    if (dialog?.kind === "conflict") dialog.onResolve("cancel", false);
    close();
  };

  useEffect(() => {
    setApplyAll(false);
    setChoice("confirm");
  }, [dialog]);

  useEffect(() => {
    if (!dialog) return;
    // Keys are caught on the way down and stop here, so nothing behind the
    // modal — the panes, the sidebar, the global shortcuts — sees them while it
    // is open.
    const onKey = (e: KeyboardEvent) => {
      e.stopPropagation();
      // Tab moves nothing here; the arrows choose the button.
      if (e.code === "Tab" || e.key === "Tab") {
        e.preventDefault();
        return;
      }
      if (e.key === "Escape") {
        dismiss();
        return;
      }
      if (dialog.kind !== "confirm") return;
      // Left and right switch between the two buttons.
      if (e.key === "ArrowLeft" || e.key === "ArrowRight") {
        e.preventDefault();
        setChoice(e.key === "ArrowLeft" ? "cancel" : "confirm");
        return;
      }
      // Enter presses the chosen one.
      if (e.key === "Enter") {
        e.preventDefault();
        if (choiceRef.current === "confirm") dialog.onConfirm();
        close();
      }
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [dialog]);

  // Positioning is measured before the browser paints, so the box never shows
  // up in the middle of the window and then jumps to its pane.
  const target = dialog?.center ? null : (dialog?.paneIndex ?? activePane);
  useLayoutEffect(() => {
    if (!dialog) {
      setPos(null);
      return;
    }
    const measure = () => {
      const vw = window.innerWidth;
      const vh = window.innerHeight;
      const box = boxRef.current;
      const bw = box?.offsetWidth ?? 360;
      const bh = box?.offsetHeight ?? 0;

      let cx = vw / 2;
      let cy = vh / 2;
      const el =
        target != null
          ? document.querySelector<HTMLElement>(`[data-pane="${target}"]`)
          : null;
      if (el) {
        const r = el.getBoundingClientRect();
        cx = r.left + r.width / 2;
        cy = r.top + r.height / 2;
      }

      const left = Math.round(
        Math.min(Math.max(MARGIN, cx - bw / 2), vw - bw - MARGIN),
      );
      const top = Math.round(
        Math.min(Math.max(MARGIN, cy - bh / 2), vh - bh - MARGIN),
      );
      setPos({ left, top });
    };
    measure();
    window.addEventListener("resize", measure);
    return () => window.removeEventListener("resize", measure);
  }, [dialog, target]);

  if (!dialog) return null;

  const title =
    dialog.kind === "conflict" ? t("dialog.conflictTitle") : dialog.title;

  return createPortal(
    <div
      className="fixed inset-0 z-[150] bg-black/50"
      onMouseDown={dismiss}
    >
      <div
        ref={boxRef}
        className="absolute w-[360px] rounded-lg border border-neutral-700 bg-surface p-5 shadow-lg"
        style={
          pos
            ? { left: pos.left, top: pos.top }
            : { left: "50%", top: "50%", transform: "translate(-50%, -50%)" }
        }
        onMouseDown={(e) => e.stopPropagation()}
      >
        <h4 className="text-[14px] font-medium">{title}</h4>

        {dialog.kind === "conflict" ? (
          <>
            <p className="mt-2 text-[12.5px] leading-relaxed text-neutral-400">
              {t("dialog.conflictBody", {
                name: dialog.name,
                dest: dialog.destLabel,
              })}
            </p>
            {dialog.remaining > 1 && (
              <label className="mt-3 flex cursor-pointer items-center gap-2.5 text-[12px] text-neutral-400 select-none">
                <input
                  type="checkbox"
                  checked={applyAll}
                  onChange={(e) => setApplyAll(e.target.checked)}
                  className="h-4 w-4 shrink-0 cursor-pointer accent-[var(--color-accent)]"
                />
                {t("dialog.conflictApplyAll", { count: dialog.remaining })}
              </label>
            )}
            <div className="mt-5 flex flex-wrap justify-end gap-2.5">
              <button
                type="button"
                className="btn btn-ghost"
                onClick={dismiss}
              >
                {t("dialog.cancel")}
              </button>
              <button
                type="button"
                className="btn btn-ghost"
                onClick={() => {
                  dialog.onResolve("skip", applyAll);
                  close();
                }}
              >
                {t("dialog.conflictSkip")}
              </button>
              <button
                type="button"
                className="btn btn-ghost"
                onClick={() => {
                  dialog.onResolve("rename", applyAll);
                  close();
                }}
              >
                {t("dialog.conflictRename")}
              </button>
              <button
                type="button"
                className="btn btn-primary"
                onClick={() => {
                  dialog.onResolve("overwrite", applyAll);
                  close();
                }}
              >
                {t("dialog.conflictOverwrite")}
              </button>
            </div>
          </>
        ) : dialog.kind === "confirm" ? (
          <>
            <p className="mt-2 text-[12.5px] leading-relaxed text-neutral-400">
              {dialog.body}
            </p>
            <div className="mt-5 flex justify-end gap-2.5">
              <button
                type="button"
                className={`btn btn-choice${choice === "cancel" ? " btn-selected" : ""}`}
                onFocus={() => setChoice("cancel")}
                onClick={close}
              >
                {dialog.cancelLabel ?? t("dialog.cancel")}
              </button>
              <button
                type="button"
                className={`btn btn-choice${choice === "confirm" ? " btn-selected" : ""}`}
                onFocus={() => setChoice("confirm")}
                onClick={() => {
                  dialog.onConfirm();
                  close();
                }}
              >
                {dialog.confirmLabel}
              </button>
            </div>
          </>
        ) : (
          <>
            <div className="mt-3 text-[12.5px] text-neutral-300">
              {dialog.content}
            </div>
            <div className="mt-5 flex justify-end">
              <button
                type="button"
                className="btn btn-ghost"
                onClick={close}
              >
                {t("dialog.close")}
              </button>
            </div>
          </>
        )}
      </div>
    </div>,
    document.body,
  );
}
