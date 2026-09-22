import type { DirEntry } from "./commands";

/* What a keyboard action works on in a pane: the selection, or, when nothing
   is selected, the entry under the keyboard cursor.

   Each pane registers how to find them, because only the pane knows which
   rows it is showing and whether its cursor is drawn at all. Copy, cut, F5
   and F6 live in the main screen, Delete and F2 in the pane; all of them ask
   here. */

type Finder = () => DirEntry[];

const finders: [Finder | null, Finder | null] = [null, null];

/** Called by a pane with its finder, and with `null` when it goes away. */
export function registerKeyboardTargets(index: 0 | 1, finder: Finder | null): void {
  finders[index] = finder;
}

/** The entries a keyboard action in pane `index` should act on; possibly none. */
export function keyboardTargets(index: 0 | 1): DirEntry[] {
  return finders[index]?.() ?? [];
}
