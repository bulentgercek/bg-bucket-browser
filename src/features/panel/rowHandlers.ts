import {
  useMemo,
  useRef,
  type DragEvent as ReactDragEvent,
  type MouseEvent as ReactMouseEvent,
} from "react";
import { type Entry } from "../../state/paneStore";
import { type RowDnd } from "./rowDnd";

/* What a row or tile calls when something happens to it, always with its own
   entry. The set is made once per list and keeps calling the pane's latest
   functions, so a memoised row is drawn again only when its own data changes,
   not because the pane made new functions. */
export interface RowHandlers {
  click: (e: ReactMouseEvent, entry: Entry) => void;
  open: (entry: Entry) => void;
  context: (e: ReactMouseEvent, entry: Entry) => void;
  renameCommit: (entry: Entry, value: string) => void;
  renameCancel: () => void;
  dragStart: (e: ReactDragEvent, entry: Entry) => void;
  dragEnd: () => void;
  dirDragOver: (e: ReactDragEvent, entry: Entry) => void;
  dirDrop: (e: ReactDragEvent, entry: Entry) => void;
}

export function useRowHandlers(latest: {
  onItemClick: (e: ReactMouseEvent, entry: Entry) => void;
  onOpen: (entry: Entry) => void;
  onContext: (e: ReactMouseEvent, entry: Entry) => void;
  onRenameCommit: (entry: Entry, value: string) => void;
  onRenameCancel: () => void;
  dnd: RowDnd;
}): RowHandlers {
  const ref = useRef(latest);
  ref.current = latest;
  return useMemo<RowHandlers>(
    () => ({
      click: (e, entry) => ref.current.onItemClick(e, entry),
      open: (entry) => ref.current.onOpen(entry),
      context: (e, entry) => ref.current.onContext(e, entry),
      renameCommit: (entry, value) => ref.current.onRenameCommit(entry, value),
      renameCancel: () => ref.current.onRenameCancel(),
      dragStart: (e, entry) => ref.current.dnd.onDragStart(e, entry),
      dragEnd: () => ref.current.dnd.onDragEnd(),
      dirDragOver: (e, entry) => ref.current.dnd.onDirDragOver(e, entry),
      dirDrop: (e, entry) => ref.current.dnd.onDirDrop(e, entry),
    }),
    [],
  );
}
