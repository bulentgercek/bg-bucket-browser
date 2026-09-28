import {
  memo,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  type MouseEvent as ReactMouseEvent,
  type DragEvent as ReactDragEvent,
  type RefObject,
} from "react";
import {
  ArrowBendLeftUpIcon,
  PlayIcon,
  CaretDownIcon,
  CaretUpIcon,
} from "@phosphor-icons/react";
import {
  childPath,
  type Entry,
  type PaneSide,
  type TabState,
  type ViewSize,
} from "../../state/paneStore";
import { useThumbStore, thumbKey } from "../../state/thumbStore";
import { type SortKey, type SortState } from "../../lib/entries";
import { t } from "../../locale/en";
import { type RowDnd } from "./rowDnd";
import { type RowHandlers, useRowHandlers } from "./rowHandlers";
import { useVirtualRows } from "./useVirtualRows";
import { SELECTION } from "./selectionColors";
import { RenameInput } from "./RenameInput";
import { entryIcon } from "./EntryIcon";
import { StateRow, type StateOpts, bodyStateRow } from "./BodyState";

/* The icon view: one tile per entry, with thumbnails. */

/* Smallest tile width in the icon view. */
const ICON_TRACK: Record<ViewSize, number> = { s: 72, m: 104, l: 148 };
/* The grid's own padding above and below its tiles, in px. */
const GRID_PAD = 10;

/* Which extensions get the image icon while their thumbnail is on its way, or
   when there is none. */
const IMAGE_EXT = new Set([
  "png",
  "jpg",
  "jpeg",
  "webp",
  "gif",
  "bmp",
  "tif",
  "tiff",
]);

function isImage(name: string): boolean {
  const dot = name.lastIndexOf(".");
  return dot >= 0 && IMAGE_EXT.has(name.slice(dot + 1).toLowerCase());
}

/* What follows the pointer when a tile is dragged: a small square of the
   thumbnail rather than the whole tile.

   A tall tile ends up hanging well below the pointer, which reads as dragging
   nothing in particular; a small chip stays at the tip. Rows are short enough
   that the default is fine for them. */
const DRAG_CHIP = 56;

export function iconsDragImage(e: ReactDragEvent, thumbUri: string | undefined) {
  if (!thumbUri) return;
  const chip = document.createElement("div");
  chip.style.cssText =
    `position:fixed;top:-200px;left:-200px;width:${DRAG_CHIP}px;height:${DRAG_CHIP}px;` +
    "border-radius:6px;overflow:hidden;border:1px solid var(--color-accent-600);" +
    "box-shadow:0 4px 12px rgba(0,0,0,.35);background:#101119";
  const img = document.createElement("img");
  img.src = thumbUri;
  img.style.cssText = "width:100%;height:100%;object-fit:cover;display:block";
  chip.appendChild(img);
  document.body.appendChild(chip);
  e.dataTransfer.setDragImage(chip, DRAG_CHIP / 2, DRAG_CHIP / 2); // held in the middle
  setTimeout(() => chip.remove(), 0);
}

export function IconsHeader({
  listing,
  sort,
  onSort,
}: {
  listing: Entry[];
  sort: SortState;
  onSort: (k: SortKey) => void;
}) {
  let files = 0;
  let folders = 0;
  for (const e of listing) {
    if (e.kind === "dir") folders += 1;
    else files += 1;
  }
  // Tiles sort by name only; the arrow shows which way, and points up while
  // some other key is in effect.
  const nameDesc = sort.key === "name" && sort.dir === "desc";
  return (
    <div className="flex items-center justify-between border-y border-neutral-900 px-3 py-[5px]">
      <button
        type="button"
        onClick={() => onSort("name")}
        aria-label={t("panel.sortBy", { col: t("panel.col.name") })}
        className="flex items-center gap-1 text-[9.5px] font-semibold uppercase tracking-[0.08em] text-neutral-600 hover:text-neutral-500"
      >
        {t("panel.col.name")}
        {nameDesc ? (
          <CaretDownIcon size={10} className="text-neutral-600" />
        ) : (
          <CaretUpIcon size={10} className="text-neutral-600" />
        )}
      </button>
      <span className="text-[10.5px] text-neutral-700">
        {t("panel.icons.summary", { files, folders })}
      </span>
    </div>
  );
}

