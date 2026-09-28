import { type PaneSide } from "../../state/paneStore";

/* The selection and the keyboard cursor in a pane's list, in the colour of the
   pane's side, like its frame and its folders. The two palettes run the same
   scale in both themes, so a shade reads the same on either side. The class
   names are written out in full: Tailwind finds them by reading the source. */
export interface SelectionColors {
  /** A selected row's or tile's fill. */
  fill: string;
  /** Text on that fill: a row's name, a tile's label. */
  text: string;
  /** A selected row's icon, size and date. */
  detail: string;
  /** A selected row's outline. */
  rowOutline: string;
  /** The keyboard cursor's line, on anything. */
  cursor: string;
  /** A selected tile's thick outline, without and with the cursor on it. */
  tileOutline: string;
  tileOutlineCursor: string;
  /** A selected tile's picture box: its border, and the tint over a picture. */
  tileBorder: string;
  tileTint: string;
  /** The chip behind a selected tile's label. */
  tileLabel: string;
  /** A selected tile's icon, when it has no picture. */
  tileIcon: string;
}

export const SELECTION: Record<PaneSide, SelectionColors> = {
  remote: {
    fill: "bg-accent-900",
    text: "text-accent-100",
    detail: "text-accent-300",
    rowOutline: "outline-accent-600",
    cursor: "outline-accent-400",
    tileOutline: "outline-accent-500",
    tileOutlineCursor: "outline-accent-300",
    tileBorder: "border-accent-500",
    tileTint: "bg-accent-500",
    tileLabel: "bg-accent-800",
    tileIcon: "text-accent-400",
  },
  local: {
    fill: "bg-local-900",
    text: "text-local-100",
    detail: "text-local-300",
    rowOutline: "outline-local-600",
    cursor: "outline-local-400",
    tileOutline: "outline-local-500",
    tileOutlineCursor: "outline-local-300",
    tileBorder: "border-local-500",
    tileTint: "bg-local-500",
    tileLabel: "bg-local-800",
    tileIcon: "text-local-400",
  },
};
