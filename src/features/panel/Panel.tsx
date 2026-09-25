import {
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
  type MouseEvent as ReactMouseEvent,
} from "react";
import {
  PlusIcon,
  FolderOpenIcon,
  FileTextIcon,
  FileZipIcon,
  PushPinIcon,
  CopyIcon,
  ArrowRightIcon,
  DownloadSimpleIcon,
  TrayArrowDownIcon,
  CursorTextIcon,
  FolderPlusIcon,
  LinkIcon,
  InfoIcon,
  TrashIcon,
  ArrowsClockwiseIcon,
  ListChecksIcon,
  ProhibitIcon,
  SelectionInverseIcon,
  ClipboardIcon,
  ClipboardTextIcon,
  ScissorsIcon,
  ArrowSquareOutIcon,
} from "@phosphor-icons/react";
import {
  usePaneStore,
  childPath,
  type Entry,
  type PaneIndex,
  type ViewSize,
} from "../../state/paneStore";
import {
  createFolder,
  createFile,
  deleteEntries,
  renameEntry,
  isConnectionComplete,
  openPath,
  type FsErr,
} from "../../lib/commands";
import { useToastStore } from "../../state/toastStore";
import { useDialogStore } from "../../state/dialogStore";
import {
  startPaneTransfer,
  startPaneZip,
  startOpenRemote,
  pasteIntoPane,
} from "../../state/transferQueueStore";
import { useClipboardStore } from "../../state/clipboardStore";
import { visibleEntries, sortEntries } from "../../lib/entries";
import { writeText } from "@tauri-apps/plugin-clipboard-manager";
import { listen } from "@tauri-apps/api/event";
import { useUiStore } from "../../state/uiStore";
import { usePinStore } from "../../state/pinStore";
import { useConnectionStore } from "../../state/connectionStore";
import { useDeviceStore } from "../../state/deviceStore";
import { useContextMenu, type MenuItem } from "../../state/contextMenuStore";
import { t, type StringKey } from "../../locale/en";
import { ListBody, ColHeader, ParentRow } from "./ListView";
import { IconsHeader, IconsBody } from "./IconsView";
import { TabStrip } from "./TabStrip";
import { Breadcrumb } from "./Breadcrumb";
import { SearchBox } from "./SearchBox";
import { ViewControl } from "./ViewControl";
import { PropertiesContent } from "./PropertiesContent";
import { usePaneKeyboard } from "./usePaneKeyboard";
import { usePaneDnd } from "./usePaneDnd";

/* One pane: its tabs, the path and filter above the list, the list itself, and
   everything that can be done to what is in it.

   Both panes are this same component; which side it is comes from its index.
   Most of the file is interaction — selecting, renaming, the context menus and
   the file operations behind them — rather than layout. The keyboard and
   dragging have hooks of their own, and the pieces drawn here live in the
   files next to this one. */

/* Below this width the filter field shrinks to its icon, so the path above the
   list keeps its room. Focusing it opens it again. */
const FILTER_COLLAPSE_W = 500;

function colGrid(panelWidth: number): { gridClass: string; showModified: boolean } {
  if (panelWidth < 300) {
    return { gridClass: "grid grid-cols-[1fr_70px] gap-2", showModified: false };
  }
  if (panelWidth < 420) {
    return {
      gridClass: "grid grid-cols-[1fr_70px_96px] gap-2",
      showModified: true,
    };
  }
  return {
    gridClass: "grid grid-cols-[1fr_78px_116px] gap-2",
    showModified: true,
  };
}

/* Row heights for the list view; the horizontal padding does not change. */
const DETAILS_ROW: Record<ViewSize, string> = {
  s: "py-1 text-[12px]",
  m: "py-1.5 text-[12.5px]",
  l: "py-[7px] text-[13px]",
};

