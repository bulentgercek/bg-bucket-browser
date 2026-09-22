import {
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
  type MouseEvent as ReactMouseEvent,
} from "react";
import {
  CloudIcon,
  DesktopTowerIcon,
  PlusIcon,
  XIcon,
} from "@phosphor-icons/react";
import { type PaneState, type TabState } from "../../state/paneStore";
import { IS_WINDOWS } from "../../lib/platform";
import { t } from "../../locale/en";

/* A pane's tabs, folding into a menu when they do not fit. */

const TAB_GAP = 1; // the gap between tabs
const TAB_CHIP_W = 24; // assumed width of the overflow chip when it cannot be measured

/* What a tab is called: the last part of its path, or the name of the root it
   is sitting on. */
function tabLabel(
  tab: TabState,
  remote: boolean,
  bucket: string,
  homeDir: string,
): string {
  const trimmed = tab.path === "/" ? "/" : tab.path.replace(/\/+$/, "");
  if (trimmed === "" || trimmed === "~") {
    if (remote) return bucket;
    // A tilde means nothing on Windows, so the home directory's own folder
    // name is used there; a tab label has no room for the full path.
    if (IS_WINDOWS && homeDir) {
      const seg = homeDir.split(/[\\/]/).filter(Boolean).pop();
      if (seg) return seg;
    }
    return "~";
  }
  if (trimmed === "/") return "/";
  // Both separators are accepted: a path that came from a Windows drive uses
  // the other one, and on Linux that character simply never appears here.
  return trimmed.split(/[\\/]/).filter(Boolean).pop() ?? trimmed;
}

/* The row of tabs, which folds when they do not fit.

   A hidden copy of the strip is measured at full width, and from that a window
   of tabs around the active one is chosen; whatever falls outside it collapses
   into a chip on that side. The active tab is never folded away, and the
   overflow can happen on either side of it because the active tab can be
   anywhere.

   The same measure-and-fold idea as the path above the list, with two chips
   instead of one. */
