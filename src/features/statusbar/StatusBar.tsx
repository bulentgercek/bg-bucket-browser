import { useEffect, useState } from "react";
import { usePaneStore } from "../../state/paneStore";
import { useUiStore } from "../../state/uiStore";
import { useConnectionStore } from "../../state/connectionStore";
import { diskUsage, volumeQuota, type DiskUsage, type VolumeQuota } from "../../lib/commands";
import { visibleEntries } from "../../lib/entries";
import { formatSize } from "../../lib/format";
import { t } from "../../locale/en";
import TransfersIndicator from "./TransfersIndicator";
import ClipboardIndicator from "./ClipboardIndicator";

/* The bar along the bottom: what is selected on the left, the transfer and
   clipboard indicators in the middle, and how much room is left on the right.

   The two indicators are separate components that share nothing but which
   popup is open. */

/* Free space on the filesystem a local path is on, refreshed as the pane
   moves. */
function useDiskFree(path: string | null): DiskUsage | null {
  const [du, setDu] = useState<DiskUsage | null>(null);
  useEffect(() => {
    if (path == null) {
      setDu(null);
      return;
    }
    let alive = true;
    diskUsage(path)
      .then((r) => alive && setDu(r))
      .catch(() => alive && setDu(null));
    return () => {
      alive = false;
    };
  }, [path]);
  return du;
}

/* The volume's total size, which needs the account key. Without it, or on any
   failure, this stays empty and the bar simply shows the connection instead —
   a status bar is no place to nag.

   Total only: used and free space is not available for a volume without a
   running pod. */
function useVolumeQuota(connId: string | null): VolumeQuota | null {
  const [q, setQ] = useState<VolumeQuota | null>(null);
  useEffect(() => {
    if (connId == null) {
      setQ(null);
      return;
    }
    let alive = true;
    volumeQuota()
      .then((r) => alive && setQ(r))
      .catch(() => alive && setQ(null));
    return () => {
      alive = false;
    };
  }, [connId]);
  return q;
}

export default function StatusBar() {
  // With the sidebar active there is no active pane, and the summary should
  // keep describing the pane the user was last in rather than snapping back to
  // the first one.
  const rawActivePane = useUiStore((s) => s.activePane);
  const [stickyPane, setStickyPane] = useState<0 | 1>(0);
  useEffect(() => {
    if (rawActivePane !== null) setStickyPane(rawActivePane);
  }, [rawActivePane]);

  const tab = usePaneStore((s) => {
    const pane = s.panes[stickyPane];
    return pane.tabs.find((tb) => tb.id === pane.activeTabId) ?? pane.tabs[0];
  });
  const showHidden = useUiStore((s) => s.showHidden);
  const total = visibleEntries(tab.listing, tab.filter, showHidden).length;

  // ── Right-hand side: room left on whichever side is active ──
  const activeSide = usePaneStore((s) => s.panes[stickyPane].side);
  const disk = useDiskFree(activeSide === "local" ? tab.path : null);
  const connections = useConnectionStore((s) => s.connections);
  const activeConnId = useConnectionStore((s) => s.activeId);
  const activeConn =
    connections.find((c) => c.id === activeConnId) ?? connections[0] ?? null;
  const quota = useVolumeQuota(activeSide === "remote" ? activeConnId : null);
  const rightText =
    activeSide === "local"
      ? disk
        ? t("statusbar.freeSpace", {
            volume: disk.label,
            free: t("statusbar.free", { size: formatSize(disk.free) }),
          })
        : ""
      : activeConn
        ? quota
          ? `${activeConn.name || activeConn.bucket} · ${activeConn.bucket} · ${t("statusbar.total", { size: formatSize(quota.totalBytes) })}`
          : `${activeConn.name || activeConn.bucket} · ${activeConn.bucket}`
        : "";

  return (
    <div className="grid min-h-[33px] grid-cols-[1fr_auto_1fr] items-center gap-4 border-t border-neutral-900 bg-chrome px-[14px] py-[7px] text-[11.5px] text-neutral-500">
      <span>
        {t("statusbar.selection", { count: tab.selection.length, total })}
      </span>

      <div className="flex items-center justify-center gap-2">
        <TransfersIndicator />
        <ClipboardIndicator />
      </div>

      <span className="truncate text-right">{rightText}</span>
    </div>
  );
}
