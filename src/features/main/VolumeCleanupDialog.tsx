import { createPortal } from "react-dom";
import { useEffect, useRef, useState, type ReactNode } from "react";
import {
  ArrowsOutLineVerticalIcon,
  CircleNotchIcon,
  DotsSixVerticalIcon,
  FileIcon,
  FolderIcon,
  TrashIcon,
  WarningCircleIcon,
} from "@phosphor-icons/react";
import { useCleanupStore } from "../../state/cleanupStore";
import { useConnectionStore } from "../../state/connectionStore";
import { useDialogStore } from "../../state/dialogStore";
import { formatSize } from "../../lib/format";
import { t } from "../../locale/en";

// Volume Cleanup window: scan, report and delete. The scan itself runs in Rust (cleanup.rs).

export default function VolumeCleanupDialog() {
  const open = useCleanupStore((s) => s.open);
  const status = useCleanupStore((s) => s.status);
  const progress = useCleanupStore((s) => s.progress);
  const report = useCleanupStore((s) => s.report);
  const selected = useCleanupStore((s) => s.selected);
  const close = useCleanupStore((s) => s.close);
  const runScan = useCleanupStore((s) => s.runScan);
  const stopScan = useCleanupStore((s) => s.stopScan);
  const toggle = useCleanupStore((s) => s.toggle);
  const selectAllReclaimable = useCleanupStore((s) => s.selectAllReclaimable);
  const clearSelection = useCleanupStore((s) => s.clearSelection);
  const runDelete = useCleanupStore((s) => s.runDelete);
  // Footer button picked with the arrow keys; "" means the screen's default.
  const [choice, setChoice] = useState("");
  // Report area that takes the free height; the others shrink to short boxes.
  const [expanded, setExpanded] = useState<ReportArea>("reclaimable");
  const topExpanded = expanded === "top";
  // Share of the top row given to Top folders; the rest goes to Largest objects.
  const [topSplit, setTopSplit] = useState(TOP_SPLIT_DEFAULT);
  const topRowRef = useRef<HTMLDivElement>(null);

  const beginSplitDrag = (e: React.MouseEvent) => {
    if (e.button !== 0) return;
    const row = topRowRef.current;
    if (!row) return;
    e.preventDefault();
    const rect = row.getBoundingClientRect();
    const usable = rect.width - SPLIT_GAP_PX;
    const move = (ev: MouseEvent) => {
      const ratio = (ev.clientX - rect.left - SPLIT_GAP_PX / 2) / usable;
      setTopSplit(Math.min(TOP_SPLIT_MAX, Math.max(TOP_SPLIT_MIN, ratio)));
    };
    const up = () => {
      window.removeEventListener("mousemove", move);
      window.removeEventListener("mouseup", up);
      document.body.style.cursor = "";
    };
    window.addEventListener("mousemove", move);
    window.addEventListener("mouseup", up);
    document.body.style.cursor = "col-resize";
  };
  const showDialog = useDialogStore((s) => s.show);

  const connections = useConnectionStore((s) => s.connections);
  const activeId = useConnectionStore((s) => s.activeId);
  const activeConn = connections.find((c) => c.id === activeId) ?? null;
  const volume = activeConn?.name || activeConn?.bucket || "—";

  // Closing by outside click or Escape during a scan asks first; the footer Cancel button does not.
  const requestClose = () => {
    if (status === "scanning") {
      showDialog({
        kind: "confirm",
        center: true,
        title: t("cleanup.abandonTitle"),
        body: t("cleanup.abandonBody"),
        confirmLabel: t("cleanup.abandonConfirm"),
        cancelLabel: t("cleanup.continueCleanup"),
        danger: true,
        onConfirm: close,
      });
      return;
    }
    close();
  };

  // A new screen starts from its default button.
  useEffect(() => setChoice(""), [status, open]);

  // Keys are caught in the capture phase so they never reach the panes behind the window.
  // A confirm dialog on top owns the keys. The listener reads current buttons from a ref.
  const navRef = useRef<{ buttons: FooterButton[]; current: string; requestClose: () => void }>({
    buttons: [],
    current: "",
    requestClose: () => {},
  });
  useEffect(() => {
    if (!open) return;
    const onKey = (e: KeyboardEvent) => {
      if (useDialogStore.getState().dialog) return;
      e.stopPropagation();
      if (e.code === "Tab" || e.key === "Tab") {
        e.preventDefault();
        return;
      }
      const { buttons, current, requestClose: rc } = navRef.current;
      if (e.key === "Escape") {
        rc();
        return;
      }
      const enabled = buttons.filter((b) => !b.disabled);
      if (enabled.length === 0) return;
      const i = Math.max(0, enabled.findIndex((b) => b.id === current));
      if (e.key === "ArrowLeft" || e.key === "ArrowRight") {
        e.preventDefault();
        const next = e.key === "ArrowLeft" ? Math.max(0, i - 1) : Math.min(enabled.length - 1, i + 1);
        setChoice(enabled[next].id);
        return;
      }
      if (e.key === "Enter") {
        e.preventDefault();
        enabled[i].onClick();
      }
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [open]);

  if (!open) return null;

  const selectedBytes = report
    ? [...report.largest, ...report.reclaimable]
        .filter((e, i, arr) => arr.findIndex((x) => x.key === e.key) === i)
        .filter((e) => selected.has(e.key))
        .reduce((a, e) => a + e.size, 0)
    : 0;

  const confirmDelete = () => {
    showDialog({
      kind: "confirm",
      center: true,
      title: t("cleanup.confirmTitle", { count: selected.size }),
      body: t("cleanup.confirmBody"),
      confirmLabel: t("cleanup.confirmDelete"),
      danger: true,
      onConfirm: () => void runDelete(),
    });
  };

  // Footer buttons for the current screen, left to right.
  const buttons: FooterButton[] = [];
  if (status === "done") {
    buttons.push({ id: "rescan", label: t("cleanup.rescan"), onClick: () => void runScan() });
  }
  buttons.push({
    id: "cancel",
    label: t("cleanup.cancel"),
    onClick: status === "scanning" ? stopScan : close,
    disabled: status === "deleting",
  });
  if (status === "idle") {
    buttons.push({ id: "scan", label: t("cleanup.runScan"), onClick: () => void runScan(), main: true });
  }
  if (status === "done") {
    buttons.push({
      id: "delete",
      label: (
        <>
          <TrashIcon size={13} />
          {t("cleanup.deleteSelected")}
        </>
      ),
      onClick: confirmDelete,
      disabled: selected.size === 0,
      main: true,
    });
  }
  const enabledButtons = buttons.filter((b) => !b.disabled);
  const current =
    enabledButtons.find((b) => b.id === choice)?.id ??
    enabledButtons.find((b) => b.main)?.id ??
    enabledButtons.find((b) => b.id === "cancel")?.id ??
    "";
  navRef.current = { buttons, current, requestClose };

  return createPortal(
    // Stacks above the context menu and below confirm dialogs, which open on top of this window.
    <div
      className="fixed inset-0 z-[120] flex items-center justify-center bg-black/50"
      onMouseDown={requestClose}
    >
      <div
        className={
          "flex flex-col rounded-lg border border-neutral-700 bg-surface shadow-lg " +
          "transition-[width,height] duration-200 " +
          (status === "done"
            ? "h-[88vh] w-[92vw] max-w-[1500px]"
            : "max-h-[80vh] w-[640px]")
        }
        onMouseDown={(e) => e.stopPropagation()}
      >
        <div className="border-b border-neutral-800 px-5 py-3.5">
          <h4 className="text-[14px] font-medium">
            {t("cleanup.title", { volume })}
          </h4>
        </div>

        <div className="flex min-h-0 flex-1 flex-col px-5 py-4">
          {status === "idle" && (
            <p className="text-[12.5px] leading-relaxed text-neutral-400">
              {t("cleanup.intro")}
            </p>
          )}

          {status === "scanning" && (
            <div className="flex flex-col items-center gap-3 py-10 text-center">
              <CircleNotchIcon size={22} className="animate-spin text-accent-400" />
              <p className="text-[12.5px] text-neutral-400">
                {t("cleanup.scanning", {
                  count: progress?.scanned ?? 0,
                  size: formatSize(progress?.bytes ?? 0),
                })}
              </p>
            </div>
          )}

          {status === "deleting" && (
            <div className="flex flex-col items-center gap-3 py-10 text-center">
              <CircleNotchIcon size={22} className="animate-spin text-accent-400" />
              <p className="text-[12.5px] text-neutral-400">{t("cleanup.deleting")}</p>
            </div>
          )}

          {status === "done" && report && (
            <div className="flex min-h-0 flex-1 flex-col gap-4">
              <div className="flex shrink-0 items-center justify-between gap-3">
                <p className="text-[12.5px] text-neutral-400">
                  {t("cleanup.summary", {
                    count: report.totalObjects,
                    size: formatSize(report.totalBytes),
                  })}
                </p>
                {!topExpanded && (
                  <ExpandButton label={t("cleanup.expandTop")} onClick={() => setExpanded("top")} />
                )}
              </div>

              <div
                ref={topRowRef}
                className={
                  "grid " + (topExpanded ? "min-h-0 flex-1 grid-rows-[minmax(0,1fr)]" : "shrink-0")
                }
                // Columns may shrink below their longest path, which is truncated instead of widening the column.
                style={{
                  gridTemplateColumns: `minmax(0, ${topSplit}fr) ${SPLIT_GAP_PX}px minmax(0, ${1 - topSplit}fr)`,
                }}
              >
                <Section
                  title={t("cleanup.topFolders")}
                  className={"min-w-0" + (topExpanded ? " min-h-0" : "")}
                  bodyClassName={topExpanded ? listFill : listShort}
                >
                  {report.topFolders.length === 0 ? (
                    <EmptyRow />
                  ) : (
                    report.topFolders.map((f) => (
                      <div
                        key={f.prefix}
                        className="flex items-center gap-2 rounded-sm px-2 py-1 text-[12px]"
                      >
                        {f.isFile ? (
                          <FileIcon size={13} className="shrink-0 text-neutral-500" />
                        ) : (
                          <FolderIcon size={13} className="shrink-0 text-neutral-500" />
                        )}
                        <span className="min-w-0 flex-1 truncate text-neutral-300">
                          {f.prefix || "/"}
                        </span>
                        <span className="shrink-0 text-neutral-500">{f.count}</span>
                        <span className="w-16 shrink-0 text-right tabular-nums text-neutral-400">
                          {formatSize(f.bytes)}
                        </span>
                      </div>
                    ))
                  )}
                </Section>

                <div
                  onMouseDown={beginSplitDrag}
                  onDoubleClick={() => setTopSplit(TOP_SPLIT_DEFAULT)}
                  className="group relative flex cursor-col-resize items-center justify-center pt-5"
                >
                  <div className="absolute bottom-0 top-5 w-px bg-transparent transition-colors group-hover:bg-accent-700" />
                  <DotsSixVerticalIcon
                    size={14}
                    weight="bold"
                    className="relative rounded-sm bg-surface py-0.5 text-neutral-600 transition-colors group-hover:text-accent-300"
                  />
                </div>

                <Section
                  title={t("cleanup.largest")}
                  className={"min-w-0" + (topExpanded ? " min-h-0" : "")}
                  bodyClassName={topExpanded ? listFill : listShort}
                >
                  {report.largest.length === 0 ? (
                    <EmptyRow />
                  ) : (
                    report.largest.map((e) => (
                      <EntryRow
                        key={e.key}
                        entry={e}
                        checked={selected.has(e.key)}
                        onToggle={() => toggle(e.key)}
                      />
                    ))
                  )}
                </Section>
              </div>

              {report.skipped.length > 0 && (
                <Section
                  title={`${t("cleanup.skipped")} · ${report.skipped.length}`}
                  className={expanded === "skipped" ? "min-h-0 flex-1" : "shrink-0"}
                  bodyClassName={expanded === "skipped" ? listFill : "max-h-[84px] overflow-y-auto"}
                  action={
                    <span className="flex items-center gap-4">
                      <span className="text-[11px] text-neutral-600">{t("cleanup.skippedHint")}</span>
                      {expanded !== "skipped" && (
                        <ExpandButton label={t("cleanup.expandSkipped")} onClick={() => setExpanded("skipped")} />
                      )}
                    </span>
                  }
                >
                  {report.skipped.map((p) => (
                    <div key={p} className="flex items-center gap-2 px-2 py-1 text-[12px]">
                      <FolderIcon size={13} className="shrink-0 text-neutral-600" />
                      <span className="min-w-0 flex-1 truncate text-neutral-500">{p}</span>
                    </div>
                  ))}
                </Section>
              )}

              <Section
                title={t("cleanup.reclaimable")}
                className={expanded === "reclaimable" ? "min-h-0 flex-1" : "shrink-0"}
                bodyClassName={expanded === "reclaimable" ? listFill : listShort}
                action={
                  <span className="flex items-center gap-4">
                    {report.reclaimable.length > 0 && (
                      <button
                        type="button"
                        onClick={selectAllReclaimable}
                        className="text-[11px] text-accent-300 hover:text-accent-200"
                      >
                        {t("cleanup.selectAllReclaimable")}
                      </button>
                    )}
                    {expanded !== "reclaimable" && (
                      <ExpandButton label={t("cleanup.expandReclaimable")} onClick={() => setExpanded("reclaimable")} />
                    )}
                  </span>
                }
              >
                {report.reclaimable.length === 0 ? (
                  <p className="px-2 py-2 text-[11.5px] text-neutral-600">
                    {t("cleanup.reclaimableEmpty")}
                  </p>
                ) : (
                  report.reclaimable.map((e) => (
                    <EntryRow
                      key={e.key}
                      entry={e}
                      checked={selected.has(e.key)}
                      onToggle={() => toggle(e.key)}
                    />
                  ))
                )}
              </Section>
            </div>
          )}
        </div>

        <div className="flex items-center justify-between gap-3 border-t border-neutral-800 px-5 py-3.5">
          {status === "done" && selected.size > 0 ? (
            <span className="flex items-center gap-1.5 text-[11.5px] text-neutral-500">
              <WarningCircleIcon size={13} className="text-[var(--color-danger)]" />
              {t("cleanup.selectedSummary", {
                count: selected.size,
                size: formatSize(selectedBytes),
              })}
            </span>
          ) : status === "done" ? (
            <button
              type="button"
              onClick={clearSelection}
              className="invisible text-[11.5px]"
            >
              {t("cleanup.clearSelection")}
            </button>
          ) : (
            <span />
          )}

          <div className="flex gap-2.5">
            {buttons.map((b) => (
              <button
                key={b.id}
                type="button"
                className={`btn btn-choice${b.id === current ? " btn-selected" : ""}`}
                onClick={b.onClick}
                disabled={b.disabled}
              >
                {b.label}
              </button>
            ))}
          </div>
        </div>
      </div>
    </div>,
    document.body,
  );
}

// Top row split: default and limits for the Top folders share, and the drag strip width.
const TOP_SPLIT_DEFAULT = 0.4;
const TOP_SPLIT_MIN = 0.2;
const TOP_SPLIT_MAX = 0.8;
const SPLIT_GAP_PX = 16;

// List box heights: a short fixed box, or one that fills the remaining height.
const listShort = "max-h-[150px] overflow-y-auto";
const listFill = "min-h-0 flex-1 overflow-y-auto";

/** Report areas that can take the free height of the window. */
type ReportArea = "top" | "skipped" | "reclaimable";

function ExpandButton({ label, onClick }: { label: string; onClick: () => void }) {
  return (
    <button
      type="button"
      onClick={onClick}
      className="flex items-center gap-1.5 text-[11px] text-accent-300 hover:text-accent-200"
    >
      <ArrowsOutLineVerticalIcon size={12} />
      {label}
    </button>
  );
}

/** A footer button. `main` marks the screen's primary action, selected by default. */
interface FooterButton {
  id: string;
  label: ReactNode;
  onClick: () => void;
  disabled?: boolean;
  main?: boolean;
}

function Section({
  title,
  action,
  children,
  className = "",
  bodyClassName = "max-h-[160px] overflow-y-auto",
}: {
  title: string;
  action?: ReactNode;
  children: ReactNode;
  /** Extra classes for the outer box, e.g. `flex-1` to fill the remaining height. */
  className?: string;
  /** Extra classes for the list box; sets its height and scrolling. */
  bodyClassName?: string;
}) {
  return (
    <div className={`flex flex-col gap-1 ${className}`}>
      <div className="flex items-baseline justify-between px-1">
        <span className="text-[9.5px] font-semibold uppercase tracking-[0.09em] text-neutral-600">
          {title}
        </span>
        {action}
      </div>
      <div className={`rounded-sm border border-neutral-900 ${bodyClassName}`}>
        {children}
      </div>
    </div>
  );
}

function EmptyRow() {
  return <p className="px-2 py-2 text-[11.5px] text-neutral-600">—</p>;
}

function EntryRow({
  entry,
  checked,
  onToggle,
}: {
  entry: { key: string; size: number };
  checked: boolean;
  onToggle: () => void;
}) {
  return (
    <label className="flex cursor-pointer items-center gap-2 px-2 py-1 text-[12px] hover:bg-neutral-900">
      <input
        type="checkbox"
        checked={checked}
        onChange={onToggle}
        className="h-3.5 w-3.5 shrink-0 cursor-pointer accent-[var(--color-accent)]"
      />
      <span className="min-w-0 flex-1 truncate text-neutral-300">{entry.key}</span>
      <span className="w-16 shrink-0 text-right tabular-nums text-neutral-500">
        {formatSize(entry.size)}
      </span>
    </label>
  );
}
