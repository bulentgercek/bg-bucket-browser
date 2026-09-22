import {
  useEffect,
  useMemo,
  useRef,
  useState,
  type DragEvent as ReactDragEvent,
  type MouseEvent as ReactMouseEvent,
  type ReactNode,
} from "react";
import {
  CaretDownIcon,
  CloudIcon,
  DesktopTowerIcon,
  FolderPlusIcon,
  PushPinSlashIcon,
} from "@phosphor-icons/react";
import { usePinStore, type Pin } from "../../state/pinStore";
import { usePaneStore, type PaneIndex } from "../../state/paneStore";
import { useDeviceStore } from "../../state/deviceStore";
import { useConnPathStore } from "../../state/connPathStore";
import { useRecentStore } from "../../state/recentStore";
import { useUiStore } from "../../state/uiStore";
import { useConnectionStore } from "../../state/connectionStore";
import type { ConnectionInfo } from "../../lib/commands";
import { useDialogStore } from "../../state/dialogStore";
import { backToPaneList } from "../../lib/regions";
import { useContextMenu } from "../../state/contextMenuStore";
import { t } from "../../locale/en";

/* The sidebar: the connection in use, the local disks, the pinned places and
   where the user has been.

   A row opens in the pane its own side belongs to, whichever side of the window
   that pane is drawn on. Holding shift turns every row into "open in a new
   tab", by click or by keyboard.

   A pin's icon says what kind of place it is, not which pane it opens in. */

function Section({
  title,
  legend,
  children,
}: {
  title: string;
  legend?: ReactNode;
  children: ReactNode;
}) {
  return (
    <div className="flex flex-col gap-0.5">
      <div className="flex items-baseline justify-between px-1 pb-1">
        <span className="text-[9.5px] font-semibold uppercase tracking-[0.09em] text-neutral-600">
          {title}
        </span>
        {legend}
      </div>
      {children}
    </div>
  );
}

/* The shared row. The new-tab control is a button of its own, so the row wraps
   a button rather than being one. */
function Row({
  icon,
  className = "",
  cursor = false,
  newTabHint = false,
  shiftHeld = false,
  onClick,
  onNewTab,
  children,
}: {
  icon?: ReactNode;
  className?: string;
  /** Whether the keyboard cursor is on this row. */
  cursor?: boolean;
  /** Show the new-tab control because shift is held on this row. */
  newTabHint?: boolean;
  /** Whether shift is held at all, for the hovered row. */
  shiftHeld?: boolean;
  onClick?: () => void;
  /** Without this the row has no new-tab control. */
  onNewTab?: () => void;
  children: ReactNode;
}) {
  return (
    <div
      className={
        "group relative flex items-center rounded-sm hover:bg-neutral-900 " +
        (cursor
          ? "outline outline-1 -outline-offset-1 outline-accent-400 "
          : "") +
        className
      }
    >
      <button
        type="button"
        data-sbcursor={cursor || undefined}
        onClick={(e) =>
          e.shiftKey && onNewTab ? onNewTab() : onClick?.()
        }
        className="flex min-w-0 flex-1 items-center gap-[7px] px-[7px] py-[5px] text-left text-[12.5px]"
      >
        {icon}
        <span className="min-w-0 truncate">{children}</span>
      </button>
      {onNewTab && shiftHeld && (
        <button
          type="button"
          onClick={onNewTab}
          aria-label={t("main.sidebar.pinNewTab")}
          className={
            "absolute right-1 top-1/2 z-10 -translate-y-1/2 rounded-[3px] p-[3px] text-neutral-400 hover:bg-accent-900 hover:text-accent-200 " +
            (newTabHint
              ? "flex bg-accent-900 text-accent-200"
              : "hidden group-hover:flex")
          }
        >
          <FolderPlusIcon size={13} />
        </button>
      )}
    </div>
  );
}

