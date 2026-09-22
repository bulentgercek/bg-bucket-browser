import { useEffect, useRef, useState, type ReactNode } from "react";
import {
  ListIcon,
  SquaresFourIcon,
  CheckIcon,
  CaretDownIcon,
} from "@phosphor-icons/react";
import { type ViewMode, type ViewSize } from "../../state/paneStore";
import { t } from "../../locale/en";

/* The view control at the end of the path row: rows or tiles, and how large.
   Its menu is positioned against the window, like the other menus here. */
export function ViewControl({
  view,
  onSetMode,
  onSetSize,
}: {
  view: { viewMode: ViewMode; viewSize: ViewSize };
  onSetMode: (mode: ViewMode) => void;
  onSetSize: (size: ViewSize) => void;
}) {
  const [open, setOpen] = useState(false);
  const [pos, setPos] = useState<{ top: number; left: number } | null>(null);
  const btnRef = useRef<HTMLButtonElement>(null);
  const menuRef = useRef<HTMLDivElement>(null);

  const openMenu = () => {
    const r = btnRef.current?.getBoundingClientRect();
    if (r) setPos({ top: r.bottom + 4, left: Math.max(8, r.right - 186) });
    setOpen(true);
  };

  useEffect(() => {
    if (!open) return;
    const onDoc = (e: MouseEvent) => {
      if (
        !btnRef.current?.contains(e.target as Node) &&
        !menuRef.current?.contains(e.target as Node)
      ) {
        setOpen(false);
      }
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") setOpen(false);
    };
    const onResize = () => setOpen(false);
    document.addEventListener("mousedown", onDoc);
    document.addEventListener("keydown", onKey);
    window.addEventListener("resize", onResize);
    return () => {
      document.removeEventListener("mousedown", onDoc);
      document.removeEventListener("keydown", onKey);
      window.removeEventListener("resize", onResize);
    };
  }, [open]);

  const CurrentIcon = view.viewMode === "icons" ? SquaresFourIcon : ListIcon;

  return (
    <>
      <button
        ref={btnRef}
        type="button"
        onClick={() => (open ? setOpen(false) : openMenu())}
        aria-label={t("view.open")}
        className={
          "flex shrink-0 items-center gap-1 rounded-sm border px-1.5 py-1 " +
          (open
            ? "border-accent-700 bg-accent-900 text-accent-200"
            : "border-neutral-800 text-neutral-400 hover:text-neutral-200")
        }
      >
        <CurrentIcon size={14} />
        <CaretDownIcon size={9} className={open ? "" : "text-neutral-600"} />
      </button>

      {open && pos && (
        <div
          ref={menuRef}
          style={{ top: pos.top, left: pos.left }}
          className="fixed z-50 w-[186px] rounded-md border border-neutral-800 bg-surface p-[5px] shadow-lg"
        >
          <div className="px-[9px] pb-[5px] pt-1 text-[9.5px] font-semibold uppercase tracking-[0.09em] text-neutral-600">
            {t("view.title")}
          </div>

          <ModeRow
            icon={<ListIcon size={14} />}
            label={t("view.details")}
            active={view.viewMode === "details"}
            onClick={() => {
              onSetMode("details");
              setOpen(false);
            }}
          />
          <ModeRow
            icon={<SquaresFourIcon size={14} />}
            label={t("view.icons")}
            active={view.viewMode === "icons"}
            onClick={() => {
              onSetMode("icons");
              setOpen(false);
            }}
          />

          <div className="mx-1.5 my-1 border-t border-neutral-800" />

          <div className="flex items-center justify-between px-[9px] py-[5px]">
            <span className="text-[11.5px] text-neutral-500">
              {t("view.size")}
            </span>
            <div className="flex items-center rounded-sm border border-neutral-800 p-[2px]">
              {(["s", "m", "l"] as ViewSize[]).map((s) => (
                <button
                  key={s}
                  type="button"
                  onClick={() => onSetSize(s)}
                  className={
                    "rounded-[3px] px-1.5 text-[10.5px] uppercase leading-[1.5] " +
                    (view.viewSize === s
                      ? "bg-accent-900 text-accent-200"
                      : "text-neutral-500 hover:text-neutral-300")
                  }
                >
                  {s}
                </button>
              ))}
            </div>
          </div>
        </div>
      )}
    </>
  );
}

function ModeRow({
  icon,
  label,
  active,
  onClick,
}: {
  icon: ReactNode;
  label: string;
  active: boolean;
  onClick: () => void;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      className={
        "flex w-full items-center gap-2 rounded-sm px-[9px] py-[5px] text-[12.5px] " +
        (active
          ? "bg-accent-900 text-accent-100"
          : "text-neutral-300 hover:bg-accent-900")
      }
    >
      <span className={active ? "text-accent-300" : ""}>{icon}</span>
      <span className="flex-1 text-left">{label}</span>
      {active && <CheckIcon size={13} className="text-accent-300" />}
    </button>
  );
}
