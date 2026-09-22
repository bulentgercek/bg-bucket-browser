import { create } from "zustand";
import { listen } from "@tauri-apps/api/event";
import {
  transferStart,
  transferZipStart,
  openRemoteStart,
  listRemote,
  listLocal,
  type TransferKind,
  type ConflictPolicy,
} from "../lib/commands";
import {
  usePaneStore,
  childPath,
  type Entry,
  type PaneSide,
} from "./paneStore";
import { useConnectionStore } from "./connectionStore";
import { useClipboardStore } from "./clipboardStore";
import { useToastStore } from "./toastStore";
import { useDialogStore, type ConflictAction } from "./dialogStore";
import { groupPathsByDir } from "../lib/osFiles";
import { addSample, type RateSample } from "../lib/rate";
import { t } from "../locale/en";

// The transfer queue: Rust events feed it, the status bar and the queue panel read it.

export type TransferStatus =
  | "queued"
  | "active"
  | "done"
  | "error"
  | "canceled";

export interface Transfer {
  id: string;
  kind: TransferKind;
  name: string;
  /** Source label shown in the queue row: "OS", "Local" or "Remote". */
  from: string;
  /** Destination label: "Local", "Remote" or "Open". */
  to: string;
  /** Move: the source pane is refreshed too when the job finishes. */
  move: boolean;
  /** Connection that was active when the job started; missing for local↔local jobs. */
  connId?: string;
  connName?: string;
  bytesTotal: number;
  bytesDone: number;
  /** Recent progress reports, for the speed (`rate.ts`). */
  samples: RateSample[];
  status: TransferStatus;
  detail?: string;
  /** Relative path of the file being sent right now; only set for folder transfers. */
  currentFile?: string;
  /** Set while a chunk is being retried; cleared as soon as it arrives. */
  retry?: { attempt: number; of: number };
}

interface TransferState {
  transfers: Transfer[];
  add: (
    t: Omit<Transfer, "bytesTotal" | "bytesDone" | "samples" | "status">,
  ) => void;
  progress: (id: string, bytesDone: number, bytesTotal: number) => void;
  finish: (id: string, status: TransferStatus, detail?: string) => void;
  /** Called when a folder transfer moves on to the next file. */
  setCurrentFile: (id: string, name: string) => void;
  /** Called while a chunk is being retried; `attempt` 0 clears the notice. */
  setRetry: (id: string, attempt: number, of: number) => void;
  remove: (id: string) => void;
  clearFinished: () => void;
}

/* Jobs that reported their end before the queue learned their id.

   Starting a job is a round trip: the command answers with the id, and only
   then does the row exist. A job can be over before that answer arrives — a
   local move of one file is a rename — and its end event then finds nothing to
   mark. It waits here instead, and is applied the moment the row appears.

   Entries are dropped after a while so a job whose row never arrives (a failed
   start) cannot keep one forever. */
const settledEarly = new Map<string, { status: TransferStatus; detail?: string }>();
const EARLY_TTL_MS = 30_000;

export const useTransferStore = create<TransferState>((set) => ({
  transfers: [],
  add: (t) =>
    set((s) => {
      const early = settledEarly.get(t.id);
      settledEarly.delete(t.id);
      return {
        transfers: [
          ...s.transfers,
          {
            ...t,
            bytesTotal: 0,
            bytesDone: 0,
            samples: [],
            status: (early?.status ?? "queued") as TransferStatus,
            detail: early?.detail,
          },
        ],
      };
    }),
  progress: (id, bytesDone, bytesTotal) =>
    set((s) => ({
      transfers: s.transfers.map((t) =>
        t.id === id
          ? {
              ...t,
              bytesDone,
              bytesTotal,
              samples: addSample(t.samples, { at: performance.now(), bytes: bytesDone }),
              status: "active",
            }
          : t,
      ),
    })),
  setCurrentFile: (id, name) =>
    set((s) => ({
      transfers: s.transfers.map((t) =>
        t.id === id ? { ...t, currentFile: name } : t,
      ),
    })),
  setRetry: (id, attempt, of) =>
    set((s) => ({
      transfers: s.transfers.map((t) =>
        t.id === id ? { ...t, retry: attempt > 0 ? { attempt, of } : undefined } : t,
      ),
    })),
  // Finishing a job drops the previously finished ones: only live jobs and this one stay.
  finish: (id, status, detail) =>
    set((s) => {
      if (!s.transfers.some((t) => t.id === id)) {
        settledEarly.set(id, { status, detail });
        setTimeout(() => settledEarly.delete(id), EARLY_TTL_MS);
        return s;
      }
      return {
        transfers: s.transfers
          .filter(
            (t) => t.id === id || t.status === "queued" || t.status === "active",
          )
          .map((t) => (t.id === id ? { ...t, status, samples: [], detail } : t)),
      };
    }),
  remove: (id) =>
    set((s) => ({ transfers: s.transfers.filter((t) => t.id !== id) })),
  clearFinished: () =>
    set((s) => ({
      transfers: s.transfers.filter(
        (t) => t.status === "queued" || t.status === "active",
      ),
    })),
}));