/* A pinned row, which can also be dragged to reorder the pins. */
function PinRow({
  pin,
  index,
  shiftHeld,
  dragging,
  cursor = false,
  onDragStart,
  onDragOver,
  onDrop,
  onDragEnd,
  onOpen,
  onOpenNewTab,
  onContext,
}: {
  pin: Pin;
  index: number;
  shiftHeld: boolean;
  dragging: boolean;
  cursor?: boolean;
  onDragStart: (index: number) => void;
  onDragOver: (e: ReactDragEvent<HTMLDivElement>, index: number) => void;
  onDrop: () => void;
  onDragEnd: () => void;
  onOpen: () => void;
  onOpenNewTab: () => void;
  onContext: (e: ReactMouseEvent) => void;
}) {
  return (
    <div
      data-sbcursor={cursor || undefined}
      draggable
      onDragStart={(e) => {
        e.dataTransfer.effectAllowed = "move";
        e.dataTransfer.setData("text/plain", pin.id);
        onDragStart(index);
      }}
      onDragOver={(e) => {
        e.preventDefault();
        e.dataTransfer.dropEffect = "move";
        onDragOver(e, index);
      }}
      onDrop={(e) => {
        e.preventDefault();
        onDrop();
      }}
      onDragEnd={onDragEnd}
      onContextMenu={onContext}
      className={
        "group relative flex items-center rounded-sm text-neutral-300 hover:bg-neutral-900 " +
        (cursor
          ? "outline outline-1 -outline-offset-1 outline-accent-400 "
          : "") +
        (dragging ? "opacity-40" : "")
      }
    >
      <button
        type="button"
        onClick={(e) => (e.shiftKey ? onOpenNewTab() : onOpen())}
        className="flex min-w-0 flex-1 items-center gap-[7px] px-[7px] py-[5px] text-left text-[12.5px]"
      >
        {pin.side === "remote" ? (
          <CloudIcon size={14} className="shrink-0 text-accent-300" />
        ) : (
          <DesktopTowerIcon size={14} className="shrink-0 text-local-300" />
        )}
        <span className="min-w-0 truncate">{pin.label}</span>
      </button>
      {shiftHeld && (
        <button
          type="button"
          onClick={onOpenNewTab}
          aria-label={t("main.sidebar.pinNewTab")}
          className={
            "absolute right-1 top-1/2 z-10 -translate-y-1/2 rounded-[3px] p-[3px] text-neutral-400 hover:bg-accent-900 hover:text-accent-200 " +
            // Always visible under the keyboard cursor, on hover otherwise
            (cursor
              ? "flex bg-accent-900 text-accent-200"
              : "hidden group-hover:flex")
          }
        >
          <FolderPlusIcon size={13} />
        </button>
      )}
    </div>
  );
}