export function TabStrip({
  pane,
  remote,
  bucket,
  homeDir,
  onSelectTab,
  onNewTab,
  onCloseTab,
}: {
  pane: PaneState;
  remote: boolean;
  bucket: string;
  homeDir: string;
  onSelectTab: (id: string) => void;
  onNewTab: (e: ReactMouseEvent) => void;
  onCloseTab: (id: string) => void;
}) {
  const canClose = pane.tabs.length > 1;
  const wrapRef = useRef<HTMLDivElement>(null);
  const cloneRef = useRef<HTMLDivElement>(null);
  const activeRef = useRef<HTMLDivElement>(null);
  const n = pane.tabs.length;
  const [win, setWin] = useState<{ lo: number; hi: number }>(() => ({
    lo: 0,
    hi: Math.max(0, n - 1),
  }));

  const activeIndex = pane.tabs.findIndex((tb) => tb.id === pane.activeTabId);

  useLayoutEffect(() => {
    const wrap = wrapRef.current;
    const clone = cloneRef.current;
    if (!wrap || !clone) return;

    const compute = () => {
      const avail = wrap.clientWidth;
      const tabEls = Array.from(
        clone.querySelectorAll<HTMLElement>("[data-tab]"),
      );
      if (tabEls.length !== n) return;
      const w = tabEls.map((el) => el.offsetWidth + TAB_GAP);
      const total = w.reduce((a, b) => a + b, 0);
      if (total <= avail) {
        setWin({ lo: 0, hi: n - 1 });
        return;
      }
      const chipEl = clone.querySelector<HTMLElement>("[data-chip]");
      const chipW = (chipEl ? chipEl.offsetWidth : TAB_CHIP_W) + TAB_GAP;

      // Grow outwards from the active tab, alternating sides, counting the
      // room a chip would still need on whichever side is not exhausted.
      const ai = activeIndex < 0 ? 0 : activeIndex;
      let lo = ai;
      let hi = ai;
      let used = w[ai];
      let canL = lo > 0;
      let canR = hi < n - 1;
      let side: "left" | "right" = "right";
      const fits = (nlo: number, nhi: number, nused: number) =>
        nused + (nlo > 0 ? chipW : 0) + (nhi < n - 1 ? chipW : 0) <= avail;

      while (canL || canR) {
        if (side === "right" && !canR) side = "left";
        else if (side === "left" && !canL) side = "right";

        if (side === "right") {
          if (canR && fits(lo, hi + 1, used + w[hi + 1])) {
            used += w[hi + 1];
            hi += 1;
          } else {
            canR = false;
          }
          if (hi >= n - 1) canR = false;
          side = "left";
        } else {
          if (canL && fits(lo - 1, hi, used + w[lo - 1])) {
            used += w[lo - 1];
            lo -= 1;
          } else {
            canL = false;
          }
          if (lo <= 0) canL = false;
          side = "right";
        }
      }
      setWin({ lo, hi });
    };

    compute();
    const ro = new ResizeObserver(compute);
    ro.observe(wrap);
    return () => ro.disconnect();
  }, [n, activeIndex]);

  // If even the active tab does not fit, scroll it into view.
  useLayoutEffect(() => {
    activeRef.current?.scrollIntoView({ block: "nearest", inline: "nearest" });
  }, [pane.activeTabId, n, win.lo, win.hi]);

  const lo = Math.min(win.lo, Math.max(0, n - 1));
  const hi = Math.min(win.hi, n - 1);
  const leftFolded = pane.tabs.slice(0, lo);
  const rightFolded = pane.tabs.slice(hi + 1);
  const visible = pane.tabs.slice(lo, hi + 1);

  const renderTab = (tb: TabState, forClone: boolean) => {
    const active = tb.id === pane.activeTabId;
    return (
      <div
        key={tb.id}
        ref={!forClone && active ? activeRef : undefined}
        data-tab={forClone ? true : undefined}
        onClick={forClone ? undefined : () => onSelectTab(tb.id)}
        /* The active tab's bottom edge is painted in the body's own colour, so
           the two read as one surface. */
        style={
          !forClone && active
            ? { borderBottomColor: "var(--color-bg)" }
            : undefined
        }
        className={
          "flex max-w-[160px] shrink-0 items-center gap-1.5 px-[11px] py-[5px] text-[12px] " +
          (forClone
            ? "border border-transparent"
            : active
              ? "rounded-t-sm border border-neutral-800 bg-surface text-text"
              : "border border-transparent text-neutral-500 hover:text-neutral-300")
        }
      >
        {remote ? (
          <CloudIcon
            size={13}
            className={"shrink-0 " + (active ? "text-accent-400" : "")}
          />
        ) : (
          <DesktopTowerIcon
            size={13}
            className={"shrink-0 " + (active ? "text-local-400" : "")}
          />
        )}
        <span className="min-w-0 truncate">{tabLabel(tb, remote, bucket, homeDir)}</span>
        {active &&
          (forClone ? (
            <XIcon size={11} className="shrink-0" />
          ) : (
            <button
              type="button"
              disabled={!canClose}
              onClick={(e) => {
                e.stopPropagation(); // closing a tab must not also select it
                onCloseTab(tb.id);
              }}
              aria-label={t("main.tab.close")}
              className={
                "shrink-0 rounded-[3px] " +
                (canClose
                  ? "text-neutral-600 hover:text-neutral-300"
                  : "text-neutral-800")
              }
            >
              <XIcon size={11} />
            </button>
          ))}
      </div>
    );
  };

  return (
    <div className="flex items-stretch border-b border-neutral-900 px-2.5 pt-[7px]">
      <div
        ref={wrapRef}
        className="relative flex min-w-0 flex-1 items-stretch gap-px overflow-hidden"
      >
        {leftFolded.length > 0 && (
          <TabOverflowMenu
            tabs={leftFolded}
            remote={remote}
            bucket={bucket}
            homeDir={homeDir}
            align="left"
            onSelectTab={onSelectTab}
          />
        )}
        {visible.map((tb) => renderTab(tb, false))}
        {rightFolded.length > 0 && (
          <TabOverflowMenu
            tabs={rightFolded}
            remote={remote}
            bucket={bucket}
            homeDir={homeDir}
            align="right"
            onSelectTab={onSelectTab}
          />
        )}

        {/* The hidden copy that is measured: every tab at its natural width,
            kept from affecting the layout by the wrapper around it. */}
        <div
          ref={cloneRef}
          aria-hidden
          className="pointer-events-none invisible absolute left-0 top-0 flex w-max flex-nowrap items-stretch gap-px"
        >
          {pane.tabs.map((tb) => renderTab(tb, true))}
          <span
            data-chip
            className="rounded-[3px] border px-[5px] py-[3px] text-[12px] leading-none"
          >
            …
          </span>
        </div>
      </div>

      {/* Which side this pane is, large enough to see at a glance. */}
      <span className="shrink-0 self-center pl-2 pr-1.5">
        {remote ? (
          <CloudIcon size={22} className="text-accent-300" />
        ) : (
          <DesktopTowerIcon size={22} className="text-local-300" />
        )}
      </span>

      <button
        type="button"
        onClick={onNewTab}
        aria-label={t("main.tab.new")}
        /* Brighter than the design calls for, so it is actually noticed. */
        className="shrink-0 self-center rounded-sm px-2 py-[5px] text-neutral-200 hover:text-neutral-100"
      >
        <PlusIcon size={12} weight="bold" />
      </button>
    </div>
  );
}

