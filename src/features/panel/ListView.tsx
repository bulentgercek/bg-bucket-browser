import {
  memo,
  useLayoutEffect,
  useMemo,
  useRef,
  type MouseEvent as ReactMouseEvent,
  type RefObject,
} from "react";
import {
  ArrowBendLeftUpIcon,
  CaretDownIcon,
  CaretUpIcon,
} from "@phosphor-icons/react";
import { type Entry, type PaneSide, type TabState } from "../../state/paneStore";
import { formatDate, formatSize } from "../../lib/format";
import { type SortKey, type SortState } from "../../lib/entries";
import { t } from "../../locale/en";
import { type RowDnd } from "./rowDnd";
import { type RowHandlers, useRowHandlers } from "./rowHandlers";
import { useVirtualRows } from "./useVirtualRows";
import { SELECTION } from "./selectionColors";
import { RenameInput } from "./RenameInput";
import { entryIcon } from "./EntryIcon";
import { type StateOpts, bodyStateRow } from "./BodyState";

/* The details view: a header row and one row per entry. */

/* The list itself, or whatever stands in for it: loading, a failure, or an
   empty folder. The `..` row is drawn above this and is not part of it. */
export function ListBody({
  tab,
  side,
  entries,
  filterActive,
  gridClass,
  rowClass,
  showModified,
  cursorIndex,
  scrollRef,
  onItemClick,
  onOpen,
  onContext,
  onRenameCommit,
  onRenameCancel,
  dnd,
  stateOpts,
}: {
  tab: TabState;
  /** Which side the pane is; its folders are drawn in that side's colour. */
  side: PaneSide;
  /** The entries as the tab shows them: filtered and sorted. */
  entries: Entry[];
  filterActive: boolean;
  gridClass: string;
  rowClass: string;
  showModified: boolean;
  /** Which visible row the keyboard cursor is on, or a value matching none. */
  cursorIndex: number;
  /** The pane's scrolling body, which decides which rows are drawn. */
  scrollRef: RefObject<HTMLDivElement | null>;
  onItemClick: (e: ReactMouseEvent, entry: Entry) => void;
  onOpen: (entry: Entry) => void;
  onContext: (e: ReactMouseEvent, entry: Entry) => void;
  onRenameCommit: (entry: Entry, value: string) => void;
  onRenameCancel: () => void;
  dnd: RowDnd;
  stateOpts: StateOpts;
}) {
  const handlers = useRowHandlers({
    onItemClick,
    onOpen,
    onContext,
    onRenameCommit,
    onRenameCancel,
    dnd,
  });
  // Every row asks whether it is selected on every draw; a set answers that
  // in constant time however much is selected.
  const selected = useMemo(() => new Set(tab.selection), [tab.selection]);
  const sr = bodyStateRow(tab, filterActive, entries.length === 0, stateOpts);
  const listRef = useRef<HTMLDivElement>(null);
  const v = useVirtualRows(scrollRef, listRef, !sr, entries.length, 0);
  // A cursor moved to a row that is not drawn yet is brought on screen here;
  // the pane's own scrolling can only reach rows that are on the page.
  const { reveal } = v;
  useLayoutEffect(() => {
    if (cursorIndex >= 0) reveal(cursorIndex);
  }, [cursorIndex, reveal]);
  if (sr) return <>{sr}</>;
  return (
    <div
      ref={listRef}
      className="flex flex-col gap-px"
      style={{ paddingTop: v.padTop, paddingBottom: v.padBottom }}
    >
      {entries.slice(v.first, v.last + 1).map((entry, k) => (
        <FileRow
          key={entry.name}
          entry={entry}
          side={side}
          gridClass={gridClass}
          rowClass={rowClass}
          showModified={showModified}
          selected={selected.has(entry.name)}
          cursor={cursorIndex === v.first + k}
          renaming={tab.renaming === entry.name}
          dropOn={entry.kind === "dir" && dnd.dropTarget === entry.name}
          handlers={handlers}
        />
      ))}
    </div>
  );
}

/* A column heading that sorts. In a right-aligned column the arrow goes on the
   left of the label, so the numbers below stay flush to the edge. */
export function ColHeader({
  label,
  sortKey,
  sort,
  align,
  onSort,
}: {
  label: string;
  sortKey: SortKey;
  sort: SortState;
  align?: "right";
  onSort: (k: SortKey) => void;
}) {
  const active = sort.key === sortKey;
  const caret = active ? (
    sort.dir === "asc" ? <CaretUpIcon size={9} /> : <CaretDownIcon size={9} />
  ) : null;
  return (
    <button
      type="button"
      onClick={() => onSort(sortKey)}
      aria-label={t("panel.sortBy", { col: label })}
      className={
        "flex items-center gap-1 " +
        (align === "right" ? "justify-end " : "") +
        (active ? "text-neutral-400" : "hover:text-neutral-500")
      }
    >
      {align === "right" && caret}
      <span>{label}</span>
      {align !== "right" && caret}
    </button>
  );
}

