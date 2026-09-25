import { create } from "zustand";
import { persist, createJSONStorage } from "zustand/middleware";
import {
  listLocal,
  listRemote,
  openPath,
  type DirEntry,
  type ListErr,
  type ListErrKind,
} from "../lib/commands";
import {
  DEFAULT_SORT,
  sortEntries,
  visibleEntries,
  type SortKey,
  type SortState,
} from "../lib/entries";
import { persistStorage } from "../lib/persist";
import { useUiStore } from "./uiStore";
import { useToastStore } from "./toastStore";
import { useRecentStore } from "./recentStore";
import { useConnPathStore } from "./connPathStore";
import { useConnectionStore } from "./connectionStore";
import { t } from "../locale/en";

/* The two panes and their tabs: where each one is, what it is showing, and
   everything the user has done inside it.

   Index 0 is the remote pane and 1 the local one, whichever side of the window
   they are drawn on. Each tab carries its own path, listing, filter, sorting,
   selection, cursor and scroll position, so switching tabs returns to exactly
   what was left behind.

   Only the lasting parts are written to disk — where each tab was and how it
   was being viewed. A listing is fetched again on the next run. */

export type PaneSide = "remote" | "local";
export type PaneIndex = 0 | 1;

/** An entry is exactly what the backend returns; an alias, not a second copy
    that could drift from it. */
export type Entry = DirEntry;

/** Where a tab is in its listing cycle. */
export type TabStatus = "idle" | "loading" | "ready" | "error";

/* How a tab draws its list. The setting belongs to the tab rather than to the
   folder: changing it keeps applying as the user moves around, which is what
   having tabs implies. */
export type ViewMode = "details" | "icons";
export type ViewSize = "s" | "m" | "l";
export interface DirView {
  viewMode: ViewMode;
  viewSize: ViewSize;
}
export const DEFAULT_VIEW: DirView = { viewMode: "details", viewSize: "m" };

export interface TabState {
  id: string;
  /** Where this tab is, relative to its pane's root. */
  path: string;
  listing: Entry[];
  /** The live filter; cleared when the folder changes. */
  filter: string;
  /** Sorting, kept across folders: it is a preference, not a property of the
      folder. */
  sort: SortState;
  /** The entry being renamed in place, if any. */
  renaming: string | null;
  /** The selected entries, by name. */
  selection: string[];
  /** Where a shift-range starts from. */
  anchor: string | null;
  /** Whether that range adds to the selection or takes away from it, which
      depends on what the click that set the anchor did. */
  anchorMode: "add" | "sub";
  /** The keyboard cursor, as a position in the visible list; `-1` is the `..`
      row and `-2` hides it. Positions are visible ones, so they follow the
      filter and the sorting rather than the raw listing. */
  cursorIndex: number;
  /** How far the list is scrolled, restored when the tab comes back. */
  scrollTop: number;
  /** Set when a tab is entered for the first time, so that once its listing
      arrives the cursor lands on the first entry instead of on `..`. */
  snapCursor: boolean;
  status: TabStatus;
  /** Why the listing failed, when it did. */
  error: ListErrKind | null;
  /** Rows or tiles. */
  viewMode: ViewMode;
  /** How large those rows or tiles are. */
  viewSize: ViewSize;
}

export interface PaneState {
  side: PaneSide;
  tabs: TabState[];
  activeTabId: string;
}

// ── A pane starts with one empty tab that has not asked for anything yet ──
function freshTab(path: string): TabState {
  return {
    // A random id, because a counter would restart at zero and collide with
    // the ids restored from disk.
    id: crypto.randomUUID(),
    path,
    listing: [],
    filter: "",
    sort: DEFAULT_SORT,
    renaming: null,
    selection: [],
    anchor: null,
    anchorMode: "add",
    cursorIndex: -1,
    scrollTop: 0,
    snapCursor: false,
    status: "idle",
    error: null,
    viewMode: DEFAULT_VIEW.viewMode,
    viewSize: DEFAULT_VIEW.viewSize,
  };
}

function freshPane(side: PaneSide, path: string): PaneState {
  const tab = freshTab(path);
  return { side, tabs: [tab], activeTabId: tab.id };
}

// ── Immutable helpers ───────────────────────────────────────────
type Panes = [PaneState, PaneState];

/** Applies a change to one pane's active tab.

    The untouched pane keeps its identity, so only the pane that actually
    changed is redrawn. */
