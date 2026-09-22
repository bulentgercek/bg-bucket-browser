import { create } from "zustand";
import {
  listConnections,
  activeConnectionId,
  setActiveConnection,
  type ConnectionInfo,
} from "../lib/commands";

/* The frontend's copy of the connection list, refreshed after a save or a
   switch.

   Secrets are never part of it: the backend only says whether a key is stored.
   One connection is live at a time, and its bucket name is what an S3 path is
   built from. */

interface ConnState {
  connections: ConnectionInfo[];
  activeId: string | null;
  loaded: boolean;
  /** The live connection's bucket, used wherever an S3 path is shown. */
  bucket: string;
  load: () => Promise<void>;
  /** Switches the live connection; running transfers keep their own. */
  setActive: (id: string) => Promise<void>;
}

function bucketOf(conns: ConnectionInfo[], id: string | null): string {
  return conns.find((c) => c.id === id)?.bucket ?? conns[0]?.bucket ?? "";
}

export const useConnectionStore = create<ConnState>((set, get) => ({
  connections: [],
  activeId: null,
  loaded: false,
  bucket: "",
  load: async () => {
    try {
      const [connections, activeId] = await Promise.all([
        listConnections(),
        activeConnectionId(),
      ]);
      set({
        connections,
        activeId,
        loaded: true,
        bucket: bucketOf(connections, activeId),
      });
    } catch {
      set({ loaded: true });
    }
  },
  setActive: async (id) => {
    await setActiveConnection(id);
    const conns = get().connections;
    set({ activeId: id, bucket: bucketOf(conns, id) });
  },
}));
