import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { ClipboardIcon, ScissorsIcon, XIcon } from "@phosphor-icons/react";
import {
  useClipboardStore,
  type Clipboard as ClipboardData,
} from "../../state/clipboardStore";
import { useStatusBarPopupStore } from "../../state/statusBarPopupStore";
import { t } from "../../locale/en";

/* The clipboard pill in the status bar and the panel it opens.

   Every new copy flashes the panel open for a moment, so it is visible that the
   app registered it; a panel the user opened deliberately never closes on its
   own.

   The transfers pill is a mirror of this file and the two share nothing but
   which popup is open. */

export default function ClipboardIndicator() {
  const clipboard = useClipboardStore((s) => s.clipboard);
  const osClipboardTag = useClipboardStore((s) => s.osClipboardTag);
  const hasClip = clipboard != null || osClipboardTag != null;
  // A new copy is a change of content, not a change from empty to full: two
  // copies in a row both deserve their flash.
  const clipSignature = clipboard
    ? `inApp:${clipboard.mode}:${clipboard.srcPath}:${clipboard.items.map((e) => e.name).join(",")}`
    : osClipboardTag
      ? `os:${osClipboardTag.mode}:${osClipboardTag.paths.join(",")}`
      : null;

  const [clipOpen, setClipOpen] = useState(false);
  const clipBtnRef = useRef<HTMLButtonElement>(null);
  const [clipPopup, setClipPopup] = useState<
    { left: number; bottom: number; anchor: HTMLElement } | null
  >(null);

  // The one shared value: the other popup opening closes this one.
  const activePopup = useStatusBarPopupStore((s) => s.active);
  const setActivePopup = useStatusBarPopupStore((s) => s.setActive);
  useEffect(() => {
    if (activePopup !== "clipboard" && clipOpen) setClipOpen(false);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [activePopup]);

  // Whether the panel was opened deliberately. Cleared however it closes, so
  // the next copy gets its flash again.
  const pinned = useRef(false);
  const autoCloseTimer = useRef<number | null>(null);
  const clearAutoClose = () => {
    if (autoCloseTimer.current !== null) {
      window.clearTimeout(autoCloseTimer.current);
      autoCloseTimer.current = null;
    }
  };
  const closeAll = () => {
    clearAutoClose();
    pinned.current = false;
    setClipOpen(false);
    setActivePopup(null);
  };
  const toggleClip = () => {
    clearAutoClose(); // a deliberate click cancels the automatic close
    setClipOpen((v) => {
      const next = !v;
      pinned.current = next;
      setActivePopup(next ? "clipboard" : null);
      return next;
    });
  };

  // The flash: a changed signature opens the panel briefly, unless it is
  // already open on purpose, in which case its contents simply update.
  const prevSig = useRef<string | null>(null);
  useEffect(() => {
    if (clipSignature !== null && clipSignature !== prevSig.current && !pinned.current) {
      setClipOpen(true);
      setActivePopup("clipboard");
      clearAutoClose();
      autoCloseTimer.current = window.setTimeout(() => {
        setClipOpen(false);
        pinned.current = false;
        autoCloseTimer.current = null;
        setActivePopup(null);
      }, 1500);
    }
    prevSig.current = clipSignature;
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [clipSignature]);

  // Nothing left to show.
  useEffect(() => {
    if (!hasClip && clipOpen) closeAll();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [hasClip, clipOpen]);

  useLayoutEffect(() => {
    if (!clipOpen) {
      setClipPopup(null);
      return;
    }
    const el = clipBtnRef.current;
    if (!el) return;
    const r = el.getBoundingClientRect();
    const half = 190;
    const left = Math.min(
      window.innerWidth - 8 - half,
      Math.max(8 + half, r.left + r.width / 2),
    );
    setClipPopup({ left, bottom: window.innerHeight - r.top + 8, anchor: el });
  }, [clipOpen]);

  if (!hasClip) return null;

  return (
    <>
      <button
        ref={clipBtnRef}
        type="button"
        onClick={toggleClip}
        className="flex items-center gap-1.5 rounded-full border border-neutral-800 px-2 py-0.5 hover:border-accent-700 hover:text-accent-300"
      >
        <ClipboardIcon size={11} className="shrink-0 text-accent-400" />
        {t("main.clipboard")}
      </button>
      {clipOpen && clipPopup && (
        <ClipboardPanel pos={clipPopup} anchor={clipPopup.anchor} onClose={closeAll} />
      )}
    </>
  );
}

/* The panel above the pill. It can draw a row for each clipboard, though only
   one of them is ever filled. */
function ClipboardPanel({
  pos,
  anchor,
  onClose,
}: {
  pos: { left: number; bottom: number };
  anchor: HTMLElement;
  onClose: () => void;
}) {
  const clipboard = useClipboardStore((s) => s.clipboard);
  const clearClipboard = useClipboardStore((s) => s.clearClipboard);
  const osClipboardTag = useClipboardStore((s) => s.osClipboardTag);
  const clearOsClipboardTag = useClipboardStore((s) => s.clearOsClipboardTag);
  const ref = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const onDown = (e: MouseEvent) => {
      const t = e.target as Node;
      if (!ref.current?.contains(t) && !anchor.contains(t)) onClose();
    };
    const onKey = (e: KeyboardEvent) => e.key === "Escape" && onClose();
    window.addEventListener("mousedown", onDown, true);
    window.addEventListener("keydown", onKey);
    return () => {
      window.removeEventListener("mousedown", onDown, true);
      window.removeEventListener("keydown", onKey);
    };
  }, [anchor, onClose]);

  return createPortal(
    <div
      ref={ref}
      style={{ left: pos.left, bottom: pos.bottom }}
      className="fixed z-[100] flex max-h-[46vh] w-[320px] flex-col rounded-md border border-neutral-800 bg-surface shadow-lg -translate-x-1/2"
    >
      <div className="flex items-center justify-between border-b border-neutral-800 px-3 py-2 text-[11px] font-medium tracking-wide text-neutral-500">
        <span>{t("main.clipboard")}</span>
      </div>
      <div className="min-h-0 flex-1 overflow-y-auto py-1">
        {clipboard && <PendingRow cb={clipboard} onClear={clearClipboard} />}
        {osClipboardTag && (
          <OsPendingRow tag={osClipboardTag} onClear={clearOsClipboardTag} />
        )}
        {!clipboard && !osClipboardTag && (
          <p className="px-3 py-4 text-center text-[11.5px] text-neutral-600">
            {t("queue.empty")}
          </p>
        )}
      </div>
    </div>,
    document.body,
  );
}

/* The in-app clipboard as a row: what is waiting, and a way to drop it. Not a
   transfer — nothing has been queued yet. */
function PendingRow({ cb, onClear }: { cb: ClipboardData; onClear: () => void }) {
  const Icon = cb.mode === "move" ? ScissorsIcon : ClipboardIcon;
  const label =
    cb.items.length === 1
      ? cb.items[0].name
      : t("queue.clipItems", { count: cb.items.length });
  return (
    <div className="flex items-center gap-2 px-3 py-1.5 text-[11.5px]">
      <Icon size={12} className="shrink-0 text-neutral-500" />
      <div className="flex min-w-0 flex-1 items-center justify-between gap-2">
        <span className="min-w-0 truncate text-neutral-300">
          <span className="text-neutral-500">
            {t(cb.mode === "move" ? "queue.clipMove" : "queue.clipCopy")} ·{" "}
          </span>
          {label}
        </span>
        <span className="shrink-0 text-neutral-600">{t("queue.pending")}</span>
      </div>
      <button
        type="button"
        onClick={onClear}
        aria-label={t("queue.clipClear")}
        className="shrink-0 text-neutral-600 hover:text-neutral-300"
      >
        <XIcon size={11} />
      </button>
    </div>
  );
}

/* The same row for what the OS clipboard holds. Dismissing it hides the row
   without touching the real clipboard, and the same content stays hidden until
   something else is copied. */
function OsPendingRow({
  tag,
  onClear,
}: {
  tag: { paths: string[]; mode: "copy" | "move" };
  onClear: () => void;
}) {
  const Icon = tag.mode === "move" ? ScissorsIcon : ClipboardIcon;
  const label =
    tag.paths.length === 1
      ? (tag.paths[0].split("/").pop() ?? tag.paths[0])
      : t("queue.clipItems", { count: tag.paths.length });
  return (
    <div className="flex items-center gap-2 px-3 py-1.5 text-[11.5px]">
      <Icon size={12} className="shrink-0 text-neutral-500" />
      <div className="flex min-w-0 flex-1 items-center justify-between gap-2">
        <span className="min-w-0 truncate text-neutral-300">
          <span className="text-neutral-500">
            {t(tag.mode === "move" ? "queue.clipMove" : "queue.clipCopy")} ·{" "}
          </span>
          {label}
        </span>
        <span className="shrink-0 text-neutral-600">{t("queue.pending")}</span>
      </div>
      <button
        type="button"
        onClick={onClear}
        aria-label={t("queue.clipClear")}
        className="shrink-0 text-neutral-600 hover:text-neutral-300"
      >
        <XIcon size={11} />
      </button>
    </div>
  );
}
