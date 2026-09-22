import { getCurrentWebview } from "@tauri-apps/api/webview";
import { groupPathsByDir } from "./osFiles";
import { runTransfer } from "../state/transferQueueStore";
import { usePaneStore, childPath } from "../state/paneStore";
import { useUiStore } from "../state/uiStore";
import { useOsDragStore } from "../state/osDragStore";
import { useContextMenu } from "../state/contextMenuStore";
import { useToastStore } from "../state/toastStore";
import { t } from "../locale/en";

/* Files dropped onto the window from a file manager.

   This is the operating system's own drag channel, separate from the drag
   between the two panes, and it only runs on Linux: on Windows and macOS the
   setting that enables it disables in-window dragging instead, and dragging
   between panes was judged the more important of the two. There the same job is
   done by dragging inside the app, or by the copy and move keys.

   Where a drop lands is worked out from the pointer position: the pane under
   it, and the folder row under it if there is one. Dropped paths are grouped by
   their source directory and handed to the transfer queue, which brings the
   conflict handling with it.

   Whether a drop copies or asks first is the user's setting. */

interface Pos {
  x: number;
  y: number;
}

/** What sits under the pointer: which pane, and which folder row if any.
    The event reports physical pixels, which have to be scaled to CSS pixels
    before anything on the page can be found at that point. */
function targetAt(pos: Pos): { index: 0 | 1 | null; folder: string | null } {
  const dpr = window.devicePixelRatio || 1;
  const el = document.elementFromPoint(pos.x / dpr, pos.y / dpr);
  const section = el?.closest("[data-pane]");
  const n = section ? Number(section.getAttribute("data-pane")) : NaN;
  const index = n === 0 || n === 1 ? (n as 0 | 1) : null;
  const row = el?.closest("[data-entry]");
  const folder =
    row && row.getAttribute("data-kind") === "dir"
      ? row.getAttribute("data-entry")
      : null;
  return { index, folder };
}

function activeTab(pane: ReturnType<typeof usePaneStore.getState>["panes"][number]) {
  return pane.tabs.find((tb) => tb.id === pane.activeTabId) ?? pane.tabs[0];
}

async function handleDrop(
  index: 0 | 1,
  folder: string | null,
  paths: string[],
  pos: Pos,
): Promise<void> {
  const pane = usePaneStore.getState().panes[index];
  const tab = activeTab(pane);
  // A drop on a folder row goes into that folder, not into the open directory.
  const destPath = folder ? childPath(tab.path, folder) : tab.path;

  let groups;
  try {
    groups = await groupPathsByDir(paths);
  } catch {
    useToastStore.getState().push(t("osdrop.failed"), "error");
    return;
  }
  if (groups.size === 0) return;

  const run = (mode: "copy" | "move") => {
    for (const [dir, items] of groups) {
      void runTransfer({
        items,
        srcSide: "local",
        srcPath: dir,
        destSide: pane.side,
        destPath,
        mode,
        fromLabel: t("route.os"), // how the queue names the source
      });
    }
  };

  if (useUiStore.getState().osDropMode === "menu") {
    const dpr = window.devicePixelRatio || 1;
    useContextMenu.getState().openMenu(pos.x / dpr, pos.y / dpr, [
      { id: "os-move", label: t("dnd.moveHere"), onSelect: () => run("move") },
      { id: "os-copy", label: t("dnd.copyHere"), onSelect: () => run("copy") },
      { id: "os-sep", separator: true },
      { id: "os-cancel", label: t("dnd.cancel"), onSelect: () => {} },
    ]);
  } else {
    run("copy");
  }
}

let wired = false;

/** Wires up the OS drag channel; called once at startup. */
export async function initOsDrop(): Promise<void> {
  if (wired) return;
  wired = true;
  const setOver = useOsDragStore.getState().setOver;

  await getCurrentWebview().onDragDropEvent((event) => {
    const p = event.payload;
    if (p.type === "enter" || p.type === "over") {
      // Only a pane highlights. Over the divider or the chrome there is no
      // target, and keeping the last highlight stops it flickering at the edge.
      const { index, folder } = targetAt(p.position);
      if (index !== null) setOver(index, folder);
    } else if (p.type === "leave") {
      setOver(null, null);
    } else if (p.type === "drop") {
      const { index, folder } = targetAt(p.position);
      setOver(null, null);
      if (index !== null && p.paths.length > 0) {
        void handleDrop(index, folder, p.paths, p.position);
      }
    }
  });
}
