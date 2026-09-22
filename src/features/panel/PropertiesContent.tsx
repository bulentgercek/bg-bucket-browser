import { useEffect, useState } from "react";
import { type Entry } from "../../state/paneStore";
import { folderSize } from "../../lib/commands";
import { formatDate, formatSize } from "../../lib/format";
import { t } from "../../locale/en";

/* What the Properties window shows. A folder's size has to be scanned, so the
   window opens immediately and fills that line in when it can. */
export function PropertiesContent({
  entry,
  full,
  remote,
  bucket,
}: {
  entry: Entry;
  full: string;
  remote: boolean;
  bucket: string;
}) {
  const isDir = entry.kind === "dir";
  const [scan, setScan] = useState<
    { bytes: number; files: number } | "loading" | "error"
  >(isDir ? "loading" : "error");

  useEffect(() => {
    if (!isDir) return;
    let alive = true;
    folderSize(remote, full)
      .then((r) => alive && setScan(r))
      .catch(() => alive && setScan("error"));
    return () => {
      alive = false;
    };
  }, [isDir, remote, full]);

  const sizeText = !isDir
    ? formatSize(entry.size)
    : scan === "loading"
      ? t("props.calculating")
      : scan === "error"
        ? "—"
        : t("props.folderSize", {
            size: formatSize(scan.bytes),
            count: scan.files,
          });

  const rows: [string, string][] = [
    [t("props.name"), entry.name],
    [t("props.kind"), isDir ? t("props.kindFolder") : t("props.kindFile")],
    [t("props.size"), sizeText],
    [
      t("props.modified"),
      entry.modified != null ? formatDate(entry.modified) : "—",
    ],
    [t("props.path"), full],
  ];
  if (remote) {
    rows.push([
      t("props.s3uri"),
      `s3://${bucket || "bucket"}/${full}${isDir ? "/" : ""}`,
    ]);
  }

  return (
    <dl className="grid grid-cols-[auto_1fr] gap-x-4 gap-y-1.5">
      {rows.map(([k, v]) => (
        <div key={k} className="contents">
          <dt className="text-neutral-500">{k}</dt>
          <dd className="min-w-0 break-all text-neutral-200">{v}</dd>
        </div>
      ))}
    </dl>
  );
}