/* The chip that stands for the folded tabs, and the menu behind it. Choosing
   one makes it active, which unfolds it back into the strip. */
function TabOverflowMenu({
  tabs,
  remote,
  bucket,
  homeDir,
  align = "left",
  onSelectTab,
}: {
  tabs: TabState[];
  remote: boolean;
  bucket: string;
  homeDir: string;
  align?: "left" | "right";
  onSelectTab: (id: string) => void;
}) {
  const [open, setOpen] = useState(false);
  const [pos, setPos] = useState<{ top: number; left: number } | null>(null);
  const btnRef = useRef<HTMLButtonElement>(null);
  const menuRef = useRef<HTMLDivElement>(null);
  const MENU_W = 200;

  const openMenu = () => {
    const r = btnRef.current?.getBoundingClientRect();
    if (r) {
      const left =
        align === "right" ? Math.max(8, r.right - MENU_W) : r.left;
      setPos({ top: r.bottom + 4, left });
    }
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
        aria-label={t("main.tab.overflow")}
        className={
          "mr-px shrink-0 self-center rounded-[3px] border px-[5px] py-[3px] text-[12px] leading-none " +
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
          className="fixed z-50 max-h-[60vh] w-[200px] overflow-y-auto rounded-md border border-neutral-800 bg-surface p-[5px] shadow-lg"
        >
          {tabs.map((tb) => (
            <button
              key={tb.id}
              type="button"
              onClick={() => {
                onSelectTab(tb.id);
                setOpen(false);
              }}
              className="flex w-full items-center gap-2 rounded-sm px-[9px] py-[5px] text-left text-[12.5px] text-neutral-300 hover:bg-accent-900"
            >
              {remote ? (
                <CloudIcon size={13} className="shrink-0 text-accent-400" />
              ) : (
                <DesktopTowerIcon size={13} className="shrink-0 text-local-400" />
              )}
              <span className="min-w-0 flex-1 truncate">
                {tabLabel(tb, remote, bucket, homeDir)}
              </span>
            </button>
          ))}
        </div>
      )}
    </>
  );
}
