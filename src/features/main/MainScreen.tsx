import { useEffect } from "react";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { listen } from "@tauri-apps/api/event";
import {
  PlugsConnectedIcon,
  PlugsIcon,
  BroomIcon,
  EyeIcon,
  EyeSlashIcon,
  GearSixIcon,
  CircleNotchIcon,
  WarningCircleIcon,
} from "@phosphor-icons/react";
import { startPaneTransfer, pasteIntoPane } from "../../state/transferQueueStore";
import { useUiStore } from "../../state/uiStore";
import { usePaneStore, type PaneIndex } from "../../state/paneStore";
import { useConnectionStore } from "../../state/connectionStore";
import { useCleanupStore } from "../../state/cleanupStore";
import { useClipboardStore } from "../../state/clipboardStore";
import { useDialogStore } from "../../state/dialogStore";
import { toOtherPane, toSidebar, backToPaneList } from "../../lib/regions";
import { isConnectionComplete, takeStartupPath } from "../../lib/commands";
import Sidebar from "../sidebar/Sidebar";
import Panel from "../panel/Panel";
import Divider from "./Divider";
import StatusBar from "../statusbar/StatusBar";
import DragBadge from "./DragBadge";
import VolumeCleanupDialog from "./VolumeCleanupDialog";
import { t } from "../../locale/en";
import { keyboardTargets } from "../../lib/keyboardTargets";

/* The main screen: title bar, sidebar, two panes with the divider between
   them, status bar.

   It also owns everything that belongs to the window rather than to one pane —
   the global keys, filling the panes at startup, polling the OS clipboard, and
   refreshing when the window comes back to the front. */

