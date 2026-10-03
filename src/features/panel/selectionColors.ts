import { type PaneSide } from "../../state/paneStore";

/* The selection, the keyboard cursor and the marks of a drag in a pane's list,
   in the colour of the pane's side, like its frame and its folders. The two palettes run the same
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
  /** A folder a drag is over: its wash and its dashed outline. The pane's own
      dashed frame takes the same outline. */
  dropFill: string;
  dropOutline: string;
  /** The edge of the thumbnail that follows the pointer, as a CSS colour. */
  dragChipBorder: string;
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
    dropFill: "bg-[color-mix(in_srgb,var(--color-accent)_14%,transparent)]",
    dropOutline: "outline-accent-400",
    dragChipBorder: "var(--color-accent-600)",
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
    dropFill: "bg-[color-mix(in_srgb,var(--color-local)_14%,transparent)]",
    dropOutline: "outline-local-400",
    dragChipBorder: "var(--color-local-600)",
  },
};
