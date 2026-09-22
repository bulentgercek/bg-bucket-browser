import { useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import { createPortal } from "react-dom";
import {
  ArrowLineDownIcon,
  ArrowLineUpIcon,
  CheckCircleIcon,
  ProhibitIcon,
  WarningCircleIcon,
  XIcon,
} from "@phosphor-icons/react";
import {
  useTransferStore,
  type Transfer,
  type TransferStatus,
} from "../../state/transferQueueStore";
import { useStatusBarPopupStore } from "../../state/statusBarPopupStore";
import { transferCancel } from "../../lib/commands";
import { formatSize } from "../../lib/format";
import { rateOf } from "../../lib/rate";
import { t } from "../../locale/en";

// Status bar pills for running transfers, one per connection, each opening its own queue panel.

const isLive = (s: TransferStatus) => s === "queued" || s === "active";
const pctOf = (tr: Transfer) =>
  tr.bytesTotal > 0 ? Math.round((tr.bytesDone / tr.bytesTotal) * 100) : 0;
const progressText = (percent: number, bytesPerSec: number) =>
  t("statusbar.transfer", { percent, speed: `${formatSize(Math.round(bytesPerSec))}/s` });

/** Draws again every second while `running`, so the speed keeps moving, and
    falls to zero on a stall, even when no progress report arrives. */
function useSecondTick(running: boolean): void {
  const [, setTick] = useState(0);
  useEffect(() => {
    if (!running) return;
    const timer = setInterval(() => setTick((n) => n + 1), 1000);
    return () => clearInterval(timer);
  }, [running]);
}

// Error red, the same token the toasts use.
const ERR = "var(--color-danger)";

/** Transfers of one connection; local↔local jobs have no connection and share one group. */
interface TransferGroup {
  key: string;
  label: string;
  items: Transfer[];
}
function groupTransfers(transfers: Transfer[]): TransferGroup[] {
  const order: string[] = [];
  const map = new Map<string, TransferGroup>();
  for (const tr of transfers) {
    const key = tr.connId ?? "__local__";
    let g = map.get(key);
    if (!g) {
      g = { key, label: tr.connName ?? t("route.local"), items: [] };
      map.set(key, g);
      order.push(key);
    }
    g.items.push(tr);
  }
  return order.map((k) => map.get(k)!);
}

export default function TransfersIndicator() {
  const transfers = useTransferStore((s) => s.transfers);
  const remove = useTransferStore((s) => s.remove);

  // Every destination gets its own pill; with nothing running the whole indicator disappears.
  const groups = useMemo(() => groupTransfers(transfers), [transfers]);
  const liveGroups = useMemo(
    () => groups.filter((g) => g.items.some((x) => isLive(x.status))),
    [groups],
  );

  useSecondTick(liveGroups.length > 0);
  const now = performance.now();

  const [openKey, setOpenKey] = useState<string | null>(null);
  // Only one status bar popup is open at a time; the clipboard one closes this.
  const activePopup = useStatusBarPopupStore((s) => s.active);
  const setActivePopup = useStatusBarPopupStore((s) => s.setActive);
  useEffect(() => {
    if (activePopup !== "transfers" && openKey !== null) setOpenKey(null);
  }, [activePopup, openKey]);
  const open = (key: string | null) => {
    setOpenKey(key);
    setActivePopup(key !== null ? "transfers" : null);
  };
  // When the open group has nothing running left, its panel closes with it.
  useEffect(() => {
    if (openKey !== null && !liveGroups.some((g) => g.key === openKey)) {
      open(null);
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [liveGroups, openKey]);

  // The panel is positioned from the pill's screen coordinates, not from the DOM tree.
  const pillRefs = useRef<Map<string, HTMLElement>>(new Map());
  const [popup, setPopup] = useState<
    { left: number; bottom: number; anchor: HTMLElement } | null
  >(null);
  useLayoutEffect(() => {
    if (openKey === null) {
      setPopup(null);
      return;
    }
    const el = pillRefs.current.get(openKey);
    if (!el) return;
    const r = el.getBoundingClientRect();
    const half = 190; // half of the panel width below
    const left = Math.min(
      window.innerWidth - 8 - half,
      Math.max(8 + half, r.left + r.width / 2),
    );
    setPopup({ left, bottom: window.innerHeight - r.top + 8, anchor: el });
  }, [openKey]);

  if (liveGroups.length === 0) return null;

  const openGroup =
    openKey === null ? undefined : groups.find((g) => g.key === openKey);

  return (
    <div className="flex items-center gap-2">
      {openKey !== null && popup && openGroup && (
        <QueuePanel
          key={openKey}
          pos={popup}
          anchor={popup.anchor}
          title={t("queue.titleFor", { volume: openGroup.label })}
          items={openGroup.items}
          now={now}
          onRemove={remove}
          onClearFinished={() => {
            for (const tr of openGroup.items) {
              if (!isLive(tr.status)) remove(tr.id);
            }
          }}
          onCancelAll={() => {
            for (const tr of openGroup.items) {
              if (isLive(tr.status)) void transferCancel(tr.id);
            }
          }}
          onClose={() => open(null)}
        />
      )}

      {liveGroups.map((g) => {
        const glive = g.items.filter((x) => isLive(x.status));
        const gactive = glive.find((x) => x.status === "active") ?? glive[0];
        const gDone = glive.reduce((a, x) => a + x.bytesDone, 0);
        const gTotal = glive.reduce((a, x) => a + x.bytesTotal, 0);
        const gPct = gTotal > 0 ? Math.round((gDone / gTotal) * 100) : 0;
        // The queue runs one job at a time, so a group has at most one active.
        const running = glive.find((x) => x.status === "active");
        return (
          <button
            key={g.key}
            type="button"
            ref={(el) => {
              if (el) pillRefs.current.set(g.key, el);
              else pillRefs.current.delete(g.key);
            }}
            onClick={() => open(openKey === g.key ? null : g.key)}
            title={g.label}
            className="flex items-center gap-1.5 rounded-full border border-neutral-800 px-2 py-0.5 hover:border-accent-700 hover:text-accent-300"
          >
            {gactive?.kind === "upload" ? (
              <ArrowLineUpIcon size={11} className="shrink-0 text-accent-400" />
            ) : (
              <ArrowLineDownIcon size={11} className="shrink-0 text-accent-400" />
            )}
            <span className="max-w-[110px] truncate">{g.label}</span>
            <span className="tabular-nums text-neutral-600">
              {running ? progressText(gPct, rateOf(running.samples, now)) : `${gPct}%`}
            </span>
          </button>
        );
      })}
    </div>
  );
}

/** The queue panel above a pill; what it shows and what its buttons do come from props. */
function QueuePanel({
  pos,
  anchor,
  title,
  items,
  now,
  onRemove,
  onClearFinished,
  onCancelAll,
  onClose,
}: {
  pos: { left: number; bottom: number };
  anchor: HTMLElement;
  title: string;
  items: Transfer[];
  now: number;
  onRemove: (id: string) => void;
  onClearFinished: () => void;
  onCancelAll: () => void;
  onClose: () => void;
}) {
  const ref = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const onDown = (e: MouseEvent) => {
      // A click on the panel or on its own pill is not "outside".
      const t = e.target as Node;
      if (!ref.current?.contains(t) && !anchor.contains(t)) onClose();
    };
    const onKey = (e: KeyboardEvent) => e.key === "Escape" && onClose();
    window.addEventListener("mousedown", onDown, true);
    window.addEventListener("keydown", onKey);
    return () => {
      window.removeEventListener("mousedown", onDown, true);
      window.removeEventListener("keydown", onKey);
    };
  }, [anchor, onClose]);

  const anyLive = items.some((x) => isLive(x.status));
  const anyFinished = items.some((x) => !isLive(x.status));

  // Rendered into the body so no ancestor's stacking context can cover it.
  return createPortal(
    <div
      ref={ref}
      style={{ left: pos.left, bottom: pos.bottom }}
      className="fixed z-[100] flex max-h-[46vh] w-[380px] -translate-x-1/2 flex-col rounded-md border border-neutral-800 bg-surface shadow-lg"
    >
      <div className="flex items-center justify-between border-b border-neutral-800 px-3 py-2 text-[11px] font-medium tracking-wide text-neutral-500">
        <span className="truncate">{title}</span>
        <span className="shrink-0 text-neutral-600">{items.length}</span>
      </div>

      <div className="min-h-0 flex-1 overflow-y-auto py-1">
        {items.length === 0 ? (
          <p className="px-3 py-4 text-center text-[11.5px] text-neutral-600">
            {t("queue.empty")}
          </p>
        ) : (
          items.map((tr) => <QueueRow key={tr.id} tr={tr} now={now} onRemove={onRemove} />)
        )}
      </div>

      <div className="flex items-center justify-between gap-2 border-t border-neutral-800 px-3 py-2">
        <button
          type="button"
          disabled={!anyFinished}
          onClick={onClearFinished}
          className="text-[11px] text-neutral-500 hover:text-neutral-200 disabled:cursor-default disabled:opacity-40 disabled:hover:text-neutral-500"
        >
          {t("queue.clearFinished")}
        </button>
        <button
          type="button"
          disabled={!anyLive}
          onClick={onCancelAll}
          className="text-[11px] font-medium text-neutral-400 hover:text-neutral-100 disabled:cursor-default disabled:opacity-40 disabled:hover:text-neutral-400"
        >
          {t("queue.cancelAll")}
        </button>
      </div>
    </div>,
    document.body,
  );
}

function QueueRow({
  tr,
  now,
  onRemove,
}: {
  tr: Transfer;
  now: number;
  onRemove: (id: string) => void;
}) {
  const live = isLive(tr.status);
  const pctVal = pctOf(tr);
  const Dir = tr.kind === "upload" ? ArrowLineUpIcon : ArrowLineDownIcon;

  return (
    <div className="flex items-center gap-2 px-3 py-1.5 text-[11.5px]">
      <Dir
        size={12}
        className={"shrink-0 " + (tr.status === "error" ? "" : "text-accent-400")}
        style={tr.status === "error" ? { color: ERR } : undefined}
      />
      <div className="min-w-0 flex-1">
        <div className="flex items-center justify-between gap-2">
          <span className="min-w-0 truncate text-neutral-300">{tr.name}</span>
          <span className="shrink-0 tabular-nums text-neutral-500">
            {live ? statusRight(tr, now) : <StatusTag status={tr.status} />}
          </span>
        </div>
        {/* Direction, so a stuck transfer says where it was going. */}
        <div className="mt-0.5 text-[10px] text-neutral-600">
          {tr.from} <span className="text-neutral-700">→</span> {tr.to}
        </div>
        {live && tr.currentFile && (
          <div className="mt-0.5 truncate text-[10px] text-neutral-500" title={tr.currentFile}>
            {tr.currentFile}
          </div>
        )}
        {/* A chunk is being retried; the transfer is recovering, not frozen. */}
        {live && tr.retry && (
          <div className="mt-0.5 truncate text-[10px] text-amber-400">
            {t("transfer.retrying", { attempt: tr.retry.attempt, of: tr.retry.of })}
          </div>
        )}
        {live && (
          <div className="mt-1 h-[3px] overflow-hidden rounded-full bg-neutral-800">
            <div className="h-full bg-accent" style={{ width: `${pctVal}%` }} />
          </div>
        )}
        {tr.status === "error" && tr.detail && (
          <p
            className="mt-0.5 truncate text-[10.5px]"
            style={{ color: ERR }}
            title={tr.detail}
          >
            {tr.detail}
          </p>
        )}
      </div>
      <button
        type="button"
        onClick={() => (live ? void transferCancel(tr.id) : onRemove(tr.id))}
        aria-label={live ? t("statusbar.cancel") : t("queue.remove")}
        className="shrink-0 text-neutral-600 hover:text-neutral-300"
      >
        <XIcon size={11} />
      </button>
    </div>
  );
}

function statusRight(tr: Transfer, now: number): string {
  if (tr.status === "queued") return t("queue.status.queued");
  return progressText(pctOf(tr), rateOf(tr.samples, now));
}

function StatusTag({ status }: { status: TransferStatus }) {
  if (status === "done")
    return (
      <span className="flex items-center gap-1 text-neutral-500">
        <CheckCircleIcon size={12} weight="fill" className="text-accent-400" />
        {t("queue.status.done")}
      </span>
    );
  if (status === "error")
    return (
      <span className="flex items-center gap-1" style={{ color: ERR }}>
        <WarningCircleIcon size={12} weight="fill" />
        {t("queue.status.error")}
      </span>
    );
  return (
    <span className="flex items-center gap-1 text-neutral-500">
      <ProhibitIcon size={12} />
      {t("queue.status.canceled")}
    </span>
  );
}