export default function Panel({ index }: { index: PaneIndex }) {
  const pane = usePaneStore((s) => s.panes[index]);
  const navigate = usePaneStore((s) => s.navigate);
  const goParent = usePaneStore((s) => s.goParent);
  const openEntry = usePaneStore((s) => s.openEntry);
  const setSelection = usePaneStore((s) => s.setSelection);
  const setActiveTab = usePaneStore((s) => s.setActiveTab);
  const addTab = usePaneStore((s) => s.addTab);
  const closeTab = usePaneStore((s) => s.closeTab);
  const setFilter = usePaneStore((s) => s.setFilter);
  const setSort = usePaneStore((s) => s.setSort);
  const beginRename = usePaneStore((s) => s.beginRename);
  const cancelRename = usePaneStore((s) => s.cancelRename);
  const setViewMode = usePaneStore((s) => s.setViewMode);
  const setViewSize = usePaneStore((s) => s.setViewSize);
  const setCursor = usePaneStore((s) => s.setCursor);
  const setScrollTop = usePaneStore((s) => s.setScrollTop);
  const exitFilter = usePaneStore((s) => s.exitFilter);
  const showHidden = useUiStore((s) => s.showHidden);
  const flipped = useUiStore((s) => s.flipped);
  const setActivePane = useUiStore((s) => s.setActivePane);
  const active = useUiStore((s) => s.activePane) === index;
  const addPin = usePinStore((s) => s.addPin);
  const bucket = useConnectionStore((s) => s.bucket);
  const homeDir = useDeviceStore((s) => s.homeDir);
  const connCount = useConnectionStore((s) => s.connections.length);
  const activeConn = useConnectionStore(
    (s) => s.connections.find((c) => c.id === s.activeId) ?? s.connections[0] ?? null,
  );
  const setScreen = useUiStore((s) => s.setScreen);
  const setSettingsTab = useUiStore((s) => s.setSettingsTab);
  const openMenu = useContextMenu((s) => s.openMenu);
  const clipboard = useClipboardStore((s) => s.clipboard);
  // The app's own clipboard wins; the OS one is what is left to paste when it
  // is empty.
  const osTag = useClipboardStore((s) => s.osClipboardTag);
  const canPaste = clipboard != null || osTag != null;
  const pasteMode = clipboard?.mode ?? osTag?.mode ?? "copy";
  const pasteLabel = pasteMode === "move" ? "ctx.pasteMove" : "ctx.pasteCopy";
  const tab = pane.tabs.find((tb) => tb.id === pane.activeTabId) ?? pane.tabs[0];
  const remote = pane.side === "remote";

  // The menu says "to the left" or "to the right", which depends on how the
  // panes are arranged rather than on which pane this is.
  const otherPanelSide = (): "left" | "right" => {
    const remoteOnLeft = !flipped;
    const otherIsRemote = index === 1;
    const otherOnLeft = otherIsRemote ? remoteOnLeft : !remoteOnLeft;
    return otherOnLeft ? "left" : "right";
  };

  // The tab that started the action is the one listed again, even if the user
  // has switched tabs while it ran.
  const reload = () => void navigate(index, tab.path, tab.id);

  // What the pane shows instead of a list, and what can be done from there.
  const stateOpts = {
    noConnection: remote && connCount === 0,
    // A connection that exists but cannot possibly work. The test has to match
    // the backend's own idea of usable — keys and endpoint and region and
    // bucket — or a half-filled connection reaches the list and draws a path
    // out of empty strings.
    credentialError:
      remote &&
      connCount > 0 &&
      activeConn != null &&
      !isConnectionComplete(activeConn),
    // The configuration is complete and the volume still turned us away. The
    // same screen fits, with its own sentence: nothing here is browsable, and
    // a folder-level error with a Retry button would point at the wrong thing.
    connectionRejected: remote && tab.status === "error" && tab.error === "credentials",
    onRetry: reload,
    onOpenSettings: () => {
      setSettingsTab("connections");
      setScreen("settings");
    },
  };
  const errText = (e: unknown) => {
    const fe = e as Partial<FsErr>;
    const kind = fe?.kind ?? "unknown";
    const detail = fe?.detail ? ` — ${fe.detail}` : "";
    return t(`fs.error.${kind}` as StringKey) + detail;
  };
  const toastErr = (e: unknown) => {
    useToastStore.getState().push(errText(e), "error");
  };

  const uniqueName = (base: string, ext = "") => {
    const taken = new Set(tab.listing.map((e) => e.name));
    let name = `${base}${ext}`;
    let n = 2;
    while (taken.has(name)) name = `${base} ${n++}${ext}`;
    return name;
  };

  // Refresh even after a failure: something changed underneath, and the list
  // should show what is really there.
  const toastErrReload = (e: unknown) => {
    toastErr(e);
    reload();
  };

  const doCreateFolder = () => {
    const name = uniqueName("New folder");
    createFolder(remote, tab.path, name)
      .then((finalName) => {
        useToastStore.getState().push(t("fs.folderCreated", { name: finalName }));
        reload();
        beginRename(index, finalName); // straight into renaming it
      })
      .catch(toastErrReload);
  };

  const doCreateFile = () => {
    const name = uniqueName("New file", ".txt");
    createFile(remote, tab.path, name)
      .then((finalName) => {
        useToastStore.getState().push(t("fs.fileCreated", { name: finalName }));
        reload();
        beginRename(index, finalName);
      })
      .catch(toastErrReload);
  };

  const doProperties = (entry: Entry) => {
    useDialogStore.getState().show({
      kind: "info",
      paneIndex: index,
      title: t("ctx.properties"),
      content: (
        <PropertiesContent
          entry={entry}
          full={childPath(tab.path, entry.name)}
          remote={remote}
          bucket={bucket}
        />
      ),
    });
  };

  const doRename = (entry: Entry, raw: string) => {
    const next = raw.trim();
    if (!next || next === entry.name) {
      cancelRename(index);
      return;
    }
    renameEntry(
      remote,
      childPath(tab.path, entry.name),
      entry.kind === "dir",
      next,
    )
      .then((finalName) => {
        useToastStore.getState().push(t("fs.renamed", { name: finalName }));
        cancelRename(index);
        reload();
      })
      .catch((e) => {
        toastErr(e);
        cancelRename(index);
        reload();
      });
  };

  // The plus button above the list. It sits at the pane's right end, so its
  // menu hangs leftwards from the button's right edge and stays in this pane.
  const onPlusMenu = (e: ReactMouseEvent) => {
    const btn = e.currentTarget as HTMLElement;
    // A second press closes the menu this button opened.
    const menu = useContextMenu.getState();
    if (menu.open && menu.anchor === btn) {
      menu.closeMenu();
      return;
    }
    const r = btn.getBoundingClientRect();
    openMenu(
      r.right,
      r.bottom + 4,
      [
        {
          id: "newtab",
          label: t("ctx.newTab"),
          icon: <PlusIcon size={14} />,
          onSelect: () => addTab(index),
        },
        {
          id: "newfolder",
          label: t("ctx.newFolder"),
          icon: <FolderPlusIcon size={14} />,
          onSelect: doCreateFolder,
        },
        {
          id: "newfile",
          label: t("ctx.newTextFile"),
          icon: <FileTextIcon size={14} />,
          onSelect: doCreateFile,
        },
      ],
      { alignRight: true, anchor: btn },
    );
  };

  // Right-clicking the empty space below the list.
  const onBgContext = (e: ReactMouseEvent) => {
    e.preventDefault();
    setActivePane(index); // acting on a pane makes it the active one
    openMenu(e.clientX, e.clientY, [
      {
        id: "newfolder",
        label: t("ctx.newFolder"),
        icon: <FolderPlusIcon size={14} />,
        onSelect: doCreateFolder,
      },
      {
        id: "newfile",
        label: t("ctx.newTextFile"),
        icon: <FileTextIcon size={14} />,
        onSelect: doCreateFile,
      },
      ...(canPaste
        ? [
            {
              id: "paste-bg",
              label: t(pasteLabel),
              icon: <ClipboardTextIcon size={14} />,
              onSelect: () => void doPaste(tab.path),
            } as MenuItem,
          ]
        : []),
      { id: "sep-bg1", separator: true },
      ...selectItems,
      { id: "sep-bg2", separator: true },
      {
        id: "refresh",
        label: t("ctx.refresh"),
        icon: <ArrowsClockwiseIcon size={14} />,
        onSelect: reload,
      },
    ]);
  };

  // Right-clicking inside the selection acts on all of it; right-clicking
  // something else acts on that alone.
  const selTargets = (clicked: Entry): Entry[] => {
    const sel = tab.listing.filter((e) => tab.selection.includes(e.name));
    return sel.some((e) => e.name === clicked.name) ? sel : [clicked];
  };

  // Sends this pane's selection to the other one.
  const doTransfer = (move: boolean) => startPaneTransfer(index, move);

  // Hands a local path to the system, which opens a folder in the file manager
  // and a file in its own application.
  const openInOS = (entry: Entry) => {
    openPath("local", childPath(tab.path, entry.name)).catch(() => {
      useToastStore
        .getState()
        .push(t("open.failed", { name: entry.name }), "error");
    });
  };

  // Opening an entry, by double click or from the menu. A folder opens in the
  // pane; a local file goes to the system; a remote file has to be downloaded
  // first, so it goes through the queue.
  const handleOpen = (entry: Entry) => {
    if (entry.kind !== "dir" && remote) {
      startOpenRemote(entry, tab.path);
      return;
    }
    void openEntry(index, entry);
  };

  // Copy and cut put the selection on the app's clipboard; where it lands is
  // decided when it is pasted.
  const doClip = (entry: Entry, mode: "copy" | "move") => {
    useClipboardStore.getState().setClipboard({
      items: selTargets(entry),
      srcSide: pane.side,
      srcPath: tab.path,
      mode,
      origin: "inApp",
    });
  };
  // Pasting lives with the queue, so the menu entry and the keyboard shortcut
  // are the same code.
  const doPaste = (destPath: string) => void pasteIntoPane(index, destPath);

  // ── Drag and drop ────────────────────────────────────────
  const {
    rowDnd,
    showBodyFrame,
    osBodyFrame,
    onBodyDragOver,
    onBodyDragLeave,
    onBodyDrop,
  } = usePaneDnd({ index, pane, tab, remote, bucket, selTargets });

  const runDelete = async (targets: Entry[], permanent: boolean) => {
    const total = targets.length;
    const label =
      total === 1 ? targets[0].name : t("fs.nItems", { count: total });
    const doneText = permanent
      ? t("fs.deleted", { name: label })
      : t("fs.trashed", { name: label });

    // Anything that can take a while reports as it goes. A large remote folder
    // takes minutes, and without a count it looks like nothing is happening.
    const progress = total > 1 || targets.some((e) => e.kind === "dir");
    const toastId = crypto.randomUUID();
    const where = remote ? bucket : "~";
    const progressText = (done: number, files: number) =>
      files === 1
        ? t("fs.deletingFile", { name: label })
        : files > 1
        ? t("fs.deletingFiles", { name: label, files })
        : total > 1
          ? t("fs.deleting", { done, total, where })
          : t("fs.deletingOne", { name: label });
    let unlisten: (() => void) | undefined;
    if (progress) {
      useToastStore.getState().push(progressText(0, 0), "info", {
        id: toastId,
        sticky: true,
      });
      unlisten = await listen<{ done: number; total: number; files: number }>(
        "delete-progress",
        (e) => {
          useToastStore
            .getState()
            .push(progressText(e.payload.done, e.payload.files), "info", {
              id: toastId,
              sticky: true,
            });
        },
      );
    }

    try {
      await deleteEntries(
        remote,
        permanent,
        targets.map((e) => ({
          path: childPath(tab.path, e.name),
          isDir: e.kind === "dir",
        })),
      );
      if (progress) useToastStore.getState().settle(toastId, doneText, "info");
      else useToastStore.getState().push(doneText);
      setSelection(index, [], null);
      reload();
    } catch (e) {
      if (progress)
        useToastStore.getState().settle(toastId, errText(e), "error");
      else toastErr(e);
      reload();
    } finally {
      unlisten?.();
    }
  };

  // Permanent deletion asks first; the trash does not need to.
  const requestDelete = (targets: Entry[], permanent: boolean) => {
    if (targets.length === 0) return;
    if (!permanent) {
      void runDelete(targets, false); // recoverable, so no question
      return;
    }
    const multi = targets.length > 1;
    useDialogStore.getState().show({
      kind: "confirm",
      paneIndex: index,
      title: t("dialog.deleteTitle"),
      body: multi
        ? t(remote ? "dialog.deleteRemoteBodyN" : "dialog.deletePermanentBodyN", {
            count: targets.length,
          })
        : t(remote ? "dialog.deleteRemoteBody" : "dialog.deletePermanentBody", {
            name: targets[0].name,
          }),
      confirmLabel: t("ctx.delete"),
      danger: true,
      onConfirm: () => void runDelete(targets, true),
    });
  };
  // A clicked entry takes the selection along when it is part of it.
  const doDelete = (clicked: Entry, permanent: boolean) =>
    requestDelete(selTargets(clicked), permanent);

  const rowMenu = (entry: Entry, shift: boolean): MenuItem[] => {
    const isDir = entry.kind === "dir";
    const single = tab.selection.length <= 1; // right-clicking selects the row
    const side = t(`ctx.side.${otherPanelSide()}` as StringKey);
    const permanent = remote || shift; // the volume has no trash; shift skips it
    const items: MenuItem[] = [];

    // Opening outside the app: a single file on either side, and a single
    // local folder in the file manager. A remote folder has nothing to open.
    if (single && !isDir) {
      items.push({
        id: "open-os",
        label: t("ctx.open"),
        icon: <ArrowSquareOutIcon size={14} />,
        onSelect: () => handleOpen(entry),
      });
      items.push({ id: "sep-open", separator: true });
    } else if (single && isDir && !remote) {
      items.push({
        id: "open-os",
        label: t("ctx.openInOS"),
        icon: <ArrowSquareOutIcon size={14} />,
        onSelect: () => openInOS(entry),
      });
    }

    if (isDir) {
      items.push({
        id: "newtab",
        label: t("ctx.openNewTab"),
        icon: <FolderOpenIcon size={14} />,
        onSelect: () => addTab(index, childPath(tab.path, entry.name)),
      });
      items.push({
        id: "pin",
        label: t("ctx.addPinned"),
        icon: <PushPinIcon size={14} />,
        onSelect: () =>
          addPin({
            side: pane.side,
            path: childPath(tab.path, entry.name),
            label: entry.name,
          }),
      });
      items.push({ id: "sep-dir", separator: true });
    }

    items.push({
      id: "copy",
      label: t("ctx.copyTo", { side }),
      icon: <CopyIcon size={14} />,
      shortcut: "F5",
      onSelect: () => doTransfer(false),
    });
    items.push({
      id: "move",
      label: t("ctx.moveTo", { side }),
      icon: <ArrowRightIcon size={14} />,
      shortcut: "F6",
      onSelect: () => doTransfer(true),
    });
    items.push({
      id: "clip-copy",
      label: t("ctx.clipCopy"),
      icon: <ClipboardIcon size={14} />,
      onSelect: () => doClip(entry, "copy"),
    });
    items.push({
      id: "clip-move",
      label: t("ctx.clipMove"),
      icon: <ScissorsIcon size={14} />,
      onSelect: () => doClip(entry, "move"),
    });
    if (canPaste) {
      items.push({
        id: "paste",
        label: t(pasteLabel),
        icon: <ClipboardTextIcon size={14} />,
        onSelect: () =>
          void doPaste(isDir ? childPath(tab.path, entry.name) : tab.path),
      });
    }
    items.push({
      id: "dl",
      label: remote ? t("ctx.download") : t("ctx.upload"),
      icon: remote ? <DownloadSimpleIcon size={14} /> : <TrayArrowDownIcon size={14} />,
      onSelect: () => doTransfer(false),
    });
    if (remote) {
      items.push({
        id: "dlzip",
        label: t("ctx.downloadZip"),
        icon: <FileZipIcon size={14} />,
        onSelect: () => startPaneZip(index),
      });
    }
    items.push({ id: "sep-1", separator: true });
    items.push({
      id: "rename",
      label: t("ctx.rename"),
      icon: <CursorTextIcon size={14} />,
      disabled: !single, // renaming is for one entry at a time
      onSelect: () => beginRename(index, entry.name),
    });
    items.push({
      id: "newfolder",
      label: t("ctx.newFolder"),
      icon: <FolderPlusIcon size={14} />,
      onSelect: doCreateFolder,
    });
    if (remote) {
      items.push({
        id: "s3path",
        label: t("ctx.copyPath"),
        icon: <LinkIcon size={14} />,
        onSelect: () => {
          const key = childPath(tab.path, entry.name);
          void writeText(`s3://${bucket || "bucket"}/${key}`);
        },
      });
    }
    items.push({
      id: "props",
      label: t("ctx.properties"),
      icon: <InfoIcon size={14} />,
      disabled: !single,
      onSelect: () => doProperties(entry),
    });
    items.push({ id: "sep-2", separator: true });
    items.push({
      id: "delete",
      label: (
        <>
          {t("ctx.delete")}{" "}
          <span
            style={permanent ? { color: "var(--color-warn)" } : undefined}
            className={permanent ? "" : "text-neutral-500"}
          >
            ({t(permanent ? "ctx.deletePermanentTag" : "ctx.deleteTrashTag")})
          </span>
        </>
      ),
      icon: <TrashIcon size={14} />,
      danger: true,
      onSelect: () => doDelete(entry, permanent),
    });
    return items;
  };

  // What the list actually shows: filtered, then sorted. All of it happens
  // here, so typing narrows the list without asking for it again.
  const visible = useMemo(
    () =>
      sortEntries(
        visibleEntries(tab.listing, tab.filter, showHidden),
        tab.sort,
      ),
    [tab.listing, tab.filter, showHidden, tab.sort],
  );
  const filterActive = tab.filter.trim() !== "";

  // ── Selecting. The usual modifiers, plus clicking the one selected entry
  //    again to deselect it. Ranges follow the visible order. ──
  const visibleNames = visible.map((v) => v.name);
  const selectAll = () => setSelection(index, visibleNames, null);
  const selectNone = () => setSelection(index, [], null);
  const selectInvert = () =>
    setSelection(
      index,
      visibleNames.filter((n) => !tab.selection.includes(n)),
      null,
    );

  const onRowClick = (e: ReactMouseEvent, entry: Entry) => {
    const sel = tab.selection;
    // A click always moves the keyboard cursor too, so the two never disagree
    // about where the user is.
    setCursor(index, visibleNames.indexOf(entry.name));
    endShiftGesture(); // a click ends a keyboard shift-selection

    // Shift extends from the anchor to here, adding or removing depending on
    // what the click that set the anchor did.
    if (e.shiftKey && tab.anchor) {
      const a = visibleNames.indexOf(tab.anchor);
      const b = visibleNames.indexOf(entry.name);
      if (a !== -1 && b !== -1) {
        const [lo, hi] = a < b ? [a, b] : [b, a];
        const range = visibleNames.slice(lo, hi + 1);
        const next =
          tab.anchorMode === "sub"
            ? sel.filter((n) => !range.includes(n))
            : [...sel, ...range.filter((n) => !sel.includes(n))];
        setSelection(index, next, entry.name, tab.anchorMode);
        return;
      }
    }

    // Control toggles one entry and becomes the anchor, in whichever direction
    // it just went.
    if (e.ctrlKey || e.metaKey) {
      const has = sel.includes(entry.name);
      setSelection(
        index,
        has ? sel.filter((n) => n !== entry.name) : [...sel, entry.name],
        entry.name,
        has ? "sub" : "add",
      );
      return;
    }

    // A plain click narrows to this entry, or clears it if it was the only
    // one selected.
    if (sel.length === 1 && sel[0] === entry.name) {
      setSelection(index, [], null);
    } else {
      setSelection(index, [entry.name], entry.name, "add");
    }
  };

  const selectItems: MenuItem[] = [
    {
      id: "sel-all",
      label: t("ctx.selectAll"),
      icon: <ListChecksIcon size={14} />,
      onSelect: selectAll,
    },
    {
      id: "sel-none",
      label: t("ctx.selectNone"),
      icon: <ProhibitIcon size={14} />,
      disabled: tab.selection.length === 0,
      onSelect: selectNone,
    },
    {
      id: "sel-invert",
      label: t("ctx.selectInvert"),
      icon: <SelectionInverseIcon size={14} />,
      onSelect: selectInvert,
    },
  ];

  const onRowContext = (e: ReactMouseEvent, entry: Entry) => {
    e.preventDefault();
    e.stopPropagation();
    setActivePane(index); // acting on a pane makes it the active one
    if (!tab.selection.includes(entry.name)) {
      setSelection(index, [entry.name], entry.name);
    }
    openMenu(e.clientX, e.clientY, [
      ...rowMenu(entry, e.shiftKey),
      { id: "sep-sel", separator: true },
      ...selectItems,
    ]);
  };

  // The view belongs to the tab, not to the pane.
  const rowClass = DETAILS_ROW[tab.viewSize];

  // The columns follow the pane's own width. Measuring before the first paint
  // keeps a narrow pane from showing wide columns for a frame.
  const sectionRef = useRef<HTMLElement>(null);
  const [panelWidth, setPanelWidth] = useState(Number.POSITIVE_INFINITY);
  useLayoutEffect(() => {
    const el = sectionRef.current;
    if (!el) return;
    const update = () => setPanelWidth(el.clientWidth);
    update();
    const ro = new ResizeObserver(update);
    ro.observe(el);
    return () => ro.disconnect();
  }, []);
  const { gridClass, showModified } = colGrid(panelWidth);

  // ── Remembering where the list was scrolled to ──────────────────────────
  // Written back to the tab as it scrolls and restored when the tab comes back,
  // so switching tabs returns to the same place. A new folder starts at the
  // top, which the store takes care of.
  const scrollBodyRef = useRef<HTMLDivElement>(null);
  const scrollSaveTimer = useRef<number | null>(null);
  const scrollTopRef = useRef(tab.scrollTop);
  scrollTopRef.current = tab.scrollTop;
  const onBodyScroll = () => {
    if (scrollSaveTimer.current !== null) return;
    scrollSaveTimer.current = window.setTimeout(() => {
      scrollSaveTimer.current = null;
      const el = scrollBodyRef.current;
      if (el) setScrollTop(index, el.scrollTop);
    }, 120);
  };
  useLayoutEffect(() => {
    const el = scrollBodyRef.current;
    if (el) el.scrollTop = scrollTopRef.current;
  }, [tab.id, tab.path, tab.status]);

  // ── Control and the wheel change the row or tile size ───────────
  // Attached directly rather than through React, because the event has to be
  // cancelled: otherwise the page zooms and the list scrolls at the same
  // time.
  useEffect(() => {
    const el = sectionRef.current;
    if (!el) return;
    let lastAt = 0;
    const onWheel = (e: WheelEvent) => {
      if (!e.ctrlKey && !e.metaKey) return;
      e.preventDefault();
      const now = Date.now();
      if (now - lastAt < 90) return; // a trackpad pinch sends far too many
      lastAt = now;
      const st = usePaneStore.getState().panes[index];
      const tb = st.tabs.find((t) => t.id === st.activeTabId);
      if (!tb) return;
      const order: ViewSize[] = ["s", "m", "l"];
      const cur = order.indexOf(tb.viewSize);
      const next = Math.max(
        0,
        Math.min(order.length - 1, cur + (e.deltaY < 0 ? 1 : -1)),
      );
      if (next !== cur) setViewSize(index, order[next]);
    };
    el.addEventListener("wheel", onWheel, { passive: false });
    return () => el.removeEventListener("wheel", onWheel);
  }, [index, setViewSize]);

  // ── Moving around with the keyboard ────────────
  // The filter field, so a shortcut can put the cursor in it.
  const searchInputRef = useRef<HTMLInputElement>(null);
  // Whether the user opened the collapsed filter field.
  const [filterOpen, setFilterOpen] = useState(false);
  // Each tab decides that for itself.
  useEffect(() => setFilterOpen(false), [tab.id]);
  const { drawnCursor, endShiftGesture } = usePaneKeyboard({
    index,
    active,
    remote,
    tab,
    visible,
    handleOpen,
    requestDelete,
    openFilter: () => {
      setFilterOpen(true); // open it first if it is collapsed
      requestAnimationFrame(() => {
        searchInputRef.current?.focus();
        searchInputRef.current?.select();
      });
    },
  });

  // Keep the cursor row in view.
  useEffect(() => {
    if (!active) return;
    scrollBodyRef.current
      ?.querySelector<HTMLElement>('[data-cursor="true"]')
      ?.scrollIntoView({ block: "nearest", inline: "nearest" });
  }, [tab.cursorIndex, tab.id, active]);

  // Leaving the filter puts the cursor back into the list.
  const onFilterExit = () => exitFilter(index);

  return (
    <section
      ref={sectionRef}
      data-pane={index}
      tabIndex={-1}
      /* A pane becomes the active one when the pointer enters it and stays
         active after the pointer leaves: only another pane or the sidebar takes
         that over. The border is always drawn, in this side's own colour and in
         three strengths, so nothing shifts as it changes. */
      onMouseEnter={() => {
        // An open context menu freezes which pane is active: it belongs to the
        // entry it was opened on.
        if (useContextMenu.getState().open) return;
        setActivePane(index);
      }}
      className={
        "focus:outline-none " +
        "relative isolate flex min-h-0 min-w-0 flex-col rounded-sm border transition-colors " +
        (remote
          ? active
            ? "border-accent-600 hover:border-accent-400"
            : "border-accent-800"
          : active
            ? "border-local-600 hover:border-local-400"
            : "border-local-800")
      }
    >
      {/* The glow at the foot of the active pane, fading upwards. It sits on
          its own layer behind the content so it can never cover the list. */}
      <div
        aria-hidden
        className={
          "pointer-events-none absolute inset-0 -z-10 transition-opacity duration-150 " +
          (active ? "opacity-100" : "opacity-0")
        }
        style={{
          background: `linear-gradient(to top, var(${
            remote ? "--pane-lift-remote" : "--pane-lift-local"
          }), transparent 55%)`,
        }}
      />
      {stateOpts.noConnection ? (
        // With no connection at all, tabs and a path mean nothing: the whole
        // pane becomes one invitation to set one up.
        <div className="flex flex-1 min-h-0 flex-col items-center justify-center gap-3 px-6 text-center">
          <span className="text-[13px] text-neutral-500">
            {t("panel.noConnection")}
          </span>
          <button
            type="button"
            onClick={stateOpts.onOpenSettings}
            className="rounded-sm border border-neutral-800 px-3 py-1 text-[12px] text-neutral-400 hover:border-accent-700 hover:text-accent-300"
          >
            {t("panel.openSettings")}
          </button>
        </div>
      ) : stateOpts.credentialError || stateOpts.connectionRejected ? (
        // A connection that exists but cannot work gets its own message:
        // saying there is none would be untrue, and showing its old path would
        // read as connected but empty.
        <div className="flex flex-1 min-h-0 flex-col items-center justify-center gap-3 px-6 text-center">
          <span className="text-[13px] text-neutral-500">
            {t(
              stateOpts.credentialError
                ? "panel.credentialError"
                : "panel.connectionRejected",
            )}
          </span>
          <button
            type="button"
            onClick={stateOpts.onOpenSettings}
            className="rounded-sm border border-neutral-800 px-3 py-1 text-[12px] text-neutral-400 hover:border-accent-700 hover:text-accent-300"
          >
            {t("panel.openSettings")}
          </button>
        </div>
      ) : (
        <>
          <TabStrip
            pane={pane}
            remote={remote}
            bucket={bucket}
            homeDir={homeDir}
            onSelectTab={(id) => setActiveTab(index, id)}
            onNewTab={onPlusMenu}
            onCloseTab={(id) => closeTab(index, id)}
          />

          {/* One line that never wraps: the path gives up its room first, and
              the controls after it keep theirs. */}
          <div className="flex min-w-0 items-center gap-2 px-2.5 py-2">
            <Breadcrumb
              path={tab.path}
              remote={remote}
              bucket={bucket}
              homeDir={homeDir}
              onNavigate={(p) => navigate(index, p)}
            />
            <SearchBox
              value={tab.filter}
              onChange={(v) => setFilter(index, v)}
              inputRef={searchInputRef}
              onExit={onFilterExit}
              narrow={panelWidth < FILTER_COLLAPSE_W}
              open={filterOpen}
              onOpenChange={setFilterOpen}
            />
            <ViewControl
              view={tab}
              onSetMode={(m) => setViewMode(index, m)}
              onSetSize={(s) => setViewSize(index, s)}
            />
          </div>

          {tab.viewMode === "icons" ? (
        <IconsHeader
          listing={visible}
          sort={tab.sort}
          onSort={(k) => setSort(index, k)}
        />
      ) : (
        <div
          className={`${gridClass} border-y border-neutral-900 px-3 py-[5px] text-[9.5px] font-semibold uppercase tracking-[0.08em] text-neutral-600`}
        >
          <ColHeader
            label={t("panel.col.name")}
            sortKey="name"
            sort={tab.sort}
            onSort={(k) => setSort(index, k)}
          />
          <ColHeader
            label={t("panel.col.size")}
            sortKey="size"
            sort={tab.sort}
            align="right"
            onSort={(k) => setSort(index, k)}
          />
          {showModified && (
            <ColHeader
              label={t("panel.col.modified")}
              sortKey="modified"
              sort={tab.sort}
              onSort={(k) => setSort(index, k)}
            />
          )}
        </div>
      )}

      {/* Clicking the empty space below the list clears the selection — only
         a click on this area itself, not one that came up from a row. */}
      <div
        ref={scrollBodyRef}
        onScroll={onBodyScroll}
        onContextMenu={onBgContext}
        onClick={(e) => {
          if (e.target === e.currentTarget) selectNone();
        }}
        onDragOver={onBodyDragOver}
        onDragLeave={onBodyDragLeave}
        onDrop={onBodyDrop}
        className={
          "flex min-h-0 flex-1 flex-col overflow-y-auto rounded-sm outline outline-2 -outline-offset-2 outline-dashed transition-[outline-color] duration-150 " +
          (showBodyFrame || osBodyFrame
            ? "outline-accent-400"
            : "outline-transparent")
        }
      >
        {tab.viewMode === "icons" ? (
          <IconsBody
            tab={tab}
            side={pane.side}
            entries={visible}
            filterActive={filterActive}
            viewSize={tab.viewSize}
            onItemClick={onRowClick}
            onOpen={handleOpen}
            onContext={onRowContext}
            onRenameCommit={doRename}
            onRenameCancel={() => cancelRename(index)}
            onParent={() => goParent(index)}
            dnd={rowDnd}
            stateOpts={stateOpts}
          />
        ) : (
          <div className="flex flex-col gap-px px-2 py-1">
            <ParentRow
              gridClass={gridClass}
              rowClass={rowClass}
              showModified={showModified}
              cursor={drawnCursor === -1}
              onOpen={() => goParent(index)}
            />
            <ListBody
              tab={tab}
              entries={visible}
              filterActive={filterActive}
              gridClass={gridClass}
              rowClass={rowClass}
              showModified={showModified}
              cursorIndex={drawnCursor}
              onItemClick={onRowClick}
              onOpen={handleOpen}
              onContext={onRowContext}
              onRenameCommit={doRename}
              onRenameCancel={() => cancelRename(index)}
              dnd={rowDnd}
              stateOpts={stateOpts}
            />
          </div>
        )}
          </div>
        </>
      )}
    </section>
  );
}