function patchActiveTab(
  panes: Panes,
  index: PaneIndex,
  patch: (tab: TabState) => TabState,
): Panes {
  const pane = panes[index];
  const tabs = pane.tabs.map((tb) =>
    tb.id === pane.activeTabId ? patch(tb) : tb,
  );
  const nextPane: PaneState = { ...pane, tabs };
  return index === 0 ? [nextPane, panes[1]] : [panes[0], nextPane];
}

/** Patches one tab by id; a tab closed in the meantime is left alone. */
function patchTab(
  panes: Panes,
  index: PaneIndex,
  tabId: string,
  patch: (tab: TabState) => TabState,
): Panes {
  const pane = panes[index];
  if (!pane.tabs.some((tb) => tb.id === tabId)) return panes;
  const tabs = pane.tabs.map((tb) => (tb.id === tabId ? patch(tb) : tb));
  const nextPane: PaneState = { ...pane, tabs };
  return index === 0 ? [nextPane, panes[1]] : [panes[0], nextPane];
}

/** The newest listing request of each tab. A reply that is not the newest one
    is dropped: the user has moved on, in that tab or to another. */
const latestRequest = new Map<string, number>();
let requestCounter = 0;
function beginRequest(tabId: string): () => boolean {
  const n = ++requestCounter;
  latestRequest.set(tabId, n);
  return () => latestRequest.get(tabId) === n;
}

/** Keeps only the selected entries that are on screen. What is selected is
    what an action takes, so an entry the filter or the hidden-files setting
    has taken out of view leaves the selection too. */
function keepVisibleSelection(
  tab: TabState,
  filter: string,
  showHidden: boolean,
): TabState {
  const visible = new Set(
    visibleEntries(tab.listing, filter, showHidden).map((e) => e.name),
  );
  const selection = tab.selection.filter((n) => visible.has(n));
  const anchor = tab.anchor && visible.has(tab.anchor) ? tab.anchor : null;
  return selection.length === tab.selection.length && anchor === tab.anchor
    ? tab
    : { ...tab, selection, anchor };
}

/** Joins a name onto a path, whichever root that path has. */
export function childPath(path: string, name: string): string {
  if (path === "/") return `/${name}`; // the absolute root must not be trimmed away
  const base = path.replace(/\/+$/, "");
  return base === "" ? name : `${base}/${name}`;
}

/** The parent of a path. Each root stops where it should: the volume root has
    nothing above it, the home directory is the top of a `~` path, and an
    absolute local path goes all the way up to its own root. */
function parentPath(path: string, side: PaneSide): string {
  const trimmed = path === "/" ? "/" : path.replace(/\/+$/, "");
  const slash = trimmed.lastIndexOf("/");
  if (side === "remote") return slash <= 0 ? "" : trimmed.slice(0, slash);
  if (trimmed.startsWith("/")) return slash <= 0 ? "/" : trimmed.slice(0, slash);
  // A Windows drive root has nothing above it, and returning it unchanged is
  // what makes the caller treat going up as a no-op.
  if (/^[A-Za-z]:[\\/]?$/.test(trimmed)) return trimmed;
  if (trimmed === "~" || trimmed === "" || slash <= 0) return "~";
  return trimmed.slice(0, slash);
}

/** Reduces whatever a failed call threw to the kind the UI can describe. */
function errKind(e: unknown): ListErrKind {
  if (e && typeof e === "object" && "kind" in e) {
    return (e as ListErr).kind;
  }
  return "unknown";
}

