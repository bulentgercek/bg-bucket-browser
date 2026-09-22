import { create } from "zustand";
import { localHomeDir, readOsClipboardFiles, writeOsClipboardFiles } from "../lib/commands";
import { childPath, type Entry, type PaneSide } from "./paneStore";

/* What Copy and Cut put down, from inside the app and from the rest of the
   desktop.

   A clipboard has one slot: whichever was filled last is the one that pastes.
   The two sources are separate fields here, so each one clears the other when
   it wins — a copy made in the app drops the OS row, and a genuinely new copy
   made outside drops the in-app one.

   The OS clipboard is polled rather than watched, because a copy made while the
   window is in the background has to show up too. Telling a new copy from the
   same content sitting there is the whole difficulty, and the three signatures
   below are the answer.

   Nothing here is kept between runs. */

export interface Clipboard {
  items: Entry[];
  srcSide: PaneSide;
  /** The source directory, relative to that pane's root. */
  srcPath: string;
  mode: "copy" | "move";
  origin: "inApp";
}

interface ClipboardState {
  clipboard: Clipboard | null;
  /** What the OS clipboard is holding, when it holds files. */
  osClipboardTag: { kind: "files"; paths: string[]; mode: "copy" | "move" } | null;
  /** What the user dismissed with the row's close button. Closing the row does
      not clear the real clipboard, so without this the next poll would bring it
      straight back. A different copy has a different signature and shows. */
  dismissedOsSignature: string | null;
  /** What the last poll saw. Only something different counts as a new copy;
      content that has simply been sitting there must not keep overwriting an
      in-app copy made after it. */
  seenOsSignature: string | null;
  /** What this app itself wrote to the OS clipboard. Reading it back is our
      own echo, not a copy from outside. */
  ownOsSignature: string | null;
  setClipboard: (c: Clipboard) => void;
  clearClipboard: () => void;
  /** Polls the OS clipboard; called on a timer, whether focused or not. */
  refreshOsClipboard: () => Promise<void>;
  /** Hides the OS row without touching the real clipboard. */
  clearOsClipboardTag: () => void;
}

/** The home directory, asked once: signatures compare absolute paths, and this
    side of the app writes `~`. */
let homePromise: Promise<string> | null = null;
function homeDir(): Promise<string> {
  homePromise ??= localHomeDir().catch(() => "");
  return homePromise;
}

function expandHome(path: string, home: string): string {
  if (!home) return path;
  if (path === "~") return home;
  return path.startsWith("~/") ? `${home}${path.slice(1)}` : path;
}

/** A comparable signature for a list of paths. Separators and trailing slashes
    are normalised, because the same path can come back written differently than
    it went out. */
function pathsSignature(paths: string[]): string {
  return paths
    .map((p) => p.replace(/\\/g, "/").replace(/(.)\/+$/, "$1"))
    .join("\n");
}

export const useClipboardStore = create<ClipboardState>((set, get) => ({
  clipboard: null,
  osClipboardTag: null,
  dismissedOsSignature: null,
  seenOsSignature: null,
  ownOsSignature: null,
  // A copy made in the app takes the slot; the OS row goes.
  setClipboard: (c) => {
    set({ clipboard: c, osClipboardTag: null });
    // Only a local copy goes out to the desktop, so it can be pasted in a file
    // manager. A remote object has no local path to offer.
    if (c.srcSide === "local") {
      const isMove = c.mode === "move";
      const paths = c.items.map((e) => childPath(c.srcPath, e.name));
      // The echo signature is recorded before the write, so a poll landing in
      // between still recognises the content as ours.
      void homeDir()
        .then((home) => {
          set({ ownOsSignature: pathsSignature(paths.map((p) => expandHome(p, home))) });
          return writeOsClipboardFiles(paths, isMove);
        })
        .catch(() => {});
    }
  },
  clearClipboard: () => set({ clipboard: null }),
  refreshOsClipboard: async () => {
    try {
      const { paths, cut } = await readOsClipboardFiles();
      const sig = paths.join("\n");
      const { dismissedOsSignature: dismissed, seenOsSignature: seen, ownOsSignature: own } =
        get();
      // Our own echo?
      const isOwn = paths.length > 0 && pathsSignature(paths) === own;
      // Genuinely new content is compared against what was last seen, not
      // against the row being shown: the row is cleared by every in-app copy,
      // which used to make stale content look new again.
      const isNewOsCopy = paths.length > 0 && sig !== dismissed && sig !== seen && !isOwn;
      set((s) => {
        // A new copy from outside takes the slot in turn.
        const clipboard = isNewOsCopy ? null : s.clipboard;
        return {
          seenOsSignature: sig,
          ownOsSignature: isNewOsCopy ? null : s.ownOsSignature,
          clipboard,
          // The row hides while the in-app clipboard holds the slot, and for
          // our own echo: after a move, the files it names are no longer there.
          osClipboardTag:
            paths.length > 0 && sig !== dismissed && !clipboard && !isOwn
              ? { kind: "files", paths, mode: cut ? "move" : "copy" }
              : null,
        };
      });
    } catch {
      set({ osClipboardTag: null });
    }
  },
  clearOsClipboardTag: () =>
    set((s) => ({
      osClipboardTag: null,
      dismissedOsSignature: s.osClipboardTag
        ? s.osClipboardTag.paths.join("\n")
        : s.dismissedOsSignature,
    })),
}));
