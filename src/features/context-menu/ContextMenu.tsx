import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { useContextMenu } from "../../state/contextMenuStore";

/* The single context menu, mounted once at the root of the app.

   It opens at the pointer, or hangs leftwards from it when asked to, and is
   pushed back inside the window when it would not fit. Anything that moves
   what is underneath it — a click elsewhere, scrolling, resizing, the window
   losing focus — closes it. A press on the button that opened it is left to
   that button, which toggles it. */
export default function ContextMenu() {
  const open = useContextMenu((s) => s.open);
  const x = useContextMenu((s) => s.x);
  const y = useContextMenu((s) => s.y);
  const alignRight = useContextMenu((s) => s.alignRight);
  const anchor = useContextMenu((s) => s.anchor);
  const items = useContextMenu((s) => s.items);
  const closeMenu = useContextMenu((s) => s.closeMenu);

  const ref = useRef<HTMLDivElement>(null);
  const [pos, setPos] = useState({ left: x, top: y });

  useLayoutEffect(() => {
    if (!open) return;
    const el = ref.current;
    const w = el?.offsetWidth ?? 212;
    const h = el?.offsetHeight ?? 0;
    const pad = 8;
    const left = alignRight ? x - w : x;
    setPos({
      left: Math.max(pad, Math.min(left, window.innerWidth - w - pad)),
      top: Math.max(pad, Math.min(y, window.innerHeight - h - pad)),
    });
  }, [open, x, y, alignRight, items]);

  useEffect(() => {
    if (!open) return;
    const onDown = (e: MouseEvent) => {
      const target = e.target as Node;
      if (ref.current?.contains(target) || anchor?.contains(target)) return;
      closeMenu();
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") closeMenu();
    };
    const onScroll = () => closeMenu();
    document.addEventListener("mousedown", onDown, true);
    document.addEventListener("keydown", onKey);
    window.addEventListener("scroll", onScroll, true);
    window.addEventListener("resize", closeMenu);
    window.addEventListener("blur", closeMenu);
    return () => {
      document.removeEventListener("mousedown", onDown, true);
      document.removeEventListener("keydown", onKey);
      window.removeEventListener("scroll", onScroll, true);
      window.removeEventListener("resize", closeMenu);
      window.removeEventListener("blur", closeMenu);
    };
  }, [open, anchor, closeMenu]);

  if (!open) return null;

  return createPortal(
    <div
      ref={ref}
      style={{ left: pos.left, top: pos.top }}
      className="fixed z-[100] w-[212px] rounded-md border border-neutral-800 bg-surface p-[5px] shadow-lg"
      role="menu"
    >
      {items.map((it) =>
        it.separator ? (
          <div
            key={it.id}
            className="mx-1.5 my-1 border-t border-neutral-800"
          />
        ) : (
          <button
            key={it.id}
            type="button"
            role="menuitem"
            disabled={it.disabled}
            onClick={() => {
              if (it.disabled) return;
              it.onSelect?.();
              closeMenu();
            }}
            className={
              "flex w-full items-center gap-2 rounded-sm px-[9px] py-[5px] text-left text-[12.5px] " +
              (it.disabled
                ? "cursor-default text-neutral-600 opacity-50"
                : (it.danger ? "text-neutral-300" : "text-neutral-200") +
                  " hover:bg-accent-900")
            }
          >
            {it.icon && (
              <span
                className={
                  it.disabled
                    ? ""
                    : it.danger
                      ? "text-neutral-400"
                      : "text-accent-400"
                }
              >
                {it.icon}
              </span>
            )}
            <span className="flex-1">{it.label}</span>
            {it.shortcut && (
              <span className="text-[10.5px] text-neutral-600">
                {it.shortcut}
              </span>
            )}
          </button>
        ),
      )}
    </div>,
    document.body,
  );
}