// ── Store ──────────────────────────────────────────────────────────
interface PaneStore {
  panes: Panes;
  /** Goes to a path in the active tab, or in the given one. The listing lands
      in the tab it was asked for, even if another one is in view by then. */
  navigate: (index: PaneIndex, path: string, tabId?: string) => Promise<void>;
  /** Lists the same folder again without disturbing anything: no loading
      flash, and the cursor, selection, scroll, filter and sorting all survive.
      The cursor follows its entry by name, so it stays on the same file even if
      the order changed. */
  refresh: (index: PaneIndex, tabId?: string) => Promise<void>;
  /** Goes up one level; at a root, nothing happens. */
  goParent: (index: PaneIndex) => Promise<void>;
  /** Opens an entry: a folder in the pane, a local file in its default
      application. A remote file is opened from the queue instead, because it
      has to be downloaded first. */
  openEntry: (index: PaneIndex, entry: Entry) => Promise<void>;
  /** Switches to another tab of this pane. */
  setActiveTab: (index: PaneIndex, tabId: string) => void;
  /** Steps to the next or previous tab, wrapping around. A tab already visited
      keeps its cursor; one opened for the first time starts at its first
      entry. */
  cycleTab: (index: PaneIndex, dir: -1 | 1) => void;
  /** Opens a new tab, at the pane's root or at a given path. */
  addTab: (index: PaneIndex, path?: string) => void;
  /** Closes a tab; a pane always keeps at least one. */
  closeTab: (index: PaneIndex, tabId: string) => void;
  /** Replaces the selection and the range anchor. What a particular click or
      command selects is worked out where the visible order is known. */
  setSelection: (
    index: PaneIndex,
    names: string[],
    anchor: string | null,
    anchorMode?: "add" | "sub",
  ) => void;
  /** Sets the filter text; the listing is not fetched again. */
  setFilter: (index: PaneIndex, value: string) => void;
  /** Sorts by a column: the same one again reverses it, a new one starts
      ascending. */
  setSort: (index: PaneIndex, key: SortKey) => void;
  /** Starts renaming an entry in place. */
  beginRename: (index: PaneIndex, name: string) => void;
  /** Ends renaming, finished or abandoned. */
  cancelRename: (index: PaneIndex) => void;
  /** Rows or tiles, for this tab. */
  setViewMode: (index: PaneIndex, mode: ViewMode) => void;
  /** Row or tile size, for this tab. */
  setViewSize: (index: PaneIndex, size: ViewSize) => void;
  /** Puts the keyboard cursor on a row. */
  setCursor: (index: PaneIndex, cursorIndex: number) => void;
  /** Brings the cursor back into the list when the filter field is left with
      the keyboard: onto the selection if it is still visible, otherwise onto
      the first row, and has the pane draw it there. */
  exitFilter: (index: PaneIndex) => void;
  /** Remembers how far this tab's list is scrolled. */
  setScrollTop: (index: PaneIndex, scrollTop: number) => void;
}

// ── Reading the stored panes back ─────────────────────────────
const isViewMode = (v: unknown): v is ViewMode =>
  v === "details" || v === "icons";
const isViewSize = (v: unknown): v is ViewSize =>
  v === "s" || v === "m" || v === "l";
const isSort = (v: unknown): v is SortState =>
  !!v &&
  typeof v === "object" &&
  ["name", "size", "modified"].includes((v as SortState).key) &&
  ["asc", "desc"].includes((v as SortState).dir);

/** Rebuilds full tabs from the thin records on disk; everything that was not
    stored starts empty, and the tabs list themselves once the app is up.

    Every field is checked rather than trusted: a hand-edited or half-written
    store file should open the app at its defaults, not break it. */
function mergePanes(persisted: unknown, current: PaneStore): PaneStore {
  const raw = (persisted as { panes?: unknown } | undefined)?.panes;
  if (!Array.isArray(raw) || raw.length !== 2) return current;

  const rebuilt = raw.map((pp, i): PaneState => {
    const side: PaneSide = i === 0 ? "remote" : "local";
    const root = side === "remote" ? "" : "~";
    const rawTabs: unknown[] = Array.isArray(
      (pp as { tabs?: unknown })?.tabs,
    )
      ? (pp as { tabs: unknown[] }).tabs
      : [];

    let tabs: TabState[] = rawTabs
      .filter(
        (tb): tb is { id: string; path: string } & Record<string, unknown> =>
          !!tb &&
          typeof (tb as { id?: unknown }).id === "string" &&
          typeof (tb as { path?: unknown }).path === "string",
      )
      .map((tb): TabState => ({
        id: tb.id,
        path: tb.path,
        listing: [],
        filter: "",
        sort: isSort(tb.sort) ? tb.sort : DEFAULT_SORT,
        renaming: null,
        selection: [],
        anchor: null,
        anchorMode: "add",
        cursorIndex: -1,
        scrollTop: 0,
        snapCursor: false,
        status: "idle",
        error: null,
        viewMode: isViewMode(tb.viewMode)
          ? tb.viewMode
          : DEFAULT_VIEW.viewMode,
        viewSize: isViewSize(tb.viewSize)
          ? tb.viewSize
          : DEFAULT_VIEW.viewSize,
      }));

    if (tabs.length === 0) tabs = [freshTab(root)];
    const savedActive = (pp as { activeTabId?: unknown })?.activeTabId;
    const activeTabId =
      typeof savedActive === "string" &&
      tabs.some((tb) => tb.id === savedActive)
        ? savedActive
        : tabs[0].id;
    return { side, tabs, activeTabId };
  });

  return { ...current, panes: [rebuilt[0], rebuilt[1]] };
}

