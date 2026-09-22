import { type ReactNode } from "react";
import { type TabState } from "../../state/paneStore";
import { type ListErrKind } from "../../lib/commands";
import { t, type StringKey } from "../../locale/en";

/* What a pane shows instead of entries: loading, a failure, an empty folder. */

// Every failure the backend can report has a message here; the compiler
// checks that none is missing.
const ERROR_KEY: Record<ListErrKind, StringKey> = {
  notFound: "panel.error.notFound",
  accessDenied: "panel.error.accessDenied",
  unreachable: "panel.error.unreachable",
  credentials: "panel.error.credentials",
  io: "panel.error.io",
  unknown: "panel.error.unknown",
};

export function StateRow({
  text,
  tone,
  actions,
}: {
  text: string;
  tone?: "error";
  /** What can be done from here, such as retrying or opening Settings. */
  actions?: { label: string; onClick: () => void }[];
}) {
  return (
    <div
      className={
        "flex flex-col items-start gap-2 px-2 py-3 text-[12.5px] " +
        (tone === "error" ? "text-neutral-300" : "text-neutral-600")
      }
    >
      <span>{text}</span>
      {actions && actions.length > 0 && (
        <div className="flex gap-2">
          {actions.map((a) => (
            <button
              key={a.label}
              type="button"
              onClick={a.onClick}
              className="rounded-sm border border-neutral-800 px-2 py-0.5 text-[11px] text-neutral-400 hover:border-accent-700 hover:text-accent-300"
            >
              {a.label}
            </button>
          ))}
        </div>
      )}
    </div>
  );
}

export type StateOpts = {
  noConnection: boolean;
  onRetry: () => void;
  onOpenSettings: () => void;
};

/* What stands in for the list when there is nothing to draw, shared by both
   views. */
export function bodyStateRow(
  tab: TabState,
  filterActive: boolean,
  isEmpty: boolean,
  opts: StateOpts,
): ReactNode {
  if (opts.noConnection && tab.status !== "ready") {
    return (
      <StateRow
        text={t("panel.noConnection")}
        actions={[
          { label: t("panel.openSettings"), onClick: opts.onOpenSettings },
        ]}
      />
    );
  }
  if (tab.status === "loading") return <StateRow text={t("panel.loading")} />;
  if (tab.status === "error") {
    return (
      <StateRow
        text={t(ERROR_KEY[tab.error ?? "unknown"])}
        tone="error"
        actions={[{ label: t("panel.retry"), onClick: opts.onRetry }]}
      />
    );
  }
  if (tab.status === "ready" && isEmpty) {
    return (
      <StateRow text={t(filterActive ? "panel.noMatches" : "panel.empty")} />
    );
  }
  return null;
}
