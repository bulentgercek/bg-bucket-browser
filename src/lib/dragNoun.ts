import type { Entry } from "../state/paneStore";

/* The word the drag badge puts after its count: what is being dragged, named
   for what it is. */

export type DragNounKey =
  | "dnd.file"
  | "dnd.files"
  | "dnd.folder"
  | "dnd.folders"
  | "dnd.item"
  | "dnd.items";

export function dragNounKey(items: Pick<Entry, "kind">[]): DragNounKey {
  const one = items.length === 1;
  const folders = items.filter((e) => e.kind === "dir").length;
  if (folders === 0) return one ? "dnd.file" : "dnd.files";
  if (folders === items.length) return one ? "dnd.folder" : "dnd.folders";
  return one ? "dnd.item" : "dnd.items";
}
