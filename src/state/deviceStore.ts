import { create } from "zustand";
import { listDevices, localHomeDir, type Device } from "../lib/commands";

/* The mounted local disks the sidebar lists, and the real path of the home
   directory.

   The home path is only used for display: internally a path stays `~`, but a
   breadcrumb showing a tilde would read as foreign on Windows. Loaded once at
   startup and never persisted — mounts change between runs. */

interface DeviceState {
  devices: Device[];
  homeDir: string;
  loaded: boolean;
  load: () => Promise<void>;
}

export const useDeviceStore = create<DeviceState>((set) => ({
  devices: [],
  homeDir: "",
  loaded: false,
  load: async () => {
    try {
      const [devices, homeDir] = await Promise.all([
        listDevices(),
        localHomeDir().catch(() => ""),
      ]);
      set({ devices, homeDir, loaded: true });
    } catch {
      set({ loaded: true });
    }
  },
}));
