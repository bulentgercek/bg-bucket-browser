import type { DirEntry } from "./commands";

/* What the active tab actually shows, after its filter and the hidden-files
   setting. `..` is not part of this: the panel draws it separately.

   Filtering happens here rather than in a new listing, so typing narrows the
   list instantly. */
export function visibleEntries(
  listing: DirEntry[],
  filter: string,
  showHidden: boolean,
): DirEntry[] {
  const q = filter.trim().toLowerCase();
  return listing.filter((e) => {
    if (!showHidden && e.name.startsWith(".")) return false;
    if (q && !e.name.toLowerCase().includes(q)) return false;
    return true;
  });
}

/* ── Sorting ───────────────────────────────────────────────────────── */

export type SortKey = "name" | "size" | "modified";
export type SortDir = "asc" | "desc";
export interface SortState {
  key: SortKey;
  dir: SortDir;
}
/** The order a fresh listing already arrives in: by name, directories first. */
export const DEFAULT_SORT: SortState = { key: "name", dir: "asc" };

/** Sorts entries for display. Directories always come first, whichever
    column and direction are chosen, and the key applies inside each group.
    Ties break by name, so the order never jitters between renders. */
export function sortEntries(entries: DirEntry[], sort: SortState): DirEntry[] {
  const sign = sort.dir === "asc" ? 1 : -1;
  const byName = (a: DirEntry, b: DirEntry) => {
    const al = a.name.toLowerCase();
    const bl = b.name.toLowerCase();
    return al < bl ? -1 : al > bl ? 1 : 0;
  };
  return [...entries].sort((a, b) => {
    const dr = (a.kind === "dir" ? 0 : 1) - (b.kind === "dir" ? 0 : 1);
    if (dr !== 0) return dr; // directories before files, always

    let cmp: number;
    if (sort.key === "size") cmp = (a.size ?? 0) - (b.size ?? 0);
    else if (sort.key === "modified") cmp = (a.modified ?? 0) - (b.modified ?? 0);
    else cmp = byName(a, b);

    return cmp !== 0 ? cmp * sign : byName(a, b);
  });
}