// Every transfer the user can start — F5/F6, drag and drop, Paste — goes through runTransfer below.
export interface RunTransferArgs {
  items: Entry[];
  srcSide: PaneSide;
  /** Source directory, relative to the pane root (`""` / `"a/b"` remote, `"~"` / `"~/x"` local). */
  srcPath: string;
  destSide: PaneSide;
  /** Destination directory, same shape. */
  destPath: string;
  mode: "copy" | "move";
  /** Source label for the queue row; derived from `srcSide` when missing, `"OS"` for OS drops. */
  fromLabel?: string;
}

/** Pane side as it appears in the queue. */
export function sideLabel(side: PaneSide): string {
  return side === "remote" ? t("route.remote") : t("route.local");
}

/** Id and name of the active connection, or nothing when the user has none. */
function activeConn(): { connId: string; connName: string } | undefined {
  const { activeId, connections } = useConnectionStore.getState();
  if (!activeId) return undefined;
  const c = connections.find((x) => x.id === activeId);
  return { connId: activeId, connName: c?.name || c?.bucket || activeId };
}

function pickKind(
  srcSide: PaneSide,
  destSide: PaneSide,
  move: boolean,
): TransferKind {
  if (srcSide === "remote" && destSide === "local") return "download";
  if (srcSide === "local" && destSide === "remote") return "upload";
  if (srcSide === "local") return move ? "localMove" : "localCopy";
  return move ? "remoteMove" : "remoteCopy";
}

/** Free name for a copy: `foo.txt` → `foo copy.txt` → `foo copy 2.txt`; dotfiles stay whole. */
function freeName(name: string, taken: Set<string>): string {
  if (!taken.has(name)) return name;
  const dot = name.lastIndexOf(".");
  const base = dot > 0 ? name.slice(0, dot) : name;
  const ext = dot > 0 ? name.slice(dot) : "";
  const stem = base.replace(/ copy(?: \d+)?$/, "");
  let cand = `${stem} copy${ext}`;
  let n = 1;
  while (taken.has(cand)) {
    n += 1;
    cand = `${stem} copy ${n}${ext}`;
  }
  return cand;
}

/** Asks the user about each colliding name; `null` means they canceled the whole transfer. */
async function resolveConflicts(
  collisions: Entry[],
  destLabel: string,
  paneIndex: 0 | 1,
): Promise<Map<string, ConflictAction> | null> {
  const out = new Map<string, ConflictAction>();
  for (let i = 0; i < collisions.length; i += 1) {
    const { action, applyToAll } = await new Promise<{
      action: ConflictAction;
      applyToAll: boolean;
    }>((res) => {
      useDialogStore.getState().show({
        kind: "conflict",
        paneIndex,
        name: collisions[i].name,
        destLabel,
        remaining: collisions.length - i,
        onResolve: (a, all) => res({ action: a, applyToAll: all }),
      });
    });
    if (action === "cancel") return null;
    if (applyToAll) {
      for (let j = i; j < collisions.length; j += 1) {
        out.set(collisions[j].name, action);
      }
      break;
    }
    out.set(collisions[i].name, action);
  }
  return out;
}

