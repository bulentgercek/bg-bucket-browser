import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { CloudIcon, DesktopTowerIcon, FolderIcon } from "@phosphor-icons/react";
import { IS_WINDOWS } from "../../lib/platform";
import { t } from "../../locale/en";

/* The path above the list, as one clickable crumb per segment; the last one is
   where the tab currently is. */
export interface Crumb {
  label: string;
  path: string;
}

// A Windows drive root, or a path descending from one. Paths are built with
// forward slashes whatever the platform, so a path from a drive root ends up
// with both separators in it and both have to be accepted here.
const WIN_DRIVE_RE = /^([A-Za-z]:)([\\/]|$)/;

export function buildCrumbs(
  path: string,
  remote: boolean,
  bucket: string,
  homeDir: string,
): Crumb[] {
  // On Windows a `~` path is shown as the real home path instead, so that a
  // pin and the same folder reached by hand read identically — there is no
  // tilde on that platform. Only the display changes: what a pin stores stays
  // as it was, and the backend accepts either form.
  if (!remote && IS_WINDOWS && homeDir && (path === "~" || path.startsWith("~/"))) {
    const rest = path === "~" ? "" : path.slice("~/".length);
    path = rest ? `${homeDir}/${rest}` : homeDir;
  }
  // An absolute local path starts from the filesystem root.
  if (!remote && path.startsWith("/")) {
    const parts = path.split("/").filter((s) => s !== "");
    const crumbs: Crumb[] = [{ label: "/", path: "/" }];
    parts.forEach((seg, i) => {
      crumbs.push({ label: seg, path: "/" + parts.slice(0, i + 1).join("/") });
    });
    return crumbs;
  }
  // A path on a Windows drive: the drive itself is the root crumb.
  const winMatch = !remote && path.match(WIN_DRIVE_RE);
  if (winMatch) {
    const drive = `${winMatch[1]}\\`; // a bare drive letter is still a root
    const rest = path.slice(winMatch[0].length);
    const parts = rest.split(/[\\/]+/).filter((s) => s !== "");
    const crumbs: Crumb[] = [{ label: drive, path: drive }];
    let acc = drive;
    parts.forEach((seg) => {
      acc = /[\\/]$/.test(acc) ? `${acc}${seg}` : `${acc}/${seg}`;
      crumbs.push({ label: seg, path: acc });
    });
    return crumbs;
  }
  const parts = path.split("/").filter((s) => s !== "" && s !== "~");
  // The bucket name is always known here: a connection without one never
  // reaches the list at all.
  const crumbs: Crumb[] = [
    { label: remote ? bucket : "~", path: remote ? "" : "~" },
  ];
  parts.forEach((seg, i) => {
    const sub = parts.slice(0, i + 1).join("/");
    crumbs.push({ label: seg, path: remote ? sub : `~/${sub}` });
  });
  return crumbs;
}

const CRUMB_GAP = 5; // the gap between crumbs
const CHIP_W = 26; // assumed width of the overflow chip when it cannot be measured
const FOLD_SAFETY = 6; // px of slack, so the crumbs kept are never squeezed

/* The path row, which folds rather than truncates.

   A crumb that does not fit whole is folded into the chip on the left; no crumb
   is ever shown half-elided. How many to fold is worked out by measuring a
   hidden full-width copy, which never changes as the visible row does, so the
   two cannot chase each other.

   The current folder is the last thing to give way, and only it can be
   shortened. */