export default function MainScreen() {
  const setScreen = useUiStore((s) => s.setScreen);
  const setSettingsTab = useUiStore((s) => s.setSettingsTab);
  const flipped = useUiStore((s) => s.flipped);
  const splitRatio = useUiStore((s) => s.splitRatio);
  const showHidden = useUiStore((s) => s.showHidden);
  const toggleHidden = useUiStore((s) => s.toggleHidden);

  // ── Connection pill ──
  const connections = useConnectionStore((s) => s.connections);
  const activeConnId = useConnectionStore((s) => s.activeId);
  const activeConn =
    connections.find((c) => c.id === activeConnId) ?? connections[0] ?? null;
  const remoteStatus = usePaneStore((s) => {
    const p = s.panes[0];
    const tab = p.tabs.find((tb) => tb.id === p.activeTabId) ?? p.tabs[0];
    return tab.status;
  });
  // A volume that turned us away cannot be scanned either.
  const remoteRejected = usePaneStore((s) => {
    const p = s.panes[0];
    const tab = p.tabs.find((tb) => tb.id === p.activeTabId) ?? p.tabs[0];
    return tab.status === "error" && tab.error === "credentials";
  });
  const pill: "none" | "connecting" | "connected" | "error" = !activeConn
    ? "none"
    : remoteStatus === "error"
      ? "error"
      : remoteStatus === "idle" || remoteStatus === "loading"
        ? "connecting"
        : "connected";
  const pillVolume = activeConn?.name || activeConn?.bucket || "—";
  const openConnSettings = () => {
    setSettingsTab("connections");
    setScreen("settings");
  };

  // Fills both panes once, at their stored paths. Only a tab that has asked
  // for nothing yet is touched, so coming back from Settings does not refetch
  // anything.
  //
  // The stored paths arrive asynchronously, and filling has to wait for them or
  // they would never be listed at all.
  //
  // Launched on a folder — from the file manager entry — the local pane goes
  // there instead, and the remote one still opens where it was left.
  useEffect(() => {
    const fill = (startupPath: string | null) => {
      const { panes, navigate } = usePaneStore.getState();
      panes.forEach((pane, i) => {
        const tab =
          pane.tabs.find((tb) => tb.id === pane.activeTabId) ?? pane.tabs[0];
        const overridePath =
          startupPath && pane.side === "local" ? startupPath : null;
        if (tab.status === "idle" || overridePath) {
          void navigate(i as PaneIndex, overridePath ?? tab.path);
        }
      });
    };
    void takeStartupPath()
      .catch(() => null)
      .then((startupPath) => {
        if (usePaneStore.persist.hasHydrated()) {
          fill(startupPath);
          return;
        }
        usePaneStore.persist.onFinishHydration(() => fill(startupPath));
      });
  }, []);

  // macOS only: asking to open a folder while the app is already running
  // arrives as an event rather than as a new process, and the local pane
  // follows it.
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    void listen<string>("open-here", (e) => {
      const { panes, navigate } = usePaneStore.getState();
      const i = panes.findIndex((p) => p.side === "local");
      if (i !== -1) void navigate(i as PaneIndex, e.payload);
    }).then((fn) => {
      unlisten = fn;
    });
    return () => unlisten?.();
  }, []);

  // The OS clipboard is polled continuously, not only when the window is
  // focused: a copy made in a file manager may never give this window focus at
  // all, and it would then go unnoticed. Reading the clipboard is local and
  // cheap, and the first read happens immediately rather than a second later.
  useEffect(() => {
    void useClipboardStore.getState().refreshOsClipboard();
    const id = window.setInterval(() => {
      void useClipboardStore.getState().refreshOsClipboard();
    }, 1000);
    return () => window.clearInterval(id);
  }, []);

  useEffect(() => {
    // A quiet refresh: the cursor, the selection, the scroll position and the
    // filter all stay. Opening a file in another application and coming back
    // should not move anything.
    const reloadPanes = (onlyLocal = false) => {
      const { panes, refresh } = usePaneStore.getState();
      panes.forEach((pane, i) => {
        if (onlyLocal && pane.side !== "local") return;
        const tab =
          pane.tabs.find((tb) => tb.id === pane.activeTabId) ?? pane.tabs[0];
        if (tab.status === "ready" || tab.status === "error") {
          void refresh(i as PaneIndex);
        }
      });
    };

    // ── Moving between regions with Tab ──────────────────────────────
    // Tab never does the browser's own focus walk here: it moves between the
    // panes, with shift it enters the sidebar, and with control it cycles the
    // active pane's tabs. From a text field it leaves the field and returns to
    // that pane's list.
    const onTabCapture = (e: KeyboardEvent) => {
      // While a modal is open the keys belong to it.
      if (useDialogStore.getState().dialog || useCleanupStore.getState().open) return;
      // The physical key is what identifies Tab here: with shift held, the
      // webview reports the logical key as unidentified.
      const isTab = e.code === "Tab" || e.key === "Tab";
      if (!isTab || e.altKey || e.metaKey) return;

      // With control, Tab cycles the active pane's own tabs.
      if (e.ctrlKey) {
        const ap = useUiStore.getState().activePane;
        if (ap === null) return; // no pane to cycle from the sidebar
        e.preventDefault();
        usePaneStore.getState().cycleTab(ap, e.shiftKey ? -1 : 1);
        return;
      }

      const back = e.shiftKey;
      e.preventDefault();

      const ae = document.activeElement as HTMLElement | null;
      if (ae && (ae.tagName === "INPUT" || ae.tagName === "TEXTAREA")) {
        const paneEl = ae.closest("[data-pane]");
        const pi =
          paneEl != null
            ? (Number(paneEl.getAttribute("data-pane")) as PaneIndex)
            : null;
        ae.blur(); // a rename field commits as it loses focus
        if (pi != null && ae.dataset.filter != null) {
          usePaneStore.getState().exitFilter(pi);
        }
        if (back) toSidebar();
        else if (pi != null) backToPaneList(pi); // back to this pane's list
        else toOtherPane();
        return;
      }

      if (back) toSidebar();
      else toOtherPane();
    };
    window.addEventListener("keydown", onTabCapture, true);

    const onKey = (e: KeyboardEvent) => {
      if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "r") {
        e.preventDefault();
        reloadPanes(false);
      } else if (e.key === "F5" || e.key === "F6") {
        // These act on the active pane, and the sidebar is not one.
        const ap = useUiStore.getState().activePane;
        if (ap === null) return;
        // The selection, or the row under the keyboard cursor, goes to the
        // other pane; the second key moves it.
        e.preventDefault();
        startPaneTransfer(ap, e.key === "F6", keyboardTargets(ap));
      } else if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "v") {
        // Paste shares its path with the context menu entry. In a text field
        // it must stay ordinary text pasting.
        const ae = document.activeElement as HTMLElement | null;
        if (
          ae &&
          (ae.tagName === "INPUT" ||
            ae.tagName === "TEXTAREA" ||
            ae.isContentEditable)
        ) {
          return;
        }
        const ap = useUiStore.getState().activePane;
        if (ap === null) return;
        e.preventDefault();
        void pasteIntoPane(ap);
      } else if (
        (e.ctrlKey || e.metaKey) &&
        (e.key.toLowerCase() === "c" || e.key.toLowerCase() === "x")
      ) {
        // Copy and cut, sharing their path with the context menu entries.
        const isMove = e.key.toLowerCase() === "x";
        const ae = document.activeElement as HTMLElement | null;
        if (
          ae &&
          (ae.tagName === "INPUT" ||
            ae.tagName === "TEXTAREA" ||
            ae.isContentEditable)
        ) {
          return;
        }
        const ap = useUiStore.getState().activePane;
        if (ap === null) return;
        const pane = usePaneStore.getState().panes[ap];
        const tab = pane.tabs.find((tb) => tb.id === pane.activeTabId) ?? pane.tabs[0];
        // The selection, or the row under the keyboard cursor.
        const items = keyboardTargets(ap);
        if (items.length === 0) return;
        e.preventDefault();
        useClipboardStore.getState().setClipboard({
          items,
          srcSide: pane.side,
          srcPath: tab.path,
          mode: isMove ? "move" : "copy",
          origin: "inApp",
        });
      }
    };
    window.addEventListener("keydown", onKey);

    // Coming back to the window refreshes the local pane only: a remote
    // listing is slow enough that a loading flash on every alt-tab is worse
    // than a slightly stale list.
    let blurred = false;
    let unlisten: (() => void) | undefined;
    // Outside the app there is no window to listen to, and the UI still runs.
    // `getCurrentWindow()` itself throws there, before any promise exists, so
    // the `catch` on the chain alone would not be enough.
    try {
      getCurrentWindow()
        .onFocusChanged(({ payload: focused }) => {
          if (!focused) {
            blurred = true;
          } else if (blurred) {
            blurred = false;
            reloadPanes(true);
            // Whatever was copied while away should be known immediately.
            void useClipboardStore.getState().refreshOsClipboard();
          }
        })
        .then((u) => {
          unlisten = u;
        })
        .catch(() => {});
    } catch {
      /* no window outside the app */
    }

    return () => {
      window.removeEventListener("keydown", onTabCapture, true);
      window.removeEventListener("keydown", onKey);
      unlisten?.();
    };
  }, []);

  return (
    <div className="flex h-full flex-col overflow-hidden bg-bg text-text">
      {/* ── Title bar ── */}
      <div className="flex items-center justify-between border-b border-neutral-900 bg-titlebar px-[14px] py-[9px]">
        <div className="flex items-center gap-2.5">
          <button
            type="button"
            onClick={() => {
              setSettingsTab("about");
              setScreen("settings");
            }}
            className="text-[13px] font-medium tracking-[-0.01em] hover:text-accent-200"
          >
            {t("app.name")}
          </button>
          <button
            type="button"
            onClick={openConnSettings}
            className={
              "flex items-center gap-[5px] rounded-full border px-2 py-0.5 text-[11px] " +
              (pill === "connected"
                ? "border-accent-700 text-accent-300"
                : pill === "error"
                  ? "border-[var(--color-danger)] text-[var(--color-danger)]"
                  : "border-neutral-700 text-neutral-400")
            }
          >
            {pill === "connected" ? (
              <PlugsConnectedIcon size={12} />
            ) : pill === "error" ? (
              <WarningCircleIcon size={12} />
            ) : pill === "connecting" ? (
              <CircleNotchIcon size={12} className="animate-spin" />
            ) : (
              <PlugsIcon size={12} />
            )}
            {pill === "none"
              ? t("main.connection.none")
              : pill === "connecting"
                ? t("main.connection.connecting", { volume: pillVolume })
                : pill === "error"
                  ? t("main.connection.error", { volume: pillVolume })
                  : t("main.connection.connected", { volume: pillVolume })}
          </button>
        </div>
        <div className="flex items-center gap-2">
          <button
            type="button"
            onClick={toggleHidden}
            aria-pressed={showHidden}
            className={
              "flex items-center gap-1.5 rounded-sm border px-2 py-1 text-[11.5px] hover:border-accent-700 hover:text-accent-300 " +
              (showHidden
                ? "border-accent-700 text-accent-300"
                : "border-neutral-800 text-neutral-400")
            }
          >
            {showHidden ? <EyeSlashIcon size={13} /> : <EyeIcon size={13} />}
            {showHidden ? t("main.hideHidden") : t("main.showHidden")}
          </button>
          <button
            type="button"
            disabled={
              activeConn == null ||
              !isConnectionComplete(activeConn) ||
              remoteRejected
            }
            className="flex items-center gap-1.5 rounded-sm border border-neutral-800 px-2 py-1 text-[11.5px] text-neutral-400 hover:border-accent-700 hover:text-accent-300 disabled:cursor-default disabled:opacity-40 disabled:hover:border-neutral-800 disabled:hover:text-neutral-400"
            onClick={() => useCleanupStore.getState().show()}
          >
            <BroomIcon size={13} />
            {t("main.volumeCleanup")}
          </button>
          <button
            type="button"
            className="flex items-center gap-1.5 rounded-sm border border-neutral-800 px-2 py-1 text-[11.5px] text-neutral-400 hover:border-accent-700 hover:text-accent-300"
            onClick={() => setScreen("settings")}
          >
            <GearSixIcon size={13} />
            {t("main.settings")}
          </button>
        </div>
      </div>

      {/* ── The body: sidebar and panes ──
          The row is kept from growing with its content, so the lists inside
          scroll instead of stretching the window. */}
      <div className="grid min-h-0 flex-1 grid-cols-[196px_1fr] grid-rows-[minmax(0,1fr)]">
        {/* The sidebar brings its own outer element. */}
        <Sidebar />

        {/* Two slots with the divider between them. Swapping the panes swaps
            which pane is drawn in which slot; the panes themselves keep their
            order, so everything else can keep addressing them by it. */}
        <div
          className="grid min-h-0 min-w-0 grid-rows-[minmax(0,1fr)]"
          style={{
            gridTemplateColumns: `${splitRatio}fr 30px ${1 - splitRatio}fr`,
          }}
        >
          <Panel index={(flipped ? 1 : 0) as PaneIndex} />
          <Divider />
          <Panel index={(flipped ? 0 : 1) as PaneIndex} />
        </div>
      </div>

      {/* ── Status bar ── */}
      <StatusBar />

      {/* The drag badge, drawn above everything. */}
      <DragBadge />
      <VolumeCleanupDialog />
    </div>
  );
}