export async function runTransfer(args: RunTransferArgs): Promise<void> {
  const { items, srcSide, srcPath, destSide, destPath, mode } = args;
  const from = args.fromLabel ?? sideLabel(srcSide);
  const to = sideLabel(destSide);
  if (items.length === 0) {
    useToastStore.getState().push(t("transfer.nothingToTransfer"), "error");
    return;
  }
  const move = mode === "move";
  const sameDir = srcSide === destSide && srcPath === destPath;
  if (sameDir && move) return; // moving into the same directory changes nothing

  // A folder cannot be copied or moved into itself.
  if (srcSide === destSide) {
    const nested = items.find((e) => {
      if (e.kind !== "dir") return false;
      const p = childPath(srcPath, e.name);
      return destPath === p || destPath.startsWith(`${p}/`);
    });
    if (nested) {
      useToastStore
        .getState()
        .push(t("transfer.intoItself", { name: nested.name }), "error");
      return;
    }
  }

  const kind = pickKind(srcSide, destSide, move);

  // Names already in the destination, from a fresh listing plus what the pane currently shows.
  const taken = new Set<string>();
  try {
    const dest =
      destSide === "remote"
        ? await listRemote(destPath)
        : await listLocal(destPath);
    for (const e of dest) taken.add(e.name);
  } catch {
    /* destination could not be listed; the conflict check falls back to the pane */
  }
  for (const pane of usePaneStore.getState().panes) {
    if (pane.side !== destSide) continue;
    const tb =
      pane.tabs.find((x) => x.id === pane.activeTabId) ?? pane.tabs[0];
    if (tb.path === destPath) for (const e of tb.listing) taken.add(e.name);
  }

  type Planned = { item: Entry; destName: string; conflict: ConflictPolicy };
  let plan: Planned[];

  if (sameDir) {
    // Copying into the same directory renames silently, the way a file manager does.
    const acc = new Set(taken);
    plan = items.map((item) => {
      const destName = freeName(item.name, acc);
      acc.add(destName);
      return { item, destName, conflict: "rename" as const };
    });
  } else {
    const collisions = items.filter((e) => taken.has(e.name));
    if (collisions.length === 0) {
      plan = items.map((item) => ({
        item,
        destName: item.name,
        conflict: "skip" as const,
      }));
    } else {
      const paneIndex: 0 | 1 = destSide === "remote" ? 0 : 1;
      const label =
        lastSegment(destPath) ||
        (destSide === "remote" ? useConnectionStore.getState().bucket : "~");
      const resolved = await resolveConflicts(collisions, label, paneIndex);
      if (!resolved) return; // user canceled
      const acc = new Set(taken);
      plan = [];
      for (const item of items) {
        const action = resolved.get(item.name); // missing = this name had no conflict
        if (action === "skip") continue;
        if (action === "rename") {
          const destName = freeName(item.name, acc);
          acc.add(destName);
          plan.push({ item, destName, conflict: "rename" });
        } else {
          plan.push({
            item,
            destName: item.name,
            conflict: action === "overwrite" ? "overwrite" : "skip",
          });
        }
      }
    }
  }

  if (plan.length === 0) return;
  // Jobs that touch the remote carry the connection they were started with.
  const conn =
    srcSide === "remote" || destSide === "remote" ? activeConn() : undefined;
  for (const { item, destName, conflict } of plan) {
    void transferStart(
      kind,
      childPath(srcPath, item.name),
      destPath,
      destName,
      item.kind === "dir",
      move,
      conflict,
    ).then((id) =>
      addStarted({ id, kind, name: destName, from, to, move, ...conn }),
    );
  }
}

/** F5 and F6: sends the selection of one pane to the other pane's directory.
 *  The keyboard passes `items` itself (the selection, or the row under its
 *  cursor); without them, the selection goes. */