export function Breadcrumb({
  path,
  remote,
  bucket,
  homeDir,
  onNavigate,
}: {
  path: string;
  remote: boolean;
  bucket: string;
  homeDir: string;
  onNavigate: (path: string) => void;
}) {
  const crumbs = buildCrumbs(path, remote, bucket, homeDir);
  const current = crumbs[crumbs.length - 1];
  const middle = crumbs.slice(0, -1); // the root and everything between

  const wrapRef = useRef<HTMLDivElement>(null);
  const cloneRef = useRef<HTMLDivElement>(null);
  const [foldCount, setFoldCount] = useState(0);

  useLayoutEffect(() => {
    const wrap = wrapRef.current;
    const clone = cloneRef.current;
    if (!wrap || !clone) return;

    const compute = () => {
      const avail = wrap.clientWidth;
      const full = clone.offsetWidth; // the row at its natural width
      if (full <= avail) {
        setFoldCount(0);
        return;
      }
      const segs = Array.from(
        clone.querySelectorAll<HTMLElement>("[data-seg]"),
      );
      const chipEl = clone.querySelector<HTMLElement>("[data-chip]");
      const chipW = (chipEl ? chipEl.offsetWidth : CHIP_W) + CRUMB_GAP;
      let removed = 0;
      let f = 0;
      for (; f < segs.length; f++) {
        removed += segs[f].offsetWidth + CRUMB_GAP;
        if (full - removed + chipW <= avail - FOLD_SAFETY) {
          f += 1; // this one folds too
          break;
        }
      }
      setFoldCount(Math.min(f, segs.length));
    };

    compute();
    const ro = new ResizeObserver(compute);
    ro.observe(wrap);
    return () => ro.disconnect();
  }, [path, remote]);

  const folded = middle.slice(0, foldCount);
  const shown = middle.slice(foldCount);

  return (
    <div
      ref={wrapRef}
      title={crumbs.map((c) => c.label).join(" / ")}
      className="relative flex min-w-0 flex-1 flex-nowrap items-center gap-[5px] overflow-hidden text-[12px] text-neutral-400"
    >
      {remote ? (
        <CloudIcon size={13} className="shrink-0 text-accent-400" />
      ) : (
        <DesktopTowerIcon size={13} className="shrink-0 text-local-400" />
      )}

      {folded.length > 0 && (
        <OverflowChip crumbs={folded} remote={remote} onNavigate={onNavigate} />
      )}

      {shown.length > 0 && (
        <div className="flex shrink-0 flex-nowrap items-center gap-[5px]">
          {shown.map((crumb, i) => (
            <span key={crumb.path} className="flex items-center gap-[5px]">
              {(i > 0 || folded.length > 0) && (
                <span className="shrink-0 text-neutral-700">/</span>
              )}
              <button
                type="button"
                onClick={() => onNavigate(crumb.path)}
                className="whitespace-nowrap hover:text-neutral-200"
              >
                {crumb.label}
              </button>
            </span>
          ))}
        </div>
      )}

      {crumbs.length > 1 && <span className="shrink-0 text-neutral-700">/</span>}
      {/* The current folder, the only crumb allowed to be shortened, and only
          once everything above it has already folded. */}
      <span
        className="min-w-0 truncate text-text"
        title={current.label}
      >
        {current.label}
      </span>

      {/* The hidden copy that is measured: the whole path at its natural
          width, out of the layout. */}
      <div
        ref={cloneRef}
        aria-hidden
        className="pointer-events-none invisible absolute left-0 top-0 flex w-max flex-nowrap items-center gap-[5px] whitespace-nowrap text-[12px]"
      >
        {remote ? <CloudIcon size={13} /> : <DesktopTowerIcon size={13} />}
        {middle.map((crumb, i) => (
          <span key={crumb.path} data-seg className="flex items-center gap-[5px]">
            {i > 0 && <span>/</span>}
            <span>{crumb.label}</span>
          </span>
        ))}
        {crumbs.length > 1 && <span>/</span>}
        <span>{current.label}</span>
        {/* Measured with the same styling as the real chip. */}
        <span data-chip className="rounded-[3px] border px-[5px] leading-[1.4]">
          …
        </span>
      </div>
    </div>
  );
}

/* The chip that stands for the folded crumbs, and the menu behind it. The menu
   is positioned against the window rather than inside the path row, which would
   clip it. */
function OverflowChip({
  crumbs,
  remote,
  onNavigate,
}: {
  crumbs: Crumb[];
  remote: boolean;
  onNavigate: (path: string) => void;
}) {
  const [open, setOpen] = useState(false);
  const [pos, setPos] = useState<{ top: number; left: number } | null>(null);
  const btnRef = useRef<HTMLButtonElement>(null);
  const menuRef = useRef<HTMLDivElement>(null);

  const openMenu = () => {
    const r = btnRef.current?.getBoundingClientRect();
    if (r) setPos({ top: r.bottom + 4, left: r.left });
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

  return (
    <>
      <button
        ref={btnRef}
        type="button"
        onClick={() => (open ? setOpen(false) : openMenu())}
        aria-label={t("panel.crumbMore")}
        className={
          "shrink-0 rounded-[3px] border px-[5px] text-[12px] leading-[1.4] " +
          (open
            ? "border-accent-700 text-accent-300"
            : "border-neutral-800 text-neutral-500 hover:text-neutral-300")
        }
      >
        …
      </button>
      {open && pos && (
        <div
          ref={menuRef}
          style={{ top: pos.top, left: pos.left }}
          className="fixed z-50 w-[186px] rounded-md border border-neutral-800 bg-surface p-[5px] shadow-lg"
        >
          {crumbs.map((crumb, i) => (
            <button
              key={crumb.path}
              type="button"
              onClick={() => {
                onNavigate(crumb.path);
                setOpen(false);
              }}
              className="flex w-full items-center gap-2 rounded-sm px-[9px] py-[5px] text-left text-[12.5px] text-neutral-300 hover:bg-accent-900"
            >
              {i === 0 ? (
                remote ? (
                  <CloudIcon size={14} className="shrink-0 text-accent-400" />
                ) : (
                  <DesktopTowerIcon size={14} className="shrink-0 text-local-400" />
                )
              ) : (
                <FolderIcon size={14} className="shrink-0 text-accent-400" />
              )}
              <span className="truncate">{crumb.label}</span>
            </button>
          ))}
        </div>
      )}
    </>
  );
}