export default function Sidebar() {
  const pins = usePinStore((s) => s.pins);
  const removePin = usePinStore((s) => s.removePin);
  const movePin = usePinStore((s) => s.movePin);
  const devices = useDeviceStore((s) => s.devices);
  const recent = useRecentStore((s) => s.items);
  const connections = useConnectionStore((s) => s.connections);
  const activeConnId = useConnectionStore((s) => s.activeId);
  // Remote pins belong to a connection and are shown only on theirs; local
  // pins are always shown.
  const visiblePins = useMemo(
    () => pins.filter((p) => p.side === "local" || p.connId === activeConnId),
    [pins, activeConnId],
  );
  const setActiveConn = useConnectionStore((s) => s.setActive);
  const navigate = usePaneStore((s) => s.navigate);
  const addTab = usePaneStore((s) => s.addTab);
  const setActivePane = useUiStore((s) => s.setActivePane);
  const activePane = useUiStore((s) => s.activePane);
  const sidebarCursor = useUiStore((s) => s.sidebarCursor);
  const setSidebarCursor = useUiStore((s) => s.setSidebarCursor);
  const openMenu = useContextMenu((s) => s.openMenu);
  const showDialog = useDialogStore((s) => s.show);

  const paneOf2 = (pin: Pin): PaneIndex => (pin.side === "remote" ? 0 : 1);

  // Choosing the connection that is already live just goes to its root;
  // choosing another one asks first. Switching volumes is not the same kind of
  // act as opening a folder, and it should not happen silently.
  const switchTo = (c: ConnectionInfo) => {
    if (c.id === activeConnId) {
      void navigate(0, "");
      return;
    }
    showDialog({
      kind: "confirm",
      paneIndex: 0,
      title: t("dialog.switchConnTitle"),
      body: t("dialog.switchConnBody", {
        name: c.name || c.bucket || c.id,
        bucket: c.bucket || "—",
      }),
      confirmLabel: t("dialog.switchConnConfirm"),
      onConfirm: () => {
        void (async () => {
          await setActiveConn(c.id);
          // Back to where this connection was left, not to its root.
          void navigate(0, useConnPathStore.getState().pathFor(c.id));
        })();
      },
    });
  };

  // Whether the sidebar is the active region.
  const sbActive = activePane === null;

  // The connections are one row that opens a menu, rather than a row each:
  // only one of them is live at a time.
  const activeConn =
    connections.find((c) => c.id === activeConnId) ?? connections[0] ?? null;
  const [connMenuOpen, setConnMenuOpen] = useState(false);
  const [connMenuPos, setConnMenuPos] = useState<{
    top: number;
    left: number;
  } | null>(null);
  const connBtnRef = useRef<HTMLButtonElement>(null);
  const connMenuRef = useRef<HTMLDivElement>(null);
  const openConnMenu = () => {
    const r = connBtnRef.current?.getBoundingClientRect();
    if (r) setConnMenuPos({ top: r.bottom + 4, left: r.left });
    setConnMenuOpen(true);
  };
  useEffect(() => {
    if (!connMenuOpen) return;
    const onDoc = (e: MouseEvent) => {
      if (
        !connBtnRef.current?.contains(e.target as Node) &&
        !connMenuRef.current?.contains(e.target as Node)
      ) {
        setConnMenuOpen(false);
      }
    };
    const onKey = (e: KeyboardEvent) => e.key === "Escape" && setConnMenuOpen(false);
    document.addEventListener("mousedown", onDoc);
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("mousedown", onDoc);
      document.removeEventListener("keydown", onKey);
    };
  }, [connMenuOpen]);

  // Every row as one flat list, in the order they are drawn, because the
  // keyboard cursor is a position in that order.
  const navRows = useMemo(() => {
    type NavRow = {
      pane: PaneIndex | null;
      run: () => void;
      newTab: () => void;
    };
    const rows: NavRow[] = [];
    if (connections.length > 0) {
      // Opening the menu navigates nowhere by itself.
      rows.push({
        pane: null,
        run: () => (connMenuOpen ? setConnMenuOpen(false) : openConnMenu()),
        newTab: () => openConnMenu(),
      });
    }
    devices.forEach((d) =>
      rows.push({
        pane: 1,
        run: () => void navigate(1, d.mountPath),
        newTab: () => addTab(1, d.mountPath),
      }),
    );
    visiblePins.forEach((p) => {
      const pi = paneOf2(p);
      rows.push({
        pane: pi,
        run: () => void navigate(pi, p.path),
        newTab: () => addTab(pi, p.path),
      });
    });
    recent.forEach((r) => {
      const pi: PaneIndex = r.side === "remote" ? 0 : 1;
      rows.push({
        pane: pi,
        run: () => void navigate(pi, r.path),
        newTab: () => addTab(pi, r.path),
      });
    });
    return rows;
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [connections.length, connMenuOpen, devices, visiblePins, recent, navigate, addTab]);
  const OFF_DEV = connections.length > 0 ? 1 : 0;
  const OFF_PIN = OFF_DEV + devices.length;
  const OFF_REC = OFF_PIN + visiblePins.length;
  // Where each group starts, for the keys that jump between groups.
  const catStarts: number[] = [];
  if (connections.length) catStarts.push(0);
  if (devices.length) catStarts.push(OFF_DEV);
  if (visiblePins.length) catStarts.push(OFF_PIN);
  if (recent.length) catStarts.push(OFF_REC);

  // The keyboard while the sidebar is active. Moving between regions is not
  // handled here.
  useEffect(() => {
    if (!sbActive) return;
    const onKey = (e: KeyboardEvent) => {
      const ae = document.activeElement as HTMLElement | null;
      if (ae && (ae.tagName === "INPUT" || ae.tagName === "TEXTAREA")) return;
      if (useContextMenu.getState().open) return;
      const max = navRows.length - 1;
      if (max < 0) return;
      const cur = useUiStore.getState().sidebarCursor;
      const clamp = (n: number) => Math.max(0, Math.min(max, n));
      switch (e.key) {
        case "ArrowDown":
          e.preventDefault();
          setSidebarCursor(clamp(cur + 1));
          break;
        case "ArrowUp":
          e.preventDefault();
          setSidebarCursor(clamp(cur - 1));
          break;
        case "Home":
          e.preventDefault();
          setSidebarCursor(0);
          break;
        case "End":
          e.preventDefault();
          setSidebarCursor(max);
          break;
        case "PageDown": {
          // To the start of the next group, or to the very end.
          e.preventDefault();
          const next = catStarts.find((s) => s > cur);
          setSidebarCursor(next ?? max);
          break;
        }
        case "PageUp": {
          // To the start of the previous group — or of this one, from the
          // middle of it.
          e.preventDefault();
          const prev = [...catStarts].reverse().find((s) => s < cur);
          setSidebarCursor(prev ?? 0);
          break;
        }
        case "Enter": {
          e.preventDefault();
          const row = navRows[clamp(cur)];
          if (!row) break;
          // Shift opens in a new tab, as a shift-click does.
          if (e.shiftKey) row.newTab();
          else row.run();
          if (row.pane != null) backToPaneList(row.pane); // so the result is in view
          break;
        }
        default:
          return;
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [sbActive, navRows, setSidebarCursor]);

  // Keep the cursor row in view.
  useEffect(() => {
    if (!sbActive) return;
    document
      .querySelector<HTMLElement>('[data-sbcursor="true"]')
      ?.scrollIntoView({ block: "nearest" });
  }, [sidebarCursor, sbActive]);

  // Whether shift is held. Cleared when the window loses focus too, so it
  // cannot stay stuck down.
  const [shiftHeld, setShiftHeld] = useState(false);
  useEffect(() => {
    const down = (e: KeyboardEvent) => {
      if (e.key === "Shift") setShiftHeld(true);
    };
    const up = (e: KeyboardEvent) => {
      if (e.key === "Shift") setShiftHeld(false);
    };
    const blur = () => setShiftHeld(false);
    window.addEventListener("keydown", down);
    window.addEventListener("keyup", up);
    window.addEventListener("blur", blur);
    return () => {
      window.removeEventListener("keydown", down);
      window.removeEventListener("keyup", up);
      window.removeEventListener("blur", blur);
    };
  }, []);

  const paneOf = (pin: Pin): PaneIndex => (pin.side === "remote" ? 0 : 1);

  // Shift in keyboard mode hints the new-tab control on the cursor row.
  const sbShiftHint = sbActive && shiftHeld;

  // Reordering positions refer to the visible list. The stored list can be
  // filtered, so a position in it means nothing there and the drop is
  // translated into ids before it is applied.
  const [dragIndex, setDragIndex] = useState<number | null>(null);
  const [overIndex, setOverIndex] = useState<number | null>(null);
  const onRowDragOver = (
    e: ReactDragEvent<HTMLDivElement>,
    i: number,
  ) => {
    const r = e.currentTarget.getBoundingClientRect();
    const after = e.clientY - r.top > r.height / 2;
    setOverIndex(after ? i + 1 : i);
  };
  const endDrag = () => {
    setDragIndex(null);
    setOverIndex(null);
  };
  const dropPin = () => {
    if (dragIndex !== null && overIndex !== null) {
      const draggedId = visiblePins[dragIndex]?.id;
      const beforeId = visiblePins[overIndex]?.id ?? null;
      if (draggedId) movePin(draggedId, beforeId);
    }
    endDrag();
  };
  const InsertLine = () => (
    <div className="mx-[7px] my-px h-0.5 rounded bg-accent" />
  );

  return (
    <aside
      data-region="sidebar"
      tabIndex={-1}
      onMouseEnter={() => {
        // The sidebar is a region of its own: entering it means no pane is
        // active. An open context menu freezes that.
        if (useContextMenu.getState().open) return;
        setActivePane(null);
      }}
      className="flex min-h-0 flex-col gap-[14px] overflow-y-auto border-r border-neutral-900 bg-chrome px-2.5 py-3 focus:outline-none"
    >
      {connections.length > 0 && (
        <Section title={t("main.sidebar.networkVolumes")}>
          <button
            ref={connBtnRef}
            type="button"
            data-sbcursor={sbActive && sidebarCursor === 0 ? "true" : undefined}
            onClick={() => (connMenuOpen ? setConnMenuOpen(false) : openConnMenu())}
            className={
              "flex w-full items-center gap-[7px] rounded-sm border px-[7px] py-[5px] text-[12.5px] text-accent-200 " +
              (sbActive && sidebarCursor === 0
                ? "border-accent-400"
                : connMenuOpen
                  ? "border-accent-700 bg-accent-900"
                  : "border-transparent hover:bg-neutral-900")
            }
          >
            <CloudIcon size={14} className="shrink-0 text-accent-300" />
            <span className="min-w-0 flex-1 truncate text-left">
              {activeConn ? activeConn.name || activeConn.bucket || activeConn.id : "—"}
            </span>
            <CaretDownIcon
              size={10}
              className={
                "shrink-0 " + (connMenuOpen ? "text-accent-300" : "text-neutral-600")
              }
            />
          </button>

          {connMenuOpen && connMenuPos && (
            <div
              ref={connMenuRef}
              style={{ top: connMenuPos.top, left: connMenuPos.left }}
              className="fixed z-50 w-[176px] rounded-md border border-neutral-800 bg-surface p-[5px] shadow-lg"
            >
              {connections.map((c) => (
                <button
                  key={c.id}
                  type="button"
                  onClick={() => {
                    switchTo(c);
                    setConnMenuOpen(false);
                  }}
                  className={
                    "flex w-full items-center gap-2 rounded-sm px-[9px] py-[5px] text-[12.5px] " +
                    (c.id === activeConnId
                      ? "bg-accent-900 text-accent-100"
                      : "text-neutral-300 hover:bg-accent-900")
                  }
                >
                  <CloudIcon size={13} className="shrink-0 text-accent-300" />
                  <span className="min-w-0 flex-1 truncate text-left">
                    {c.name || c.bucket || c.id}
                  </span>
                </button>
              ))}
            </div>
          )}
        </Section>
      )}

      {devices.length > 0 && (
        <Section title={t("main.sidebar.localDevices")}>
          {devices.map((d, i) => (
            <Row
              key={d.mountPath}
              cursor={sbActive && sidebarCursor === OFF_DEV + i}
              newTabHint={sbShiftHint && sidebarCursor === OFF_DEV + i}
              shiftHeld={shiftHeld}
              className="text-neutral-300"
              onClick={() => navigate(1, d.mountPath)}
              onNewTab={() => addTab(1, d.mountPath)}
              icon={
                <DesktopTowerIcon size={14} className="shrink-0 text-local-300" />
              }
            >
              {d.label}
            </Row>
          ))}
        </Section>
      )}

      <Section
        title={t("main.sidebar.pinned")}
        legend={
          <span className="flex items-center gap-[5px]">
            <DesktopTowerIcon
              size={11}
              className="text-local-700"
              aria-label={t("main.sidebar.legendLocal")}
            />
            <CloudIcon
              size={11}
              className="text-accent-700"
              aria-label={t("main.sidebar.legendRemote")}
            />
          </span>
        }
      >
        {visiblePins.map((p, i) => (
          <div key={p.id}>
            {dragIndex !== null && overIndex === i && <InsertLine />}
            <PinRow
              pin={p}
              index={i}
              shiftHeld={shiftHeld}
              dragging={dragIndex === i}
              cursor={sbActive && sidebarCursor === OFF_PIN + i}
              onDragStart={setDragIndex}
              onDragOver={onRowDragOver}
              onDrop={dropPin}
              onDragEnd={endDrag}
              onOpen={() => navigate(paneOf(p), p.path)}
              onOpenNewTab={() => addTab(paneOf(p), p.path)}
              onContext={(e) => {
                e.preventDefault();
                openMenu(e.clientX, e.clientY, [
                  {
                    id: "unpin",
                    label: t("ctx.unpin"),
                    icon: <PushPinSlashIcon size={14} />,
                    onSelect: () => removePin(p.id),
                  },
                ]);
              }}
            />
          </div>
        ))}
        {dragIndex !== null && overIndex === visiblePins.length && <InsertLine />}
      </Section>

      {recent.length > 0 && (
        <Section title={t("main.sidebar.recent")}>
          {recent.map((r, i) => {
            const pi: PaneIndex = r.side === "remote" ? 0 : 1;
            return (
              <Row
                key={`${r.side}:${r.path}`}
                cursor={sbActive && sidebarCursor === OFF_REC + i}
                newTabHint={sbShiftHint && sidebarCursor === OFF_REC + i}
                shiftHeld={shiftHeld}
                className="text-neutral-500"
                onClick={() => navigate(pi, r.path)}
                onNewTab={() => addTab(pi, r.path)}
                icon={
                  r.side === "remote" ? (
                    <CloudIcon size={11} className="shrink-0 text-accent-700" />
                  ) : (
                    <DesktopTowerIcon
                      size={11}
                      className="shrink-0 text-local-700"
                    />
                  )
                }
              >
                {r.label}
              </Row>
            );
          })}
        </Section>
      )}
    </aside>
  );
}