export function ParentRow({
  side,
  gridClass,
  rowClass,
  showModified,
  cursor,
  onOpen,
}: {
  /** Which side the pane is; the cursor is drawn in its colour. */
  side: PaneSide;
  gridClass: string;
  rowClass: string;
  showModified: boolean;
  /** Whether the keyboard cursor is on this row. */
  cursor?: boolean;
  onOpen: () => void;
}) {
  return (
    /* The `..` row behaves like a folder: a double click goes up. A single
       click does nothing, and it never joins the selection — it is not one of
       the entries. */
    <div
      data-cursor={cursor || undefined}
      onDoubleClick={onOpen}
      className={
        `${gridClass} items-center rounded-sm px-2 ${rowClass} ` +
        (cursor
          ? `outline outline-1 -outline-offset-1 ${SELECTION[side].cursor} hover:bg-neutral-900`
          : "hover:bg-neutral-900")
      }
    >
      <span className="flex items-center gap-2">
        <ArrowBendLeftUpIcon size={14} className="text-neutral-600" />
        ..
      </span>
      <span />
      {showModified && <span />}
    </div>
  );
}

function fileGlyph(entry: Entry, className: string) {
  return entryIcon(entry.glyph ?? "file", 15, className);
}

/* One row. Memoised: a row is drawn again only when something it shows
   changes, so hovering a pane or selecting a row redraws a handful of rows
   rather than all of them. */
const FileRow = memo(function FileRow({
  entry,
  side,
  gridClass,
  rowClass,
  showModified,
  selected,
  cursor,
  renaming,
  dropOn,
  handlers,
}: {
  entry: Entry;
  side: PaneSide;
  gridClass: string;
  rowClass: string;
  showModified: boolean;
  selected: boolean;
  /** Whether the keyboard cursor is on this row, selected or not. */
  cursor: boolean;
  renaming: boolean;
  /** Whether something dragged is over this row, which is a folder. */
  dropOn: boolean;
  handlers: RowHandlers;
}) {
  const isDir = entry.kind === "dir";
  // Folders take their side's colour, like the pane's frame and its tab and
  // path icons, so the list itself says which side it is on. A selected row
  // keeps the selection's colour throughout.
  const sel = SELECTION[side];
  const iconColor = selected
    ? sel.detail
    : isDir
      ? side === "remote"
        ? "text-accent-400"
        : "text-local-400"
      : "text-neutral-500";
  const metaColor = selected ? sel.detail : "text-neutral-500";

  return (
    <div
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
        `${gridClass} items-center rounded-sm px-2 transition-colors duration-150 ${rowClass} ` +
        (dropOn
          ? `${sel.dropFill} outline outline-1 -outline-offset-1 outline-dashed ${sel.dropOutline}`
          : cursor
            ? // The cursor outline is brighter than the selection outline, so
              // it stays visible on a row that is also selected.
              (selected ? `${sel.fill} ${sel.text} ` : "hover:bg-neutral-900 ") +
              `outline outline-1 -outline-offset-1 ${sel.cursor}`
            : selected
              ? `${sel.fill} ${sel.text} outline outline-1 -outline-offset-1 ${sel.rowOutline}`
              : "hover:bg-neutral-900")
      }
    >
      <span className="flex min-w-0 items-center gap-2">
        {isDir ? (
          entryIcon(selected ? "folderOpen" : "folder", 15, iconColor)
        ) : (
          <span className="shrink-0">{fileGlyph(entry, iconColor)}</span>
        )}
        {renaming ? (
          <RenameInput
            initial={entry.name}
            onCommit={(v) => handlers.renameCommit(entry, v)}
            onCancel={handlers.renameCancel}
          />
        ) : (
          <span className="truncate">{entry.name}</span>
        )}
      </span>
      <span className={`text-right tabular-nums ${metaColor}`}>
        {formatSize(entry.size)}
      </span>
      {showModified && (
        <span className={`tabular-nums ${metaColor}`}>
          {formatDate(entry.modified)}
        </span>
      )}
    </div>
  );
});

/* ── The tile view. Selecting, opening, renaming and dragging all behave as
   they do in the list; only the layout is different. ── */