export function startPaneTransfer(
  sourceIndex: 0 | 1,
  move: boolean,
  items?: Entry[],
): void {
  const { panes } = usePaneStore.getState();
  const src = panes[sourceIndex];
  const srcTab =
    src.tabs.find((tb) => tb.id === src.activeTabId) ?? src.tabs[0];
  const other = panes[sourceIndex === 0 ? 1 : 0];
  const otherTab =
    other.tabs.find((tb) => tb.id === other.activeTabId) ?? other.tabs[0];

  void runTransfer({
    items:
      items ?? srcTab.listing.filter((e) => srcTab.selection.includes(e.name)),
    srcSide: src.side,
    srcPath: srcTab.path,
    destSide: other.side,
    destPath: otherTab.path,
    mode: move ? "move" : "copy",
  });
}

/** Paste into a pane; the context menu and Ctrl+V both end up here. */
export async function pasteIntoPane(
  index: 0 | 1,
  destPath?: string,
): Promise<void> {
  const pane = usePaneStore.getState().panes[index];
  const tab = pane.tabs.find((tb) => tb.id === pane.activeTabId) ?? pane.tabs[0];
  const dest = destPath ?? tab.path;

  const cb = useClipboardStore.getState().clipboard;
  if (cb) {
    void runTransfer({
      items: cb.items,
      srcSide: cb.srcSide,
      srcPath: cb.srcPath,
      destSide: pane.side,
      destPath: dest,
      mode: cb.mode,
    });
    if (cb.mode === "move") useClipboardStore.getState().clearClipboard();
    return;
  }

  const tag = useClipboardStore.getState().osClipboardTag;
  if (!tag) return;
  try {
    const groups = await groupPathsByDir(tag.paths);
    if (groups.size === 0) {
      // Nothing could be read from the pasted paths; say so instead of doing nothing.
      useToastStore.getState().push(t("transfer.nothingToTransfer"), "error");
      return;
    }
    for (const [dir, items] of groups) {
      void runTransfer({
        items,
        srcSide: "local",
        srcPath: dir,
        destSide: pane.side,
        destPath: dest,
        mode: tag.mode,
        fromLabel: t("route.os"),
      });
    }
    if (tag.mode === "move") useClipboardStore.getState().clearOsClipboardTag();
  } catch {
    useToastStore.getState().push(t("osdrop.failed"), "error");
  }
}

/** Download as Zip: the remote selection becomes one archive in the other pane. */
export function startPaneZip(sourceIndex: 0 | 1): void {
  const { panes } = usePaneStore.getState();
  const src = panes[sourceIndex];
  if (src.side !== "remote") {
    useToastStore.getState().push(t("transfer.zipRemoteOnly"), "error");
    return;
  }
  const srcTab =
    src.tabs.find((tb) => tb.id === src.activeTabId) ?? src.tabs[0];
  const other = panes[sourceIndex === 0 ? 1 : 0];
  const otherTab =
    other.tabs.find((tb) => tb.id === other.activeTabId) ?? other.tabs[0];

  const items = srcTab.listing.filter((e) =>
    srcTab.selection.includes(e.name),
  );
  if (items.length === 0) {
    useToastStore.getState().push(t("transfer.nothingToTransfer"), "error");
    return;
  }

  const srcs = items.map((e) => ({
    path: childPath(srcTab.path, e.name),
    isDir: e.kind === "dir",
  }));
  // Archive name: one file gives its stem, one folder its name, a multi-selection its directory.
  const stripExt = (s: string) => s.replace(/\.[^.]+$/, "") || s;
  const base =
    items.length === 1
      ? items[0].kind === "dir"
        ? items[0].name
        : stripExt(items[0].name)
      : lastSegment(srcTab.path) || "archive";

  const conn = activeConn(); // the guard above makes this a remote source
  void transferZipStart(srcs, otherTab.path, base).then((id) =>
    addStarted({
      id,
      kind: "downloadZip",
      name: `${base}.zip`,
      from: t("route.remote"),
      to: t("route.local"),
      move: false,
      ...conn,
    }),
  );
}

/** Opens a remote file in the OS default app by downloading it to the cache first. */
export function startOpenRemote(entry: Entry, srcPath: string): void {
  if (entry.kind === "dir") return; // a remote folder has nothing to open
  const conn = activeConn();
  void openRemoteStart(childPath(srcPath, entry.name), entry.name).then((id) =>
    addStarted({
      id,
      kind: "openDownload",
      name: entry.name,
      from: t("route.remote"),
      to: t("route.open"),
      move: false,
      ...conn,
    }),
  );
}

