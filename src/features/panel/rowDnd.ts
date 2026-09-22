import { type DragEvent as ReactDragEvent } from "react";
import { type Entry } from "../../state/paneStore";

/* The drag handlers a row needs, set up once by the pane and handed to every
   row of either view. */
export interface RowDnd {
  dropTarget: string | null;
  onDragStart: (e: ReactDragEvent, entry: Entry) => void;
  onDragEnd: () => void;
  onDirDragOver: (e: ReactDragEvent, entry: Entry) => void;
  onDirDrop: (e: ReactDragEvent, entry: Entry) => void;
}