export const usePaneStore = create<PaneStore>()(
persist((set, get) => ({
  panes: [freshPane("remote", ""), freshPane("local", "~")],

  navigate: async (index, path, tabId) => {
    // The loading state is written before anything is awaited, so a second
    // call arriving immediately sees it and does not fetch the same folder
    // twice.
    const side = get().panes[index].side;
    const id = tabId ?? get().panes[index].activeTabId;
    const isLatest = beginRequest(id);
    set({
      panes: patchTab(get().panes, index, id, (tb) => ({
        ...tb,
        status: "loading",
        error: null,
        filter: "", // a new folder starts unfiltered
        renaming: null, // and an open rename field belongs to the old one
      })),
    });

    try {
      const listing =
        side === "remote" ? await listRemote(path) : await listLocal(path);
      if (!isLatest()) return;
      // Only a folder that actually opened is worth remembering.
      useRecentStore.getState().visit(side, path);
      // Each connection remembers where it was left, so switching back returns
      // there instead of to the root.
      if (side === "remote") {
        useConnPathStore
          .getState()
          .record(useConnectionStore.getState().activeId, path);
      }
      set({
        panes: patchTab(get().panes, index, id, (tb) => ({
          ...tb,
          path,
          listing,
          selection: [],
          anchor: null,
          // `..` normally, or the first entry for a tab being opened now
          cursorIndex: tb.snapCursor && listing.length > 0 ? 0 : -1,
          snapCursor: false,
          scrollTop: 0, // a new folder starts at the top
          status: "ready",
          error: null,
        })),
      });
    } catch (e) {
      if (!isLatest()) return;
      set({
        panes: patchTab(get().panes, index, id, (tb) => ({
          ...tb,
          // The path that failed is still the path this tab is on. Keeping the
          // previous one draws a breadcrumb of a folder nobody asked for.
          path,
          listing: [],
          selection: [],
          anchor: null,
          cursorIndex: -1,
          snapCursor: false,
          status: "error",
          error: errKind(e),
        })),
      });
    }
  },

  refresh: async (index, tabId) => {
    const pane = get().panes[index];
    const tab = pane.tabs.find((tb) => tb.id === (tabId ?? pane.activeTabId));
    if (!tab || tab.status === "loading") return; // a fetch is already out
    const isLatest = beginRequest(tab.id);
    const side = pane.side;
    const showHidden = useUiStore.getState().showHidden;

    // What to restore afterwards: the cursor's entry by name, the selection,
    // and the scroll position.
    const visBefore = sortEntries(
      visibleEntries(tab.listing, tab.filter, showHidden),
      tab.sort,
    );
    const cursorName =
      tab.cursorIndex >= 0 ? (visBefore[tab.cursorIndex]?.name ?? null) : null;
    const selBefore = tab.selection;
    const scrollBefore = tab.scrollTop;

    try {
      const listing =
        side === "remote"
          ? await listRemote(tab.path)
          : await listLocal(tab.path);
      if (!isLatest()) return;
      const names = sortEntries(
        visibleEntries(listing, tab.filter, showHidden),
        tab.sort,
      ).map((e) => e.name);

      // The cursor follows its entry; if that entry is gone, it stays around
      // the same position in the list.
      let cursorIndex = tab.cursorIndex; // the `..` row and the hidden state stay as they are
      if (cursorName != null) {
        if (names.length === 0) cursorIndex = -1;
        else {
          const at = names.indexOf(cursorName);
          cursorIndex =
            at >= 0 ? at : Math.min(Math.max(0, tab.cursorIndex), names.length - 1);
        }
      }

      set({
        panes: patchTab(get().panes, index, tab.id, (t) => ({
          ...t,
          listing,
          selection: selBefore.filter((n) => names.includes(n)),
          cursorIndex,
          scrollTop: scrollBefore,
          status: "ready",
          error: null,
          // Everything not named here is deliberately left untouched.
        })),
      });
    } catch (e) {
      if (!isLatest()) return;
      set({
        panes: patchTab(get().panes, index, tab.id, (t) => ({
          ...t,
          status: "error",
          error: errKind(e),
        })),
      });
    }
  },

  goParent: async (index) => {
    const pane = get().panes[index];
    const tab =
      pane.tabs.find((tb) => tb.id === pane.activeTabId) ?? pane.tabs[0];
    const parent = parentPath(tab.path, pane.side);
    const here = tab.path === "/" ? "/" : tab.path.replace(/\/+$/, "");
    if (parent === here) return; // already at a root
    await get().navigate(index, parent);
  },

  openEntry: async (index, entry) => {
    const pane = get().panes[index];
    const tab =
      pane.tabs.find((tb) => tb.id === pane.activeTabId) ?? pane.tabs[0];
    const full = childPath(tab.path, entry.name);
    if (entry.kind !== "dir") {
      // A local file goes to the operating system; a remote one is handled by
      // the queue, which downloads it first.
      if (pane.side === "local") {
        try {
          await openPath("local", full);
        } catch {
          useToastStore
            .getState()
            .push(t("open.failed", { name: entry.name }), "error");
        }
      }
      return;
    }
    await get().navigate(index, full);
  },

  setActiveTab: (index, tabId) => {
    const panes = get().panes;
    const pane = panes[index];
    if (pane.activeTabId === tabId) return;
    const nextPane: PaneState = { ...pane, activeTabId: tabId };
    set({ panes: index === 0 ? [nextPane, panes[1]] : [panes[0], nextPane] });
    // A tab restored from disk has no listing yet; entering it fetches one.
    const now = get().panes[index].tabs.find((tb) => tb.id === tabId);
    if (now && now.status === "idle") void get().navigate(index, now.path);
  },

  cycleTab: (index, dir) => {
    const pane = get().panes[index];
    if (pane.tabs.length <= 1) return;
    const pos = pane.tabs.findIndex((tb) => tb.id === pane.activeTabId);
    const target =
      pane.tabs[(pos + dir + pane.tabs.length) % pane.tabs.length];
    const revisit = target.status === "ready";
    get().setActiveTab(index, target.id); // fetches the listing if it has none
    set({
      panes: patchActiveTab(get().panes, index, (tb) =>
        revisit
          ? // Already visited: keep the cursor where it was, unless it was on
            // `..` or hidden.
            tb.cursorIndex < 0 && tb.listing.length > 0
            ? { ...tb, cursorIndex: 0 }
            : tb
          : // First visit: the listing is still on its way.
            { ...tb, snapCursor: true },
      ),
    });
  },

  addTab: (index, path) => {
    const panes = get().panes;
    const pane = panes[index];
    const target = path ?? (pane.side === "remote" ? "" : "~");
    const tab = freshTab(target);
    const nextPane: PaneState = {
      ...pane,
      tabs: [...pane.tabs, tab],
      activeTabId: tab.id,
    };
    set({ panes: index === 0 ? [nextPane, panes[1]] : [panes[0], nextPane] });
    // A new tab fetches its own listing: the startup effect only covers the
    // tabs that exist when the window opens.
    void get().navigate(index, target);
  },

  closeTab: (index, tabId) => {
    const panes = get().panes;
    const pane = panes[index];
    if (pane.tabs.length <= 1) return; // a pane always keeps one tab
    const pos = pane.tabs.findIndex((tb) => tb.id === tabId);
    if (pos === -1) return;
    const tabs = pane.tabs.filter((tb) => tb.id !== tabId);
    let activeTabId = pane.activeTabId;
    if (tabId === pane.activeTabId) {
      // Focus moves to the neighbour on the left, or to the new first tab.
      activeTabId = (tabs[pos - 1] ?? tabs[pos] ?? tabs[0]).id;
    }
    const nextPane: PaneState = { ...pane, tabs, activeTabId };
    set({ panes: index === 0 ? [nextPane, panes[1]] : [panes[0], nextPane] });
    // That neighbour may be one restored from disk with no listing yet.
    const now = get().panes[index].tabs.find((tb) => tb.id === activeTabId);
    if (now && now.status === "idle") void get().navigate(index, now.path);
  },

  setViewMode: (index, mode) => {
    set({
      panes: patchActiveTab(get().panes, index, (tb) => ({
        ...tb,
        viewMode: mode,
      })),
    });
  },

  setViewSize: (index, size) => {
    set({
      panes: patchActiveTab(get().panes, index, (tb) => ({
        ...tb,
        viewSize: size,
      })),
    });
  },

  setCursor: (index, cursorIndex) => {
    set({
      panes: patchActiveTab(get().panes, index, (tb) =>
        tb.cursorIndex === cursorIndex ? tb : { ...tb, cursorIndex },
      ),
    });
  },

  setScrollTop: (index, scrollTop) => {
    set({
      panes: patchActiveTab(get().panes, index, (tb) =>
        tb.scrollTop === scrollTop ? tb : { ...tb, scrollTop },
      ),
    });
  },

  exitFilter: (index) => {
    const pane = get().panes[index];
    const tab = pane.tabs.find((tb) => tb.id === pane.activeTabId);
    if (!tab) return;
    // The visible list is worked out the same way the panel works it out, and
    // from the filter as it stands right now — pressing Escape clears it before
    // this runs.
    const vis = sortEntries(
      visibleEntries(tab.listing, tab.filter, useUiStore.getState().showHidden),
      tab.sort,
    );
    if (vis.length === 0) {
      get().setCursor(index, -2);
      return;
    }
    const selIdx = vis.findIndex((e) => tab.selection.includes(e.name));
    get().setCursor(index, selIdx >= 0 ? selIdx : 0);
    // The filter is left from the keyboard, so the cursor is drawn where it
    // lands: Enter right after opens what the user sees.
    useUiStore.getState().revealCursor(index);
  },

  setSelection: (index, names, anchor, anchorMode = "add") => {
    // The cursor is not touched here: it is a position in the visible list and
    // is set by whoever knows that list, alongside this call.
    set({
      panes: patchActiveTab(get().panes, index, (tb) => ({
        ...tb,
        selection: names,
        anchor,
        anchorMode,
      })),
    });
  },

  setFilter: (index, value) => {
    const showHidden = useUiStore.getState().showHidden;
    set({
      panes: patchActiveTab(get().panes, index, (tb) =>
        keepVisibleSelection({ ...tb, filter: value }, value, showHidden),
      ),
    });
  },

  beginRename: (index, name) => {
    set({
      panes: patchActiveTab(get().panes, index, (tb) => ({
        ...tb,
        renaming: name,
      })),
    });
  },

  cancelRename: (index) => {
    set({
      panes: patchActiveTab(get().panes, index, (tb) =>
        tb.renaming === null ? tb : { ...tb, renaming: null },
      ),
    });
  },

  setSort: (index, key) => {
    set({
      panes: patchActiveTab(get().panes, index, (tb) => ({
        ...tb,
        sort:
          tb.sort.key === key
            ? { key, dir: tb.sort.dir === "asc" ? "desc" : "asc" }
            : { key, dir: "asc" },
      })),
    });
  },
}), {
  name: "panes",
  storage: createJSONStorage(() => persistStorage),
  // Only what should survive a restart: where each tab was and how it was
  // being viewed.
  partialize: (s) => ({
    panes: s.panes.map((pane) => ({
      activeTabId: pane.activeTabId,
      tabs: pane.tabs.map((tb) => ({
        id: tb.id,
        path: tb.path,
        viewMode: tb.viewMode,
        viewSize: tb.viewSize,
        sort: tb.sort,
      })),
    })),
  }),
  merge: (persisted, current) => mergePanes(persisted, current as PaneStore),
}));

