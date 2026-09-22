import {
  useEffect,
  useRef,
  useState,
  type DragEvent as ReactDragEvent,
} from "react";
import { ArrowRightIcon, CopyIcon } from "@phosphor-icons/react";
import {
  usePaneStore,
  childPath,
  type Entry,
  type PaneIndex,
  type PaneState,
  type TabState,
} from "../../state/paneStore";
import { useThumbStore, thumbKey } from "../../state/thumbStore";
import { runTransfer } from "../../state/transferQueueStore";
import { useDragStore, type DragPayload } from "../../state/dragStore";
import { useOsDragStore } from "../../state/osDragStore";
import { useContextMenu } from "../../state/contextMenuStore";
import { t } from "../../locale/en";
import { type RowDnd } from "./rowDnd";
import { iconsDragImage } from "./IconsView";

/* Dragging entries within the app, and the highlights for drags from anywhere.

   Every row can be dragged; a drop lands in the folder under the pointer, or
   in the directory the pane is showing. Whether it copies or moves is asked
   after the drop, because a held key cannot be read during a drag. Returns the
   handlers for the rows and for the list around them, and whether the list
   should be framed as the drop target. */
export function usePaneDnd({
  index,
  pane,
  tab,
  remote,
  bucket,
  selTargets,
}: {
  index: PaneIndex;
  pane: PaneState;
  tab: TabState;
  remote: boolean;
  bucket: string;
  /** What a drag starting on this entry carries: the selection it is part of, or itself. */
  selTargets: (clicked: Entry) => Entry[];
}): {
  rowDnd: RowDnd;
  showBodyFrame: boolean;
  osBodyFrame: boolean;
  onBodyDragOver: (e: ReactDragEvent) => void;
  onBodyDragLeave: () => void;
  onBodyDrop: (e: ReactDragEvent) => void;
} {
  const setSelection = usePaneStore((s) => s.setSelection);
  const openMenu = useContextMenu((s) => s.openMenu);

  // What a drag is doing, for the highlights. The pointer position is
  // deliberately not read here: it changes on every move and would redraw the
  // whole pane each time.
  const dragActive = useDragStore((s) => s.payload !== null);
  const dragOverSide = useDragStore((s) => s.overSide);
  const dragOverFolder = useDragStore((s) => s.overFolder);
  // The same, for a drag coming from outside the app: either a folder row is
  // highlighted, or the pane as a whole is.
  const osOnThisPane = useOsDragStore((s) => s.overIndex === index);
  const rawOsDropFolder = useOsDragStore((s) =>
    s.overIndex === index ? s.overFolder : null,
  );
  const osBodyFrameWanted = osOnThisPane && rawOsDropFolder === null;

  // The highlights are delayed a little. Crossing the one-pixel gap between
  // two rows counts as leaving the list, and without the delay the highlight
  // flickers on every row boundary.
  const rawDropFolder =
    dragActive && dragOverSide === pane.side ? dragOverFolder : null;
  const [dropFolder, setDropFolder] = useState<string | null>(null);
  const bodyFrameWanted =
    dragActive && dragOverSide === pane.side && rawDropFolder === null;
  const [showBodyFrame, setShowBodyFrame] = useState(false);
  const [osDropFolder, setOsDropFolder] = useState<string | null>(null);
  const [osBodyFrame, setOsBodyFrame] = useState(false);
  useEffect(() => {
    const id = window.setTimeout(() => setDropFolder(rawDropFolder), 55);
    return () => clearTimeout(id);
  }, [rawDropFolder]);
  useEffect(() => {
    const id = window.setTimeout(() => setShowBodyFrame(bodyFrameWanted), 55);
    return () => clearTimeout(id);
  }, [bodyFrameWanted]);
  useEffect(() => {
    const id = window.setTimeout(() => setOsDropFolder(rawOsDropFolder), 55);
    return () => clearTimeout(id);
  }, [rawOsDropFolder]);
  useEffect(() => {
    const id = window.setTimeout(() => setOsBodyFrame(osBodyFrameWanted), 55);
    return () => clearTimeout(id);
  }, [osBodyFrameWanted]);
  // Leaving the pane clears the highlight after a moment, and any further
  // movement inside cancels that — which is what tells a real exit from the gap
  // between two rows.
  const dragLeaveTimer = useRef<number | null>(null);
  const cancelDragLeave = () => {
    if (dragLeaveTimer.current !== null) {
      clearTimeout(dragLeaveTimer.current);
      dragLeaveTimer.current = null;
    }
  };

  const dndDestLabel = (p: string) =>
    remote ? (p ? `${bucket}/${p}` : bucket) : p || "~";

  const performDrop = (
    payload: DragPayload,
    destPath: string,
    x: number,
    y: number,
  ) => {
    useDragStore.getState().end();
    // A folder cannot be dropped into itself.
    const destLeaf = destPath.split("/").filter(Boolean).pop() ?? "";
    const sameSpot =
      payload.sourceSide === pane.side && payload.sourcePath === tab.path;
    if (
      sameSpot &&
      destPath !== tab.path &&
      payload.items.some((i) => i.name === destLeaf)
    ) {
      return;
    }
    const run = (mode: "copy" | "move") =>
      void runTransfer({
        items: payload.items,
        srcSide: payload.sourceSide,
        srcPath: payload.sourcePath,
        destSide: pane.side,
        destPath,
        mode,
      });
    // The decision is made here, after the drop.
    openMenu(x, y, [
      {
        id: "drop-move",
        label: t("dnd.moveHere"),
        icon: <ArrowRightIcon size={14} />,
        onSelect: () => run("move"),
      },
      {
        id: "drop-copy",
        label: t("dnd.copyHere"),
        icon: <CopyIcon size={14} />,
        onSelect: () => run("copy"),
      },
      { id: "drop-sep", separator: true },
      { id: "drop-cancel", label: t("dnd.cancel"), onSelect: () => {} },
    ]);
  };

  const rowDnd: RowDnd = {
    // The highlighted folder, from either kind of drag
    dropTarget: dropFolder ?? osDropFolder,
    onDragStart: (e, entry) => {
      const items = selTargets(entry);
      e.dataTransfer.effectAllowed = "copyMove";
      // Tiles drag as a small chip rather than as themselves.
      if (tab.viewMode === "icons") {
        const k = thumbKey(
          pane.side,
          childPath(tab.path, entry.name),
          entry.modified,
        );
        const th = useThumbStore.getState().map[k];
        iconsDragImage(e, th?.status === "ready" ? th.uri : undefined);
      }
      e.dataTransfer.setData(
        "text/plain",
        items.map((i) => i.name).join("\n"),
      );
      if (!tab.selection.includes(entry.name)) {
        setSelection(index, [entry.name], entry.name, "add");
      }
      useDragStore
        .getState()
        .start({ items, sourceSide: pane.side, sourcePath: tab.path });
    },
    onDragEnd: () => useDragStore.getState().end(),
    onDirDragOver: (e, entry) => {
      const st = useDragStore.getState();
      if (!st.payload) return;
      e.preventDefault();
      e.stopPropagation(); // the row is the target, not the pane behind it
      cancelDragLeave();
      e.dataTransfer.dropEffect = "move"; // a neutral cursor: the choice comes later
      const dp = childPath(tab.path, entry.name);
      st.update({
        cursor: { x: e.clientX, y: e.clientY },
        dest: { side: pane.side, path: dp, label: dndDestLabel(dp) },
        overSide: pane.side,
        overFolder: entry.name,
      });
    },
    onDirDrop: (e, entry) => {
      const d = useDragStore.getState();
      if (!d.payload) return;
      e.preventDefault();
      e.stopPropagation();
      performDrop(
        d.payload,
        childPath(tab.path, entry.name),
        e.clientX,
        e.clientY,
      );
    },
  };

  const onBodyDragOver = (e: ReactDragEvent) => {
    const st = useDragStore.getState();
    if (!st.payload) return;
    e.preventDefault();
    cancelDragLeave();
    e.dataTransfer.dropEffect = "move"; // a neutral cursor: the choice comes later
    st.update({
      cursor: { x: e.clientX, y: e.clientY },
      dest: { side: pane.side, path: tab.path, label: dndDestLabel(tab.path) },
      overSide: pane.side,
      overFolder: null,
    });
  };
  const onBodyDragLeave = () => {
    // What the pointer moved onto is not reported here, so leaving cannot be
    // told from crossing a gap directly. Clearing is delayed instead, and any
    // movement still inside the pane cancels it.
    cancelDragLeave();
    dragLeaveTimer.current = window.setTimeout(() => {
      dragLeaveTimer.current = null;
      useDragStore
        .getState()
        .update({ overSide: null, overFolder: null, dest: null });
    }, 140);
  };
  const onBodyDrop = (e: ReactDragEvent) => {
    const d = useDragStore.getState();
    if (!d.payload) return;
    e.preventDefault();
    cancelDragLeave();
    performDrop(d.payload, tab.path, e.clientX, e.clientY);
  };

  return {
    rowDnd,
    showBodyFrame,
    osBodyFrame,
    onBodyDragOver,
    onBodyDragLeave,
    onBodyDrop,
  };
}