function lastSegment(path: string): string {
  const parts = path.split("/").filter(Boolean);
  return parts[parts.length - 1] ?? "";
}

/** Source and destination side of a job kind; decides which panes refresh when it ends. */
function sidesOf(kind: TransferKind): { src: PaneSide; dest: PaneSide } {
  switch (kind) {
    case "download":
    case "downloadZip":
    case "openDownload":
      return { src: "remote", dest: "local" };
    case "upload":
      return { src: "local", dest: "remote" };
    case "localCopy":
    case "localMove":
      return { src: "local", dest: "local" };
    case "remoteCopy":
    case "remoteMove":
      return { src: "remote", dest: "remote" };
  }
}

/* Refreshes the panes showing a side that a finished job touched. */
function refreshSide(side: PaneSide): void {
  const { panes, navigate } = usePaneStore.getState();
  panes.forEach((pane, i) => {
    if (pane.side !== side) return;
    const tab = pane.tabs.find((tb) => tb.id === pane.activeTabId) ?? pane.tabs[0];
    if (tab.status === "ready" || tab.status === "error") {
      void navigate(i as 0 | 1, tab.path);
    }
  });
}

/* Brings the panes up to date with what a finished job changed.
 *
 * Reached from the end event, and from the queue itself when the job was
 * already over by the time its row appeared.
 */
function refreshAfter(tr: Transfer): void {
  // Opening a remote file only wrote to the cache; no pane changed.
  if (tr.kind === "openDownload") return;
  const { src, dest } = sidesOf(tr.kind);
  refreshSide(dest);
  if (tr.move && src !== dest) refreshSide(src); // a move also emptied the source
}

/* Puts a just-started job in the queue.
 *
 * When the job was already over before its id came back, the row appears
 * finished and nothing else will refresh the panes: the end event arrived
 * while there was nothing to act on.
 */
function addStarted(
  t: Omit<Transfer, "bytesTotal" | "bytesDone" | "samples" | "status">,
): void {
  useTransferStore.getState().add(t);
  const row = useTransferStore.getState().transfers.find((x) => x.id === t.id);
  if (row?.status === "done") refreshAfter(row);
}

/** Wires the Rust events to the store; runs once at startup. */
let wired = false;
export async function initTransferEvents(): Promise<void> {
  if (wired) return;
  wired = true;
  const s = useTransferStore.getState();

  // The event also carries the backend's own speed figure; the queue works the
  // speed out itself (`rate.ts`).
  await listen<{ id: string; bytesDone: number; bytesTotal: number }>(
    "transfer-progress",
    (e) => {
      s.progress(e.payload.id, e.payload.bytesDone, e.payload.bytesTotal);
    },
  );

  await listen<{ id: string; name: string }>("transfer-file", (e) => {
    s.setCurrentFile(e.payload.id, e.payload.name);
  });

  // A chunk is being retried; the queue row says "Retrying 2/4…".
  await listen<{ id: string; attempt: number; of: number }>("transfer-retry", (e) => {
    s.setRetry(e.payload.id, e.payload.attempt, e.payload.of);
  });

  await listen<{ id: string }>("transfer-done", (e) => {
    const tr = useTransferStore
      .getState()
      .transfers.find((x) => x.id === e.payload.id);
    s.finish(e.payload.id, "done");
    if (tr) refreshAfter(tr);
  });
  await listen<{ id: string; detail: string }>("transfer-error", (e) => {
    const tr = useTransferStore
      .getState()
      .transfers.find((x) => x.id === e.payload.id);
    s.finish(e.payload.id, "error", e.payload.detail);
    // The error also becomes a toast, because the queue pill disappears with the last live job.
    useToastStore
      .getState()
      .push(tr ? `${tr.name}: ${e.payload.detail}` : e.payload.detail, "error");
  });
  await listen<{ id: string }>("transfer-canceled", (e) => {
    s.finish(e.payload.id, "canceled");
  });
}