export function IconsBody({
  tab,
  side,
  entries,
  filterActive,
  viewSize,
  cursorIndex,
  scrollRef,
  onItemClick,
  onOpen,
  onContext,
  onRenameCommit,
  onRenameCancel,
  onParent,
  dnd,
  stateOpts,
}: {
  tab: TabState;
  side: PaneSide;
  entries: Entry[];
  filterActive: boolean;
  viewSize: ViewSize;
  /** Which tile the keyboard cursor is on: an entry's index, -1 for `..`, or
      a value matching none. */
  cursorIndex: number;
  /** The pane's scrolling body, which decides which rows of tiles are drawn. */
  scrollRef: RefObject<HTMLDivElement | null>;
  onItemClick: (e: ReactMouseEvent, entry: Entry) => void;
  onOpen: (entry: Entry) => void;
  onContext: (e: ReactMouseEvent, entry: Entry) => void;
  onRenameCommit: (entry: Entry, value: string) => void;
  onRenameCancel: () => void;
  onParent: () => void;
  dnd: RowDnd;
  stateOpts: StateOpts;
}) {
  // As in the details view: one fixed set of handlers for every tile, and a
  // set to answer which tiles are selected.
  const handlers = useRowHandlers({
    onItemClick,
    onOpen,
    onContext,
    onRenameCommit,
    onRenameCancel,
    dnd,
  });
  const selected = useMemo(() => new Set(tab.selection), [tab.selection]);
  // A folder that is simply empty keeps its grid, so the `..` tile stays
  // reachable; everything else replaces the grid entirely.
  const sr = bodyStateRow(tab, filterActive, false, stateOpts);
  // Only the rows of tiles near the screen are drawn, as in the details view.
  // The `..` tile is the first item of the grid.
  const gridRef = useRef<HTMLDivElement>(null);
  const v = useVirtualRows(scrollRef, gridRef, !sr, entries.length + 1, GRID_PAD);
  const { reveal } = v;
  useLayoutEffect(() => {
    if (cursorIndex >= -1) reveal(cursorIndex + 1);
  }, [cursorIndex, reveal]);
  if (sr) return <>{sr}</>;
  // The drawn items, as grid positions: 0 is `..`, n is entry n - 1.
  const from = v.first * v.perRow;
  const to = Math.min(entries.length, (v.last + 1) * v.perRow - 1);
  const firstEntry = Math.max(from, 1) - 1;
  return (
    <>
      <div
        ref={gridRef}
        data-icon-grid
        className="grid gap-2 px-2.5"
        style={{
          gridTemplateColumns: `repeat(auto-fill, minmax(${ICON_TRACK[viewSize]}px, 1fr))`,
          alignContent: "start",
          paddingTop: GRID_PAD + v.padTop,
          paddingBottom: GRID_PAD + v.padBottom,
        }}
      >
        {from === 0 && (
          <ParentTile onOpen={onParent} cursor={cursorIndex === -1} side={side} />
        )}
        {entries.slice(firstEntry, to).map((entry, k) => (
          <IconTile
            key={entry.name}
            entry={entry}
            side={side}
            dirPath={tab.path}
            selected={selected.has(entry.name)}
            cursor={cursorIndex === firstEntry + k}
            renaming={tab.renaming === entry.name}
            dropOn={entry.kind === "dir" && dnd.dropTarget === entry.name}
            handlers={handlers}
          />
        ))}
      </div>
      {tab.status === "ready" && entries.length === 0 && (
        <StateRow text={t(filterActive ? "panel.noMatches" : "panel.empty")} />
      )}
    </>
  );
}

/* The icon a tile shows while it has no preview, filled like the details
   view's, and for a folder in its side's colour. */
function tileIcon(entry: Entry, selected: boolean, side: PaneSide) {
  if (entry.kind === "dir") {
    return entryIcon(
      "folder",
      34,
      side === "remote" ? "text-accent-400" : "text-local-400",
    );
  }
  if (isImage(entry.name)) {
    return entryIcon(
      "image",
      26,
      selected ? SELECTION[side].tileIcon : "text-neutral-600",
    );
  }
  return entryIcon(
    entry.glyph ?? "file",
    30,
    selected ? SELECTION[side].tileIcon : "text-neutral-500",
  );
}

/* One tile. Memoised like a row of the details view: it is drawn again only
   when something it shows changes. */
