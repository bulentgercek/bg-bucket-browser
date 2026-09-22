import { type MouseEvent as ReactMouseEvent } from "react";
import {
  ArrowBendLeftUpIcon,
  FolderIcon,
  FolderOpenIcon,
  FileIcon,
  FileTextIcon,
  FileZipIcon,
  FilmStripIcon,
  CaretDownIcon,
  CaretUpIcon,
} from "@phosphor-icons/react";
import { type Entry, type TabState } from "../../state/paneStore";
import { formatDate, formatSize } from "../../lib/format";
import { type SortKey, type SortState } from "../../lib/entries";
import { t } from "../../locale/en";
import { type RowDnd } from "./rowDnd";
import { RenameInput } from "./RenameInput";
import { type StateOpts, bodyStateRow } from "./BodyState";

/* The details view: a header row and one row per entry. */

/* The list itself, or whatever stands in for it: loading, a failure, or an
   empty folder. The `..` row is drawn above this and is not part of it. */
export function ListBody({
  tab,
  entries,
  filterActive,
  gridClass,
  rowClass,
  showModified,
  cursorIndex,
  onItemClick,
  onOpen,
  onContext,
  onRenameCommit,
  onRenameCancel,
  dnd,
  stateOpts,
}: {
  tab: TabState;
  /** The entries as the tab shows them: filtered and sorted. */
  entries: Entry[];
  filterActive: boolean;
  gridClass: string;
  rowClass: string;
  showModified: boolean;
  /** Which visible row the keyboard cursor is on, or a value matching none. */
  cursorIndex: number;
  onItemClick: (e: ReactMouseEvent, entry: Entry) => void;
  onOpen: (entry: Entry) => void;
  onContext: (e: ReactMouseEvent, entry: Entry) => void;
  onRenameCommit: (entry: Entry, value: string) => void;
  onRenameCancel: () => void;
  dnd: RowDnd;
  stateOpts: StateOpts;
}) {
  const sr = bodyStateRow(tab, filterActive, entries.length === 0, stateOpts);
  if (sr) return <>{sr}</>;
  return (
    <>
      {entries.map((entry, i) => (
        <FileRow
          key={entry.name}
          entry={entry}
          gridClass={gridClass}
          rowClass={rowClass}
          showModified={showModified}
          selected={tab.selection.includes(entry.name)}
          cursor={cursorIndex === i}
          renaming={tab.renaming === entry.name}
          onClick={(e) => onItemClick(e, entry)}
          onOpen={() => onOpen(entry)}
          onContext={(e) => onContext(e, entry)}
          onRenameCommit={(v) => onRenameCommit(entry, v)}
          onRenameCancel={onRenameCancel}
          dnd={dnd}
        />
      ))}
    </>
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
  gridClass,
  rowClass,
  showModified,
  cursor,
  onOpen,
}: {
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
          ? "outline outline-1 -outline-offset-1 outline-accent-400 hover:bg-neutral-900"
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
  if (entry.glyph === "text") return <FileTextIcon size={15} className={className} />;
  if (entry.glyph === "zip") return <FileZipIcon size={15} className={className} />;
  if (entry.glyph === "video") return <FilmStripIcon size={15} className={className} />;
  return <FileIcon size={15} className={className} />;
}

function FileRow({
  entry,
  gridClass,
  rowClass,
  showModified,
  selected,
  cursor,
  renaming,
  onClick,
  onOpen,
  onContext,
  onRenameCommit,
  onRenameCancel,
  dnd,
}: {
  entry: Entry;
  gridClass: string;
  rowClass: string;
  showModified: boolean;
  selected: boolean;
  /** Whether the keyboard cursor is on this row, selected or not. */
  cursor: boolean;
  renaming: boolean;
  onClick: (e: ReactMouseEvent) => void;
  onOpen: () => void;
  onContext: (e: ReactMouseEvent) => void;
  onRenameCommit: (value: string) => void;
  onRenameCancel: () => void;
  dnd: RowDnd;
}) {
  const isDir = entry.kind === "dir";
  const dropOn = isDir && dnd.dropTarget === entry.name;
  const iconColor = selected
    ? "text-accent-300"
    : isDir
      ? "text-accent-400"
      : "text-neutral-500";
  const metaColor = selected ? "text-accent-300" : "text-neutral-500";

  return (
    <div
      data-entry={entry.name}
      data-kind={entry.kind}
      data-cursor={cursor || undefined}
      draggable={!renaming}
      onDragStart={(e) => dnd.onDragStart(e, entry)}
      onDragEnd={dnd.onDragEnd}
      onDragOver={isDir ? (e) => dnd.onDirDragOver(e, entry) : undefined}
      onDrop={isDir ? (e) => dnd.onDirDrop(e, entry) : undefined}
      onClick={onClick}
      onDoubleClick={onOpen}
      onContextMenu={onContext}
      className={
        `${gridClass} items-center rounded-sm px-2 transition-colors duration-150 ${rowClass} ` +
        (dropOn
          ? "bg-[color-mix(in_srgb,var(--color-accent)_14%,transparent)] outline outline-1 -outline-offset-1 outline-dashed outline-accent-400"
          : cursor
            ? // The cursor outline is brighter than the selection outline, so
              // it stays visible on a row that is also selected.
              (selected ? "bg-accent-900 text-accent-100 " : "hover:bg-neutral-900 ") +
              "outline outline-1 -outline-offset-1 outline-accent-400"
            : selected
              ? "bg-accent-900 text-accent-100 outline outline-1 -outline-offset-1 outline-accent-600"
              : "hover:bg-neutral-900")
      }
    >
      <span className="flex min-w-0 items-center gap-2">
        {isDir && selected ? (
          <FolderOpenIcon size={15} className={`shrink-0 ${iconColor}`} />
        ) : isDir ? (
          <FolderIcon size={15} className={`shrink-0 ${iconColor}`} />
        ) : (
          <span className="shrink-0">{fileGlyph(entry, iconColor)}</span>
        )}
        {renaming ? (
          <RenameInput
            initial={entry.name}
            onCommit={onRenameCommit}
            onCancel={onRenameCancel}
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
}

/* ── The tile view. Selecting, opening, renaming and dragging all behave as
   they do in the list; only the layout is different. ── */