/** The volume the remote pane is reading: the live connection and where it points. */
function liveVolume(s: ReturnType<typeof useConnectionStore.getState>): string {
  const c = s.connections.find((x) => x.id === s.activeId);
  return c ? `${c.id}\n${c.endpoint}\n${c.region}\n${c.bucket}` : `${s.activeId}`;
}

// When the live volume changes, the remote tabs out of view drop their listing
// and fetch a fresh one when entered. The tab in view is listed again by
// whoever made the change. A listing kept from the old volume would show its
// files while delete and rename act on the new one.
useConnectionStore.subscribe((s, prev) => {
  if (!prev.loaded || liveVolume(s) === liveVolume(prev)) return;
  usePaneStore.setState((st) => ({
    panes: st.panes.map((pane) =>
      pane.side !== "remote"
        ? pane
        : {
            ...pane,
            tabs: pane.tabs.map((tb) =>
              tb.id === pane.activeTabId
                ? tb
                : {
                    ...tb,
                    listing: [],
                    selection: [],
                    anchor: null,
                    cursorIndex: -1,
                    renaming: null,
                    status: "idle" as const,
                    error: null,
                  },
            ),
          },
    ) as Panes,
  }));
});

// Hiding dotfiles takes them out of every tab's selection, as the filter does.
useUiStore.subscribe((s, prev) => {
  if (s.showHidden || !prev.showHidden) return;
  usePaneStore.setState((st) => ({
    panes: st.panes.map((pane) => ({
      ...pane,
      tabs: pane.tabs.map((tb) => keepVisibleSelection(tb, tb.filter, false)),
    })) as Panes,
  }));
});