const IconTile = memo(function IconTile({
  entry,
  side,
  dirPath,
  selected,
  cursor,
  renaming,
  dropOn,
  handlers,
}: {
  entry: Entry;
  side: PaneSide;
  dirPath: string;
  selected: boolean;
  /** Whether the keyboard cursor is on this tile, selected or not. */
  cursor: boolean;
  renaming: boolean;
  /** Whether something dragged is over this tile, which is a folder. */
  dropOn: boolean;
  handlers: RowHandlers;
}) {
  const isVideo = entry.kind === "file" && entry.glyph === "video";
  const wantsThumb =
    entry.kind === "file" && (isImage(entry.name) || isVideo);
  const isDir = entry.kind === "dir";
  const sel = SELECTION[side];

  /* A tile asks for its thumbnail once it is on screen. The box is a fixed
     square either way, so a preview arriving never reflows the grid. */
  const boxRef = useRef<HTMLDivElement>(null);
  const thumbPath = wantsThumb ? childPath(dirPath, entry.name) : "";
  const key = wantsThumb ? thumbKey(side, thumbPath, entry.modified) : "";
  const thumb = useThumbStore((s) => (key ? s.map[key] : undefined));
  const requestThumb = useThumbStore((s) => s.request);
  const releaseThumb = useThumbStore((s) => s.release);
  const mod = entry.modified;

  useEffect(() => {
    if (!wantsThumb) return;
    const el = boxRef.current;
    if (!el) return;
    // Visibility keeps being watched, not just noticed once: scrolling a tile
    // away releases its request, which drops it from the queue if it has not
    // left yet.
    let shown = false;
    const io = new IntersectionObserver(
      (ents) => {
        const now = ents[ents.length - 1]?.isIntersecting ?? false;
        if (now === shown) return;
        shown = now;
        if (now) requestThumb(side, thumbPath, mod);
        else releaseThumb(side, thumbPath, mod);
      },
      { rootMargin: "300px" },
    );
    io.observe(el);
    return () => {
      io.disconnect();
      if (shown) releaseThumb(side, thumbPath, mod);
    };
  }, [wantsThumb, side, thumbPath, mod, requestThumb, releaseThumb]);

  return (
    <button
      type="button"
      data-entry={entry.name}
      data-kind={entry.kind}
      data-cursor={cursor || undefined}
      draggable={!renaming}
      onDragStart={(e) => handlers.dragStart(e, entry)}
      onDragEnd={handlers.dragEnd}
      // Entering counts as moving over, as it does for the pane.
      onDragEnter={isDir ? (e) => handlers.dirDragOver(e, entry) : undefined}
      onDragOver={isDir ? (e) => handlers.dirDragOver(e, entry) : undefined}
      onDrop={isDir ? (e) => handlers.dirDrop(e, entry) : undefined}
      onClick={(e) => handlers.click(e, entry)}
      onDoubleClick={() => handlers.open(entry)}
      onContextMenu={(e) => handlers.context(e, entry)}
      className={
        "flex flex-col items-center gap-1.5 rounded-md px-1.5 py-2 transition-colors duration-150 " +
        (dropOn
          ? "bg-[color-mix(in_srgb,var(--color-accent)_14%,transparent)] outline outline-1 -outline-offset-1 outline-dashed outline-accent-400"
          : selected
            ? // A tile is mostly its picture, which hides the fill a selected
              // row shows; the selection is a thick outline instead, and the
              // cursor on it makes that outline brighter, as in the details view.
              `${sel.fill} outline outline-2 -outline-offset-2 ` +
              (cursor ? sel.tileOutlineCursor : sel.tileOutline)
            : cursor
              ? `hover:bg-neutral-900 outline outline-1 -outline-offset-1 ${sel.cursor}`
              : "hover:bg-neutral-900")
      }
    >
      <div
        ref={boxRef}
        className={
          "relative flex aspect-square w-full items-center justify-center overflow-hidden rounded-sm border " +
          (selected ? `${sel.tileBorder} ` : "border-neutral-800 ") +
          (wantsThumb ? "bg-[#101119]" : "bg-neutral-900")
        }
      >
        {thumb?.status === "ready" ? (
          <>
            <img
              src={thumb.uri}
              alt=""
              draggable={false}
              className="h-full w-full object-cover"
            />
            {selected && (
              // The picture itself is tinted too, so the selection shows
              // even where the outline is out of view.
              <span className={`pointer-events-none absolute inset-0 opacity-25 ${sel.tileTint}`} />
            )}
            {isVideo && (
              <span className="absolute grid h-7 w-7 place-items-center rounded-full bg-black/55 backdrop-blur-[1px]">
                <PlayIcon size={14} weight="fill" className="translate-x-px text-white" />
              </span>
            )}
          </>
        ) : (
          tileIcon(entry, selected, side)
        )}
      </div>
      {renaming ? (
        <span className="flex w-full justify-center text-[11.5px]">
          <RenameInput
            initial={entry.name}
            center
            onCommit={(v) => handlers.renameCommit(entry, v)}
            onCancel={handlers.renameCancel}
          />
        </span>
      ) : (
        <span
          className={
            "w-full truncate rounded-sm px-1 text-center text-[11.5px] " +
            (selected ? `${sel.tileLabel} ${sel.text}` : "text-neutral-300")
          }
        >
          {entry.name}
        </span>
      )}
    </button>
  );
});

function ParentTile({
  onOpen,
  cursor,
  side,
}: {
  onOpen: () => void;
  cursor: boolean;
  side: PaneSide;
}) {
  return (
    /* The `..` tile behaves like a folder, and is not one of the entries. */
    <button
      type="button"
      data-cursor={cursor || undefined}
      onDoubleClick={onOpen}
      className={
        "flex flex-col items-center gap-1.5 rounded-md px-1.5 py-2 hover:bg-neutral-900" +
        (cursor ? ` outline outline-1 -outline-offset-1 ${SELECTION[side].cursor}` : "")
      }
    >
      <div className="flex aspect-square w-full items-center justify-center rounded-sm border border-neutral-800 bg-neutral-900">
        <ArrowBendLeftUpIcon size={24} className="text-neutral-600" />
      </div>
      <span className="w-full truncate text-center text-[11.5px] text-neutral-300">
        ..
      </span>
    </button>
  );
}
